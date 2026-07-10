import { ELECTRODE_SCHEMA_VERSION } from '@electrode/flatbuffers';
import { DEFAULT_VEHICLE_ID, resolveTopicDefinition, topicKeyFromTopic, vehicleIdFromTopic } from './topics';
import type {
  Attitude,
  Battery,
  ControlInputs,
  EventFrame,
  EventMessage,
  GcsFrame,
  LinkStatus,
  LocalizationState,
  ManualControlState,
  MissionPlanState,
  ModeState,
  Pose,
  TelemetryFrame,
  TopicSnapshot,
  VehicleState,
  Velocity
} from './types';

const RATE_WINDOW_MS = 2000;
const MAX_EVENTS = 80;
const MIN_MOCAP_VELOCITY_DT_MS = 20;
const MOCAP_VELOCITY_SMOOTHING_MS = 300;

export function createInitialVehicleState(vehicleId = DEFAULT_VEHICLE_ID): VehicleState {
  return {
    vehicleId,
    connected: false,
    lastUpdateMs: 0,
    pose: null,
    velocity: null,
    attitude: null,
    attitudeEstimate: null,
    controls: null,
    manualControl: null,
    radioControl: null,
    motors: null,
    battery: null,
    link: null,
    mode: {
      name: 'standby',
      armed: false,
      failsafe: false
    },
    localization: {
      source: 'none',
      fresh: false,
      quality: 0,
      updatedAtMs: 0
    },
    mission: null,
    events: [],
    topics: {},
    lastMocap: null
  };
}

export function applyGcsFrame(state: VehicleState, frame: GcsFrame, nowMs = Date.now()): VehicleState {
  if (frame.kind === 'event') {
    return applyEventFrame(state, frame, nowMs);
  }

  return applyTelemetryFrame(state, frame, nowMs);
}

export function applyTelemetryFrame(state: VehicleState, frame: TelemetryFrame, nowMs = Date.now()): VehicleState {
  if (frame.header.schemaVersion !== ELECTRODE_SCHEMA_VERSION) {
    return appendEvent(state, {
      severity: 'warning',
      code: 'schema_mismatch',
      message: `Schema ${frame.header.schemaVersion} is not supported`,
      timestampMs: nowMs
    });
  }

  if (frame.header.expireTimeNs > 0 && frame.header.expireTimeNs < nowMs * 1_000_000) {
    return state;
  }

  const vehicleId = vehicleIdFromTopic(frame.topic) ?? frame.header.vehicleId;
  if (state.vehicleId !== vehicleId) {
    state.vehicleId = vehicleId;
  }

  const snapshot = updateTopicSnapshot(state, frame, nowMs);
  const key = topicKeyFromTopic(frame.topic);

  if (key === 'pose') {
    state.pose = frame.payload as Pose;
  } else if (key === 'velocity') {
    state.velocity = frame.payload as Velocity;
  } else if (key === 'attitude') {
    state.attitude = frame.payload as Attitude;
  } else if (key === 'battery') {
    state.battery = frame.payload as Battery;
  } else if (key === 'link') {
    state.link = frame.payload as LinkStatus;
  } else if (key === 'mode') {
    state.mode = frame.payload as ModeState;
  } else if (key === 'localization') {
    state.localization = frame.payload as LocalizationState;
  } else {
    // Raw Synapse wire topics (`synapse/**`) — the same stream real hardware
    // emits. The adapter derives the canonical vehicle state from them so the
    // simulator and hardware share one path.
    applySynapseFrame(state, frame, nowMs);
  }

  state.topics[frame.topic] = snapshot;
  state.lastUpdateMs = nowMs;
  return refreshStaleTopics(state, nowMs);
}

export function applyEventFrame(state: VehicleState, frame: EventFrame, nowMs = Date.now()): VehicleState {
  updateTopicSnapshot(state, frame, nowMs);
  appendEvent(state, frame.payload);
  state.lastUpdateMs = nowMs;
  return refreshStaleTopics(state, nowMs);
}

function toFiniteNumber(value: unknown): number | null {
  const n = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(n) ? n : null;
}

/**
 * Extract normalized control inputs from a `synapse/v1/topic/manual_control_command`
 * payload. Accepts the wrapped `{ data: { axes: { roll, pitch, yaw, throttle } } }`
 * shape published by the simulator/bridge, as well as a flat `{ axes: {...} }`.
 */
function parseControlInputs(payload: unknown): ControlInputs | null {
  const record = payload as Record<string, unknown> | null | undefined;
  const data = (record?.data ?? record) as Record<string, unknown> | undefined;
  const axes = data?.axes as Record<string, unknown> | undefined;
  if (!axes) {
    return null;
  }
  return {
    aileron: toFiniteNumber(axes.roll) ?? 0,
    elevator: toFiniteNumber(axes.pitch) ?? 0,
    rudder: toFiniteNumber(axes.yaw) ?? 0,
    throttle: toFiniteNumber(axes.throttle) ?? 0
  };
}

/**
 * Extract the full manual-control frame (stick axes + arm/kill/mode switches)
 * from a `synapse/v1/topic/manual_control_command` payload. Accepts the wrapped
 * `{ data: { axes: {...}, arm_switch, ... } }` shape published by the
 * simulator/bridge as well as a flat `{ axes: {...}, ... }`.
 */
function parseManualControl(payload: unknown, nowMs: number): ManualControlState | null {
  const record = payload as Record<string, unknown> | null | undefined;
  const data = (record?.data ?? record) as Record<string, unknown> | undefined;
  const axes = data?.axes as Record<string, unknown> | undefined;
  if (!axes) {
    return null;
  }
  return {
    roll: toFiniteNumber(axes.roll) ?? 0,
    pitch: toFiniteNumber(axes.pitch) ?? 0,
    yaw: toFiniteNumber(axes.yaw) ?? 0,
    throttle: toFiniteNumber(axes.throttle) ?? 0,
    flightMode: toFiniteNumber(data?.flight_mode) ?? 0,
    armSwitch: Boolean(data?.arm_switch),
    killSwitch: Boolean(data?.kill_switch),
    active: data?.active === undefined ? true : Boolean(data.active),
    valid: data?.valid === undefined ? true : Boolean(data.valid),
    updatedAtMs: nowMs
  };
}

/**
 * Extract per-motor commands from a `synapse/v1/topic/pwm_signal_outputs`
 * payload. The `motors` object is keyed by motor id (`m0`, `m1`, ...); we
 * sort by key so the array order is stable across sources.
 */
function parseMotorOutputs(payload: unknown): number[] | null {
  const record = payload as Record<string, unknown> | null | undefined;
  const motors = record?.motors;
  if (!motors || typeof motors !== 'object') {
    return null;
  }
  const entries = Object.entries(motors as Record<string, unknown>)
    .map(([key, value]) => [key, toFiniteNumber(value)] as const)
    .filter((entry): entry is readonly [string, number] => entry[1] !== null)
    .sort((a, b) => a[0].localeCompare(b[0], undefined, { numeric: true }));
  return entries.length > 0 ? entries.map((entry) => entry[1]) : null;
}

function parseRadioControl(payload: unknown): number[] | null {
  const record = payload as Record<string, unknown> | null | undefined;
  const channels = (record?.channels ?? record) as Record<string, unknown> | undefined;
  if (!channels || typeof channels !== 'object') {
    return null;
  }
  const entries = Object.entries(channels)
    .map(([key, value]) => [key, toFiniteNumber(value)] as const)
    .filter((entry): entry is readonly [string, number] => entry[1] !== null)
    .sort((a, b) => a[0].localeCompare(b[0], undefined, { numeric: true }));
  return entries.length > 0 ? entries.map((entry) => entry[1]) : null;
}

function parseOpticalFlowVelocity(payload: unknown): Velocity | null {
  const record = payload as Record<string, unknown> | null | undefined;
  const data = (record?.data ?? record) as Record<string, unknown> | undefined;
  const velNe = data?.vel_ne as Record<string, unknown> | undefined;
  if (!velNe) {
    return null;
  }
  const northMps = toFiniteNumber(velNe.x);
  const eastMps = toFiniteNumber(velNe.y);
  if (northMps === null || eastMps === null) {
    return null;
  }
  return {
    northMps,
    eastMps,
    downMps: 0,
    groundSpeedMps: Math.hypot(northMps, eastMps)
  };
}

function frameSampleTimeMs(frame: TelemetryFrame, fallbackMs: number): number {
  const payload = frame.payload as Record<string, unknown> | null | undefined;
  const payloadTimestampUs = toFiniteNumber(payload?.timestamp_us);
  if (payloadTimestampUs !== null && payloadTimestampUs > 0) {
    return payloadTimestampUs / 1000;
  }
  const sourceMs = frame.header.sourceTimeNs > 0 ? frame.header.sourceTimeNs / 1_000_000 : 0;
  if (Number.isFinite(sourceMs) && sourceMs > 0) {
    return sourceMs;
  }
  const receiveMs = frame.header.receiveTimeNs > 0 ? frame.header.receiveTimeNs / 1_000_000 : 0;
  if (Number.isFinite(receiveMs) && receiveMs > 0) {
    return receiveMs;
  }
  return fallbackMs;
}

/**
 * cubs2 `vehicle_health.flight_mode` values. The schema leaves flight_mode
 * producer-defined; cubs2 publishes `auto_mode ? 1 : 0` (auto is selected by
 * the RC mode switch, or forced when manual input is invalid).
 */
const CUBS2_FLIGHT_MODES: Record<number, string> = { 0: 'manual', 1: 'auto' };
const SELECTED_MOCAP_TOPIC_FRAGMENT = 'synapse/mocap/selected/rigid_body/';
const CUB1_EXTERNAL_ODOMETRY_SUFFIX = '/external_odometry/1';
const mocapRigidBodyNames = new Map<number, string>();

/**
 * Adapter: map raw Synapse wire topics onto the canonical vehicle state. Most
 * topics live under `synapse/v1/**`; `synapse/motor_output` is the selected
 * post-arbitration PWM command stream that mirrors what the PPM radio receives.
 */
function applySynapseFrame(state: VehicleState, frame: TelemetryFrame, nowMs: number): void {
  const topic = frame.topic;
  if (topic.endsWith('rigid_body_names')) {
    updateMocapRigidBodyNames(frame.payload);
  } else if (topic.includes('/external_odometry/') && isCub1ExternalOdometry(frame.payload, topic)) {
    applyExternalOdometry(state, frame.payload, nowMs, frameSampleTimeMs(frame, nowMs));
  } else if (
    topic.endsWith('mocap_frame') ||
    topic.endsWith('mocap/frame') ||
    topic.includes('synapse/mocap/rigid_body/') ||
    topic.includes(SELECTED_MOCAP_TOPIC_FRAGMENT)
  ) {
    if (!shouldIgnoreUnselectedMocap(state, topic)) {
      applyMocapFrame(state, frame.payload, nowMs, frameSampleTimeMs(frame, nowMs), topic);
    }
  } else if (topic.endsWith('optical_flow_velocity')) {
    const velocity = parseOpticalFlowVelocity(frame.payload);
    if (velocity) {
      state.velocity = velocity;
    }
  } else if (topic.endsWith('attitude_estimate')) {
    applyAttitudeEstimate(state, frame.payload);
  } else if (topic.endsWith('vehicle_health')) {
    applyVehicleHealth(state, frame.payload);
  } else if (topic.endsWith('manual_control_command')) {
    const controls = parseControlInputs(frame.payload);
    if (controls) {
      state.controls = controls;
    }
    const manualControl = parseManualControl(frame.payload, nowMs);
    if (manualControl) {
      state.manualControl = manualControl;
    }
  } else if (topic.endsWith('radio_control')) {
    const radioControl = parseRadioControl(frame.payload);
    if (radioControl) {
      state.radioControl = radioControl;
    }
  } else if (topic.endsWith('motor_output')) {
    const motors = parseMotorOutputs(frame.payload);
    if (motors) {
      state.motors = motors;
    }
  } else if (topic.endsWith('pwm_signal_outputs')) {
    const hasSelectedOutput = Object.values(state.topics).some(
      (snapshot) => snapshot.topic.endsWith('motor_output') && !snapshot.stale
    );
    if (!hasSelectedOutput) {
      const motors = parseMotorOutputs(frame.payload);
      if (motors) {
        state.motors = motors;
      }
    }
  } else if (topic.endsWith('power_status')) {
    applyPowerStatus(state, frame.payload);
  } else if (topic.endsWith('mission_progress')) {
    applyMissionProgress(state, frame.payload, nowMs);
  } else if (topic.endsWith('local_position_command')) {
    applyLocalPositionCommand(state, frame.payload, nowMs);
  } else if (topic.endsWith('trajectory_segment')) {
    applyTrajectorySegment(state, frame.payload, nowMs);
  }
}

function shouldIgnoreUnselectedMocap(state: VehicleState, topic: string): boolean {
  if (topic.includes(SELECTED_MOCAP_TOPIC_FRAGMENT)) {
    return false;
  }
  return Object.values(state.topics).some(
    (snapshot) =>
      snapshot.topic.includes(SELECTED_MOCAP_TOPIC_FRAGMENT) &&
      !snapshot.stale
  );
}

function updateMocapRigidBodyNames(payload: unknown): void {
  const bodies = (payload as { rigidBodies?: unknown[] } | null)?.rigidBodies;
  if (!Array.isArray(bodies)) return;
  for (const body of bodies) {
    const item = body as { id?: unknown; name?: unknown };
    const id = toFiniteNumber(item.id);
    if (id !== null && typeof item.name === 'string' && item.name.trim()) {
      mocapRigidBodyNames.set(id, item.name.trim().toLowerCase());
    }
  }
}

function isCub1ExternalOdometry(payload: unknown, topic: string): boolean {
  const data = ((payload as { data?: unknown } | null)?.data ?? payload) as { id?: unknown } | null;
  const id = toFiniteNumber(data?.id);
  return (id !== null && mocapRigidBodyNames.get(id) === 'cub1') ||
    (mocapRigidBodyNames.size === 0 && topic.endsWith(CUB1_EXTERNAL_ODOMETRY_SUFFIX));
}

function applyExternalOdometry(
  state: VehicleState,
  payload: unknown,
  nowMs: number,
  sampleMs: number
): void {
  const record = payload as Record<string, unknown> | null | undefined;
  const data = (record?.data ?? record) as Record<string, unknown> | undefined;
  const position = data?.position as Record<string, unknown> | undefined;
  const attitudeValue = data?.attitude as Record<string, unknown> | undefined;
  const positionValid = data?.position_valid === true;
  const attitudeValid = data?.attitude_valid === true;
  const xM = toFiniteNumber(position?.x);
  const yM = toFiniteNumber(position?.y);
  const altM = toFiniteNumber(position?.z);

  if (!positionValid || xM === null || yM === null || altM === null) {
    state.localization = {
      source: 'external odometry',
      fresh: false,
      quality: 0,
      updatedAtMs: nowMs
    };
    return;
  }

  state.pose = { lat: 0, lon: 0, altM, xM, yM, zM: -altM };
  state.lastMocap = { tMs: sampleMs, xM, yM, altM };

  if (attitudeValid) {
    const attitude = quaternionToEuler(attitudeValue);
    if (attitude) {
      state.attitude = attitude;
    }
  }

  const velocity = data.linear_velocity as Record<string, unknown> | undefined;
  const eastMps = toFiniteNumber(velocity?.x);
  const northMps = toFiniteNumber(velocity?.y);
  const upMps = toFiniteNumber(velocity?.z);
  if (
    data.linear_velocity_valid === true &&
    eastMps !== null &&
    northMps !== null &&
    upMps !== null
  ) {
    state.velocity = {
      northMps,
      eastMps,
      downMps: -upMps,
      groundSpeedMps: Math.hypot(northMps, eastMps)
    };
  }

  const lost = data.lost === true;
  const degraded = data.degraded === true || data.outlier_rejected === true;
  const extrapolated = data.extrapolated === true;
  state.localization = {
    source: 'external odometry',
    fresh: !lost,
    quality: lost ? 0 : degraded ? 0.35 : extrapolated ? 0.7 : 1,
    updatedAtMs: nowMs
  };
}
const MISSION_STATES = new Set(['unknown', 'idle', 'active', 'paused', 'complete']);

function ensureMission(state: VehicleState, nowMs: number): MissionPlanState {
  if (!state.mission) {
    state.mission = {
      missionId: 0,
      currentSeq: 0,
      total: 0,
      state: 'unknown',
      waypoints: [],
      target: null,
      updatedAtMs: nowMs
    };
  }
  return state.mission;
}

/** Reset the waypoint table when the plan identity or size changes. */
function resyncMissionPlan(mission: MissionPlanState, missionId: number, total: number): void {
  if (mission.missionId !== missionId || mission.waypoints.length !== total) {
    mission.missionId = missionId;
    mission.total = total;
    mission.waypoints = Array.from({ length: total }, () => null);
  }
}

/** Track active item and plan identity from `synapse/v1/topic/mission_progress`. */
function applyMissionProgress(state: VehicleState, payload: unknown, nowMs: number): void {
  const record = payload as Record<string, unknown> | null | undefined;
  const data = (record?.data ?? record) as Record<string, unknown> | undefined;
  const currentSeq = toFiniteNumber(data?.current_seq);
  const total = toFiniteNumber(data?.total);
  if (currentSeq === null || total === null) {
    return;
  }
  const mission = ensureMission(state, nowMs);
  resyncMissionPlan(mission, toFiniteNumber(data?.mission_id) ?? 0, total);
  mission.currentSeq = currentSeq;
  mission.total = total;
  const missionState = typeof data?.mission_state === 'string' ? data.mission_state : 'unknown';
  mission.state = (MISSION_STATES.has(missionState) ? missionState : 'unknown') as MissionPlanState['state'];
  mission.updatedAtMs = nowMs;
}

/** Track the active navigation target from `synapse/v1/topic/local_position_command`. */
function applyLocalPositionCommand(state: VehicleState, payload: unknown, nowMs: number): void {
  const record = payload as Record<string, unknown> | null | undefined;
  const data = (record?.data ?? record) as Record<string, unknown> | undefined;
  const position = data?.position_enu_m as Record<string, unknown> | undefined;
  if (!position) {
    return;
  }
  const east = toFiniteNumber(position.x);
  const north = toFiniteNumber(position.y);
  const up = toFiniteNumber(position.z);
  if (east === null || north === null || up === null) {
    return;
  }
  const mission = ensureMission(state, nowMs);
  mission.target = { east, north, up, yawRad: toFiniteNumber(data?.yaw_rad) ?? 0 };
  mission.updatedAtMs = nowMs;
}

/** Rebuild the waypoint table from `synapse/v1/topic/trajectory_segment`. */
function applyTrajectorySegment(state: VehicleState, payload: unknown, nowMs: number): void {
  const record = payload as Record<string, unknown> | null | undefined;
  const data = (record?.data ?? record) as Record<string, unknown> | undefined;
  const points = data?.points;
  const firstPoint = Array.isArray(points) ? (points[0] as Record<string, unknown> | undefined) : undefined;
  if (!firstPoint) {
    return;
  }
  const seq = toFiniteNumber(data?.segment_seq);
  const east = toFiniteNumber(firstPoint.x);
  const north = toFiniteNumber(firstPoint.y);
  const up = toFiniteNumber(firstPoint.z);
  if (seq === null || east === null || north === null || up === null) {
    return;
  }
  const mission = ensureMission(state, nowMs);
  const total = Math.max(mission.total, seq + 1);
  if (total <= 0 || seq < 0 || seq >= total) {
    return;
  }
  resyncMissionPlan(mission, toFiniteNumber(data?.trajectory_id) ?? mission.missionId, total);
  mission.waypoints[seq] = { seq, east, north, up };
  mission.updatedAtMs = nowMs;
}

/**
 * Derive pose, attitude, velocity, and localization from a decoded
 * a mocap pose frame (rigid body 0). Position is ENU metres — x=east,
 * y=north, z=up — mapped to Pose as xM/yM/altM; attitude is taken from the
 * rigid-body quaternion (mocap is the attitude source of truth).
 */
function applyMocapFrame(
  state: VehicleState,
  payload: unknown,
  nowMs: number,
  sampleMs: number,
  topic: string
): void {
  const record = payload as Record<string, unknown> | null | undefined;
  const bodies = record?.rigid_bodies;
  const body = Array.isArray(bodies)
    ? bodies.map((item) => item as Record<string, unknown>).find((item) => {
        const id = toFiniteNumber(item.id);
        return id !== null && (mocapRigidBodyNames.get(id) === 'cub1' || (mocapRigidBodyNames.size === 0 && id === 1));
      }) ?? (bodies[0] as Record<string, unknown> | undefined)
    : undefined;
  const position = body?.position as Record<string, unknown> | undefined;
  if (!body || !position || !isValidMocapBody(body)) {
    markMocapStale(state, nowMs);
    return;
  }
  const xM = toFiniteNumber(position.x)!;
  const yM = toFiniteNumber(position.y)!;
  const altM = toFiniteNumber(position.z)!;

  // Ground-truth velocity by finite-differencing successive mocap positions.
  const prev = state.lastMocap;
  const hasDirectVelocity = Object.values(state.topics).some(
    (snapshot) => snapshot.topic.endsWith('optical_flow_velocity') && !snapshot.stale
  );
  const hasFullMocapFrame = Object.values(state.topics).some(
    (snapshot) => snapshot.topic.endsWith('mocap/frame') && !snapshot.stale
  );
  const compactPoseMirrorsFullFrame =
    (topic.includes('synapse/mocap/rigid_body/') ||
      topic.includes(SELECTED_MOCAP_TOPIC_FRAGMENT)) &&
    hasFullMocapFrame;
  const useForMocapVelocity = !compactPoseMirrorsFullFrame;
  const dtMs = prev ? sampleMs - prev.tMs : 0;
  if (prev && dtMs >= MIN_MOCAP_VELOCITY_DT_MS && !hasDirectVelocity && useForMocapVelocity) {
    const dt = dtMs / 1000;
    const eastMps = (xM - prev.xM) / dt;
    const northMps = (yM - prev.yM) / dt;
    const upMps = (altM - prev.altM) / dt;
    const alpha = 1 - Math.exp(-dtMs / MOCAP_VELOCITY_SMOOTHING_MS);
    const previousVelocity = state.velocity;
    const smoothNorthMps = previousVelocity
      ? previousVelocity.northMps + (northMps - previousVelocity.northMps) * alpha
      : northMps;
    const smoothEastMps = previousVelocity
      ? previousVelocity.eastMps + (eastMps - previousVelocity.eastMps) * alpha
      : eastMps;
    const smoothDownMps = previousVelocity
      ? previousVelocity.downMps + (-upMps - previousVelocity.downMps) * alpha
      : -upMps;
    state.velocity = {
      northMps: smoothNorthMps,
      eastMps: smoothEastMps,
      downMps: smoothDownMps,
      groundSpeedMps: Math.hypot(smoothNorthMps, smoothEastMps)
    };
  }
  if (useForMocapVelocity && (!prev || dtMs >= MIN_MOCAP_VELOCITY_DT_MS || hasDirectVelocity)) {
    state.lastMocap = { tMs: sampleMs, xM, yM, altM };
  }

  // Indoor mocap has no geodetic fix; xM/yM/altM carry the local ENU pose.
  state.pose = { lat: 0, lon: 0, altM, xM, yM, zM: -altM };

  const attitude = quaternionToEuler(body.attitude as Record<string, unknown> | undefined);
  if (attitude) {
    state.attitude = attitude;
  }

  const trackingValid = body.tracking_valid !== false;
  const residual = toFiniteNumber(body.residual) ?? 0;
  state.localization = {
    source: trackingValid ? 'mocap' : 'mocap (degraded)',
    fresh: trackingValid,
    quality: trackingValid ? Math.max(0, 1 - Math.min(residual / 0.05, 1)) : 0,
    updatedAtMs: nowMs
  };
}

function isValidMocapBody(body: unknown): body is Record<string, unknown> {
  const record = body as Record<string, unknown> | null | undefined;
  if (!record) {
    return false;
  }
  const position = record.position as Record<string, unknown> | undefined;
  const attitude = record.attitude as Record<string, unknown> | undefined;
  return (
    toFiniteNumber(position?.x) !== null &&
    toFiniteNumber(position?.y) !== null &&
    toFiniteNumber(position?.z) !== null &&
    toFiniteNumber(attitude?.w) !== null &&
    toFiniteNumber(attitude?.x) !== null &&
    toFiniteNumber(attitude?.y) !== null &&
    toFiniteNumber(attitude?.z) !== null
  );
}

function markMocapStale(state: VehicleState, nowMs: number): void {
  state.localization = {
    source: 'mocap',
    fresh: false,
    quality: 0,
    updatedAtMs: nowMs
  };
}

/**
 * Set estimator attitude from a decoded `synapse/v1/topic/attitude_estimate`.
 * The canonical `state.attitude` remains owned by mocap while tracking is live.
 */
function applyAttitudeEstimate(state: VehicleState, payload: unknown): void {
  const record = payload as Record<string, unknown> | null | undefined;
  const data = (record?.data ?? record) as Record<string, unknown> | undefined;
  const attitude = quaternionToEuler(data?.attitude as Record<string, unknown> | undefined);
  if (attitude) {
    state.attitudeEstimate = attitude;
  }
}

/**
 * Set flight mode, link quality, and a battery fallback from a decoded
 * `synapse/v1/topic/vehicle_health`. Power_status is the primary battery source;
 * vehicle_health fills in when it is absent (and skips unknown fields).
 */
function applyVehicleHealth(state: VehicleState, payload: unknown): void {
  const record = payload as Record<string, unknown> | null | undefined;
  const data = (record?.data ?? record) as Record<string, unknown> | undefined;
  if (!data) {
    return;
  }
  const flightMode = toFiniteNumber(data.flight_mode) ?? 0;
  state.mode = {
    name: CUBS2_FLIGHT_MODES[flightMode] ?? `mode ${flightMode}`,
    armed: Boolean(data.armed),
    failsafe: Boolean(data.failsafe)
  };
  const quality = clampRange(toFiniteNumber(data.link_quality_pct) ?? 0, 0, 100);
  state.link = {
    rssiDbm: -40 - (100 - quality) * 0.6,
    latencyMs: 20 + (100 - quality) * 0.5,
    packetLossPct: 100 - quality
  };
  // Battery fallback: prefer explicit power_status; a negative remaining_pct
  // means "unknown", so keep the existing value in that case.
  const remainingPct = toFiniteNumber(data.battery_remaining_pct);
  state.battery = {
    voltageV: toFiniteNumber(data.voltage_battery_v) ?? state.battery?.voltageV ?? 0,
    currentA: toFiniteNumber(data.current_battery_a) ?? state.battery?.currentA ?? 0,
    remainingPct: remainingPct !== null && remainingPct >= 0 ? remainingPct : state.battery?.remainingPct ?? 0
  };
}

/** Set battery from a decoded `synapse/v1/topic/power_status` payload. */
function applyPowerStatus(state: VehicleState, payload: unknown): void {
  const record = payload as Record<string, unknown> | null | undefined;
  const data = (record?.data ?? record) as Record<string, unknown> | undefined;
  if (!data) {
    return;
  }
  state.battery = {
    voltageV: toFiniteNumber(data.voltage_v) ?? state.battery?.voltageV ?? 0,
    currentA: toFiniteNumber(data.current_a) ?? state.battery?.currentA ?? 0,
    remainingPct: toFiniteNumber(data.remaining_pct) ?? state.battery?.remainingPct ?? 0
  };
}

/** ZYX (yaw-pitch-roll) quaternion `{x,y,z,w}` to UI Euler degrees. */
function quaternionToEuler(quat: Record<string, unknown> | undefined): Attitude | null {
  if (!quat) {
    return null;
  }
  const x = toFiniteNumber(quat.x);
  const y = toFiniteNumber(quat.y);
  const z = toFiniteNumber(quat.z);
  const w = toFiniteNumber(quat.w);
  if (x === null || y === null || z === null || w === null) {
    return null;
  }
  const sinrCosp = 2 * (w * x + y * z);
  const cosrCosp = 1 - 2 * (x * x + y * y);
  const roll = Math.atan2(sinrCosp, cosrCosp);
  const sinp = 2 * (w * y - z * x);
  const pitch = Math.abs(sinp) >= 1 ? Math.sign(sinp) * (Math.PI / 2) : Math.asin(sinp);
  const sinyCosp = 2 * (w * z + x * y);
  const cosyCosp = 1 - 2 * (y * y + z * z);
  const yaw = Math.atan2(sinyCosp, cosyCosp);
  const deg = 180 / Math.PI;
  // Mocap/model quaternions are body-to-ENU for a FLU body. The raw Y-axis
  // Euler angle is positive nose-down; the UI displays pitch positive nose-up.
  return { rollDeg: roll * deg, pitchDeg: -pitch * deg, yawDeg: yaw * deg, qx: x, qy: y, qz: z, qw: w };
}

function clampRange(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

export function refreshStaleTopics(state: VehicleState, nowMs = Date.now()): VehicleState {
  let hasFreshTopic = false;

  for (const snapshot of Object.values(state.topics)) {
    if (snapshot.staleTimeoutMs <= 0) {
      continue;
    }

    snapshot.stale = nowMs - snapshot.lastReceiveMs > snapshot.staleTimeoutMs;
    if (!snapshot.stale) {
      hasFreshTopic = true;
    }
  }

  state.connected = hasFreshTopic && nowMs - state.lastUpdateMs < 3000;
  state.localization.fresh =
    state.localization.fresh && state.localization.updatedAtMs > 0 && nowMs - state.localization.updatedAtMs < 1200;
  return state;
}

export function appendEvent(state: VehicleState, event: EventMessage): VehicleState {
  state.events = [event, ...state.events].slice(0, MAX_EVENTS);
  return state;
}

function updateTopicSnapshot(state: VehicleState, frame: GcsFrame, nowMs: number): TopicSnapshot {
  const definition = resolveTopicDefinition(frame.topic);
  const current = state.topics[frame.topic];
  const sourceMs = frame.header.sourceTimeNs / 1_000_000;
  const arrivalTimesMs = [...(current?.arrivalTimesMs ?? []), nowMs].filter((timeMs) => nowMs - timeMs <= RATE_WINDOW_MS);
  const elapsedMs = arrivalTimesMs.at(-1)! - arrivalTimesMs[0]!;
  const rateHz = elapsedMs > 0 ? ((arrivalTimesMs.length - 1) * 1000) / elapsedMs : 0;

  return {
    topic: frame.topic,
    key: definition?.key ?? 'unknown',
    label: definition?.label ?? frame.topic.split('/').at(-1) ?? frame.topic,
    schema: definition?.schema ?? frame.header.messageType,
    expectedRateHz: definition?.expectedRateHz ?? 0,
    staleTimeoutMs: definition?.staleTimeoutMs ?? 1000,
    units: definition?.units ?? '',
    loggable: definition?.loggable ?? true,
    lastSequence: frame.header.sequence,
    lastReceiveMs: nowMs,
    latencyMs: Math.max(0, nowMs - sourceMs),
    rateHz,
    stale: false,
    samples: (current?.samples ?? 0) + 1,
    arrivalTimesMs
  };
}

// Browser-side decoding of Synapse FlatBuffer payloads observed on Zenoh.
//
// This is a faithful TypeScript port of the native ground-bridge decoder
// (`crates/electrode-ground-bridge/src/synapse_decode.rs`) so that direct Zenoh
// connections produce byte-identical telemetry frames to the bridge. The
// committed readers were generated from the published `@cognipilot/synapse-fbs`
// schemas; normal development and CI do not require `flatc`.
//
// Wire encoding (synapse_fbs 0.7.0): every topic is `table X { data: XData; }`
// EXCEPT fixed-layout structs, which are transmitted as the *bare* `*Data`
// struct on the wire (raw fixed-size struct bytes, NOT a flatbuffer root table).
// Struct topics are decoded with `new XData().__init(0, bb)`; only the `mocap`
// topic is a real root TABLE.
//
// Topics use bare compact catalog keys (e.g. `att`, `manual`, `pwm`) resolved
// through the published topic catalog, and every catalog-keyed sample must
// carry the mandatory Zenoh encoding string
// `application/x-synapse-struct;type=<wireType>;schema=sha256-128:<hash>`
// (`application/x-flatbuffers;...` for root-table topics). Custom non-catalog
// keys (`synapse/mocap/...`, `synapse/motor_output`) are exempt.
import { parseKey, type TopicInfo } from '@cognipilot/synapse-fbs/topic_catalog';
import * as flatbuffers from 'flatbuffers';

import { AttitudeCommandData } from './generated/synapse/topic/attitude-command-data.js';
import { AttitudeEstimateData } from './generated/synapse/topic/attitude-estimate-data.js';
import { AttitudeEstimateFlags } from './generated/synapse/topic/attitude-estimate-flags.js';
import { ControlLoopMetricsData } from './generated/synapse/topic/control-loop-metrics-data.js';
import { ExternalOdometryData } from './generated/synapse/topic/external-odometry-data.js';
import { ExternalOdometryFlags } from './generated/synapse/topic/external-odometry-flags.js';
import { ExternalOdometryStatus } from './generated/synapse/topic/external-odometry-status.js';
import { NavigationTargetData } from './generated/synapse/topic/navigation-target-data.js';
import { LocalPositionCommandData } from './generated/synapse/topic/local-position-command-data.js';
import { ManualControlData } from './generated/synapse/topic/manual-control-data.js';
import { ManualControlFlags } from './generated/synapse/topic/manual-control-flags.js';
import { MissionProgressData } from './generated/synapse/topic/mission-progress-data.js';
import { MocapFrame } from './generated/synapse/topic/mocap-frame.js';
import { MocapPoseFrame } from './generated/synapse/topic/mocap-pose-frame.js';
import { MocapRawFlags } from './generated/synapse/topic/mocap-raw-flags.js';
import { PowerStatusData } from './generated/synapse/topic/power-status-data.js';
import { RawPoseData } from './generated/synapse/topic/raw-pose-data.js';
import { PwmSignalOutputsData } from './generated/synapse/topic/pwm-signal-outputs-data.js';
import { RadioControlData } from './generated/synapse/topic/radio-control-data.js';
import { TrajectorySegmentData } from './generated/synapse/topic/trajectory-segment-data.js';
import { VehicleHealthData } from './generated/synapse/topic/vehicle-health-data.js';
import { VehicleHealthFlags } from './generated/synapse/topic/vehicle-health-flags.js';

/** A payload decoded (or passed through) from a Zenoh sample. */
export interface Decoded {
  /** Human-facing message type, e.g. `AttitudeEstimate` or `Raw`. */
  schema: string;
  /** JSON-serializable payload forwarded to the browser state pipeline. */
  payload: unknown;
  /** True when we decoded a known Synapse topic, false for the raw fallback. */
  decoded: boolean;
}

/** Decoder schema names by catalog topic name (topics we decode today). */
const SCHEMA_BY_TOPIC_NAME: Record<string, string> = {
  ExternalOdometry: 'ExternalOdometry',
  MocapFrame: 'MocapFrame',
  MocapPoseFrame: 'MocapPoseFrame',
  ManualControlCommand: 'ManualControl',
  RadioControl: 'RadioControl',
  PwmSignalOutputs: 'PwmSignalOutputs',
  AttitudeEstimate: 'AttitudeEstimate',
  AttitudeCommand: 'AttitudeCommand',
  NavigationTarget: 'NavigationTarget',
  ControlLoopMetrics: 'ControlLoopMetrics',
  VehicleHealth: 'VehicleHealth',
  PowerStatus: 'PowerStatus',
  RawPose: 'RawPose',
  MissionProgress: 'MissionProgress',
  LocalPositionCommand: 'LocalPositionCommand',
  TrajectorySegment: 'TrajectorySegment'
  // OpticalFlow / OpticalFlowVelocity / LockstepTick: raw passthrough for now.
};

/**
 * Classify a Zenoh key into the Synapse schema we expect on it. Catalog topics
 * use bare compact keys (`att`, `manual`, possibly namespaced/instanced —
 * `robot/manual`, `external_pose/1`) resolved via `parseKey`. Custom
 * non-catalog keys (the bridge-parity mocap trio and `synapse/motor_output`)
 * are matched explicitly.
 */
export function classify(key: string): string {
  if (key.includes('rigid_body_names')) {
    return 'MocapRigidBodyNames';
  }
  if (
    key.endsWith('mocap/frame') ||
    key.endsWith('mocap_frame') ||
    key.includes('synapse/mocap/rigid_body/') ||
    key.includes('synapse/mocap/selected/rigid_body/')
  ) {
    return 'MocapFrame';
  }
  const parsed = parseKey(key);
  if (parsed) {
    return SCHEMA_BY_TOPIC_NAME[parsed.topic.name] ?? 'Raw';
  }
  if (key.endsWith('motor_output')) {
    return 'PwmSignalOutputs';
  }
  return 'Raw';
}

/** Mandatory value-contract encoding string for a catalog topic. */
export function expectedTopicEncoding(topic: TopicInfo): string {
  const mediaType = topic.fixedLayout ? 'application/x-synapse-struct' : 'application/x-flatbuffers';
  return `${mediaType};type=${topic.wireType};schema=sha256-128:${topic.schemaHash}`;
}

/**
 * Strict value-contract check: catalog-keyed samples must carry the exact
 * encoding string for their topic. Returns a human-readable error for a
 * missing/mismatched encoding, or null when the sample may be decoded.
 * Non-catalog (custom) keys are exempt.
 */
function contractError(key: string, encoding: string | null | undefined): string | null {
  // Bridge-owned keys predate the compact catalog grammar. Some end in a
  // catalog-looking leaf (for example `/pose`), but their wire contract is
  // defined by synapse_qualisys_bridge, not by that leaf.
  if (
    key.endsWith('mocap_frame') ||
    key.endsWith('mocap/frame') ||
    key.includes('synapse/mocap/rigid_body/') ||
    key.includes('synapse/mocap/selected/rigid_body/')
  ) {
    return null;
  }
  const topic = parseKey(key)?.topic;
  if (!topic) {
    return null;
  }
  const expected = expectedTopicEncoding(topic);
  if (!encoding) {
    return `missing encoding; expected ${expected}`;
  }
  // zenoh-pico represents unregistered/custom media types as the schema of
  // its default byte encoding.  On the wire that round-trips as
  // `zenoh/bytes;<original encoding>`.  The suffix is still the complete,
  // exact Synapse value contract, so unwrap only this known transport prefix
  // before enforcing the contract.
  const normalized = encoding.startsWith('zenoh/bytes;')
    ? encoding.slice('zenoh/bytes;'.length)
    : encoding;
  if (normalized !== expected) {
    return `encoding mismatch: got ${encoding}; expected ${expected}`;
  }
  return null;
}

/**
 * Decode a Zenoh sample by key, falling back to a raw preview. `encoding` is
 * the sample's Zenoh encoding string; catalog-keyed samples with a missing or
 * mismatched encoding are NOT decoded (raw fallback with `contractError`).
 */
export function decode(key: string, bytes: Uint8Array, encoding?: string | null): Decoded {
  const schema = classify(key);
  const violation = contractError(key, encoding);
  if (violation) {
    return { schema, payload: { ...rawPayload(bytes), contractError: violation }, decoded: false };
  }
  switch (schema) {
    case 'AttitudeEstimate':
      return decodeOrRaw(schema, bytes, decodeAttitudeEstimate);
    case 'AttitudeCommand':
      return decodeOrRaw(schema, bytes, decodeAttitudeCommand);
    case 'NavigationTarget':
      return decodeOrRaw(schema, bytes, decodeNavigationTarget);
    case 'ControlLoopMetrics':
      return decodeOrRaw(schema, bytes, decodeControlLoopMetrics);
    case 'VehicleHealth':
      return decodeOrRaw(schema, bytes, decodeVehicleHealth);
    case 'PowerStatus':
      return decodeOrRaw(schema, bytes, decodePowerStatus);
    case 'ManualControl':
      return decodeOrRaw(schema, bytes, decodeManualControl);
    case 'RadioControl':
      return decodeOrRaw(schema, bytes, decodeRadioControl);
    case 'PwmSignalOutputs':
      return decodeOrRaw(schema, bytes, decodePwmSignalOutputs);
    case 'ExternalOdometry':
      return decodeOrRaw(schema, bytes, decodeExternalOdometry);
    case 'RawPose':
      return decodeOrRaw(schema, bytes, decodeRawPose);
    case 'MocapFrame':
      return decodeOrRaw(schema, bytes, decodeMocapFrame);
    case 'MocapPoseFrame':
      return decodeOrRaw(schema, bytes, decodeMocapPoseFrame);
    case 'MocapRigidBodyNames':
      return decodeOrRaw(schema, bytes, (value) => JSON.parse(new TextDecoder().decode(value)));
    case 'MissionProgress':
      return decodeOrRaw(schema, bytes, decodeMissionProgress);
    case 'LocalPositionCommand':
      return decodeOrRaw(schema, bytes, decodeLocalPositionCommand);
    case 'TrajectorySegment':
      return decodeOrRaw(schema, bytes, decodeTrajectorySegment);
    default:
      return { schema, payload: rawPayload(bytes), decoded: false };
  }
}

function decodeExternalOdometry(bytes: Uint8Array): unknown | null {
  if (bytes.length !== ExternalOdometryData.sizeOf()) {
    return null;
  }
  const data = new ExternalOdometryData().__init(0, byteBuffer(bytes));
  const position = data.positionEnuM();
  const attitude = data.attitude();
  const linearVelocity = data.linearVelocityEnuMS();
  const angularVelocity = data.angularVelocityFluRadS();
  if (!position || !attitude || !linearVelocity || !angularVelocity) {
    return null;
  }
  const flags = data.flags();
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      position: { x: position.x(), y: position.y(), z: position.z() },
      attitude: { w: attitude.w(), x: attitude.x(), y: attitude.y(), z: attitude.z() },
      linear_velocity: {
        x: linearVelocity.x(),
        y: linearVelocity.y(),
        z: linearVelocity.z()
      },
      angular_velocity: {
        roll: angularVelocity.roll(),
        pitch: angularVelocity.pitch(),
        yaw: angularVelocity.yaw()
      },
      flags,
      status: externalOdometryStatusName(data.status()),
      source_id: data.sourceId(),
      id: data.id(),
      position_valid: hasFlag(flags, ExternalOdometryFlags.PositionValid),
      attitude_valid: hasFlag(flags, ExternalOdometryFlags.AttitudeValid),
      linear_velocity_valid: hasFlag(flags, ExternalOdometryFlags.LinearVelocityValid),
      angular_velocity_valid: hasFlag(flags, ExternalOdometryFlags.AngularVelocityValid),
      extrapolated: hasFlag(flags, ExternalOdometryFlags.Extrapolated),
      outlier_rejected: hasFlag(flags, ExternalOdometryFlags.OutlierRejected),
      degraded: hasFlag(flags, ExternalOdometryFlags.Degraded),
      lost: hasFlag(flags, ExternalOdometryFlags.Lost)
    }
  };
}

function externalOdometryStatusName(status: ExternalOdometryStatus): string {
  return ExternalOdometryStatus[status] ?? `Unknown(${status})`;
}

function decodeRadioControl(bytes: Uint8Array): unknown | null {
  const data = new RadioControlData().__init(0, byteBuffer(bytes));
  const channels: Record<string, number> = {
    ch0: data.chan0RawUs(),
    ch1: data.chan1RawUs(),
    ch2: data.chan2RawUs(),
    ch3: data.chan3RawUs(),
    ch4: data.chan4RawUs(),
    ch5: data.chan5RawUs(),
    ch6: data.chan6RawUs(),
    ch7: data.chan7RawUs(),
    ch8: data.chan8RawUs(),
    ch9: data.chan9RawUs(),
    ch10: data.chan10RawUs(),
    ch11: data.chan11RawUs(),
    ch12: data.chan12RawUs(),
    ch13: data.chan13RawUs(),
    ch14: data.chan14RawUs(),
    ch15: data.chan15RawUs(),
    ch16: data.chan16RawUs(),
    ch17: data.chan17RawUs()
  };
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      channel_count: data.channelCount(),
      link_quality_pct: data.linkQualityPct()
    },
    channels
  };
}

function decodeOrRaw(
  schema: string,
  bytes: Uint8Array,
  decoder: (bytes: Uint8Array) => unknown | null
): Decoded {
  let payload: unknown | null;
  try {
    payload = decoder(bytes);
  } catch {
    return { schema, payload: rawPayload(bytes), decoded: false };
  }
  return payload === null
    ? { schema, payload: rawPayload(bytes), decoded: false }
    : { schema, payload, decoded: true };
}

function rawPayload(bytes: Uint8Array): { bytes: number; hexPreview: string } {
  let hexPreview = '';
  for (let i = 0; i < Math.min(32, bytes.length); i += 1) {
    hexPreview += bytes[i].toString(16).padStart(2, '0');
  }
  return { bytes: bytes.length, hexPreview };
}

function byteBuffer(bytes: Uint8Array): flatbuffers.ByteBuffer {
  return new flatbuffers.ByteBuffer(bytes);
}

function hasFlag(flags: number, bit: number): boolean {
  return (flags & bit) !== 0;
}

/**
 * Decode a bare fixed-layout struct topic. Struct topics carry the raw `*Data`
 * struct bytes at offset 0 (not a flatbuffer root table).
 */
function decodeAttitudeEstimate(bytes: Uint8Array): unknown | null {
  const data = new AttitudeEstimateData().__init(0, byteBuffer(bytes));
  const attitude = data.attitude();
  const rates = data.angularVelocityFluRadS();
  if (!attitude || !rates) {
    return null;
  }
  const flags = data.flags();
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      attitude: { w: attitude.w(), x: attitude.x(), y: attitude.y(), z: attitude.z() },
      angular_velocity: { roll: rates.roll(), pitch: rates.pitch(), yaw: rates.yaw() },
      attitude_valid: hasFlag(flags, AttitudeEstimateFlags.AttitudeValid),
      rates_valid: hasFlag(flags, AttitudeEstimateFlags.RatesValid)
    }
  };
}

function decodeAttitudeCommand(bytes: Uint8Array): unknown | null {
  const data = new AttitudeCommandData().__init(0, byteBuffer(bytes));
  const attitude = data.attitude();
  const rates = data.bodyRateFluRadS();
  if (!attitude || !rates) {
    return null;
  }
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      attitude: { w: attitude.w(), x: attitude.x(), y: attitude.y(), z: attitude.z() },
      body_rate_flu_rad_s: { roll: rates.roll(), pitch: rates.pitch(), yaw: rates.yaw() },
      thrust: data.thrust(),
      type_mask: data.typeMask()
    }
  };
}

function decodeNavigationTarget(bytes: Uint8Array): unknown | null {
  const data = new NavigationTargetData().__init(0, byteBuffer(bytes));
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      altitude_error_m: data.altitudeErrorM(),
      airspeed_error_m_s: data.airspeedErrorMS(),
      xtrack_error_m: data.xtrackErrorM(),
      desired_roll_deg: data.desiredRollCdeg() / 100,
      desired_pitch_deg: data.desiredPitchCdeg() / 100,
      desired_yaw_deg: data.desiredYawCdeg() / 100,
      target_yaw_deg: data.targetYawCdeg() / 100,
      distance_to_waypoint_m: data.distanceToWaypointM()
    }
  };
}

function decodeControlLoopMetrics(bytes: Uint8Array): unknown | null {
  const data = new ControlLoopMetricsData().__init(0, byteBuffer(bytes));
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      period_us: data.periodUs(),
      latency_us: data.latencyUs(),
      overrun_count: data.overrunCount(),
      load_pct: data.loadDpermille() / 10
    }
  };
}

function decodeVehicleHealth(bytes: Uint8Array): unknown | null {
  const data = new VehicleHealthData().__init(0, byteBuffer(bytes));
  const flags = data.flags();
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      flight_mode: data.flightMode(),
      link_quality_pct: data.linkQualityPct(),
      voltage_battery_v: data.voltageBatteryCv() / 100,
      current_battery_a: data.currentBatteryDa() / 10,
      battery_remaining_pct: data.batteryRemainingPct(),
      armed: hasFlag(flags, VehicleHealthFlags.Armed),
      failsafe: hasFlag(flags, VehicleHealthFlags.Failsafe),
      system_state: data.systemState(),
      load_pct: data.loadDpermille() / 10
    }
  };
}

function decodePowerStatus(bytes: Uint8Array): unknown | null {
  const data = new PowerStatusData().__init(0, byteBuffer(bytes));
  const voltages = data.voltages();
  if (!voltages) {
    return null;
  }
  const cellsMv: number[] = [
    voltages.cell0Mv(),
    voltages.cell1Mv(),
    voltages.cell2Mv(),
    voltages.cell3Mv(),
    voltages.cell4Mv(),
    voltages.cell5Mv(),
    voltages.cell6Mv(),
    voltages.cell7Mv(),
    voltages.cell8Mv(),
    voltages.cell9Mv(),
    voltages.cell10Mv(),
    voltages.cell11Mv(),
    voltages.cell12Mv(),
    voltages.cell13Mv(),
    voltages.cell14Mv(),
    voltages.cell15Mv()
  ];
  const packMv = cellsMv.reduce((sum, mv) => (mv > 0 ? sum + mv : sum), 0);
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      voltage_v: packMv / 1000,
      current_a: data.currentBatteryDa() / 10,
      remaining_pct: data.remainingPct(),
      connected: data.connected(),
      cells_mv: cellsMv,
      temperature_c: data.temperatureCdeg() / 100
    }
  };
}

function decodeManualControl(bytes: Uint8Array): unknown | null {
  const data = new ManualControlData().__init(0, byteBuffer(bytes));
  const flags = data.flags();
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      axes: {
        roll: data.rollMilli() / 1000,
        pitch: data.pitchMilli() / 1000,
        yaw: data.yawMilli() / 1000,
        throttle: data.throttleMilli() / 1000
      },
      aux: [
        data.aux0Milli() / 1000,
        data.aux1Milli() / 1000,
        data.aux2Milli() / 1000,
        data.aux3Milli() / 1000,
        data.aux4Milli() / 1000,
        data.aux5Milli() / 1000
      ],
      flight_mode: data.flightMode(),
      arm_switch: hasFlag(flags, ManualControlFlags.ArmSwitch),
      kill_switch: hasFlag(flags, ManualControlFlags.KillSwitch),
      active: hasFlag(flags, ManualControlFlags.Active),
      valid: hasFlag(flags, ManualControlFlags.Valid),
      buttons: data.buttons()
    }
  };
}

function decodePwmSignalOutputs(bytes: Uint8Array): unknown | null {
  const data = new PwmSignalOutputsData().__init(0, byteBuffer(bytes));
  const outputsUs: number[] = [
    data.output0Us(),
    data.output1Us(),
    data.output2Us(),
    data.output3Us(),
    data.output4Us(),
    data.output5Us(),
    data.output6Us(),
    data.output7Us(),
    data.output8Us(),
    data.output9Us(),
    data.output10Us(),
    data.output11Us(),
    data.output12Us(),
    data.output13Us(),
    data.output14Us(),
    data.output15Us()
  ];
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      active_mask: data.activeMask(),
      port: data.port(),
      outputs_us: outputsUs
    },
    // Kept so state-store's `parseMotorOutputs` stays simple.
    motors: { m0: outputsUs[0], m1: outputsUs[1], m2: outputsUs[2], m3: outputsUs[3] }
  };
}

function decodeMocapFrame(bytes: Uint8Array): unknown | null {
  if (bytes.length === 28) {
    return decodeCompactRigidBodyPose(bytes);
  }

  const frame = MocapFrame.getRootAsMocapFrame(byteBuffer(bytes));

  const rigidBodies: unknown[] = [];
  for (let i = 0; i < frame.rigidBodiesLength(); i += 1) {
    const rigid = frame.rigidBodies(i);
    const position = rigid?.positionEnuM();
    const rotation = rigid?.rotation();
    if (!rigid || !position || !rotation) {
      continue;
    }
    const attitude = rotationMatrixToQuaternion({
      r11: rotation.r11(),
      r12: rotation.r12(),
      r13: rotation.r13(),
      r21: rotation.r21(),
      r22: rotation.r22(),
      r23: rotation.r23(),
      r31: rotation.r31(),
      r32: rotation.r32(),
      r33: rotation.r33()
    });
    rigidBodies.push({
      id: rigid.id(),
      position: { x: position.x(), y: position.y(), z: position.z() },
      attitude,
      residual: rigid.residualMm(),
      tracking_valid: hasFlag(rigid.flags(), MocapRawFlags.Valid)
    });
  }

  const labeledMarkers: unknown[] = [];
  for (let i = 0; i < frame.markersLength(); i += 1) {
    const marker = frame.markers(i);
    const position = marker?.positionEnuM();
    if (!marker || !position) {
      continue;
    }
    labeledMarkers.push({
      id: marker.id(),
      position: { x: position.x(), y: position.y(), z: position.z() },
      residual: marker.residualMm()
    });
  }

  return {
    timestamp_us: Number(frame.timestampUs()),
    frame_number: frame.frameNumber(),
    drop_rate_2d_dpermille: frame.dropRate2dDpermille(),
    out_of_sync_rate_2d_dpermille: frame.outOfSyncRate2dDpermille(),
    rigid_bodies: rigidBodies,
    labeled_markers: labeledMarkers
  };
}

function decodeRawPose(bytes: Uint8Array): unknown | null {
  const data = new RawPoseData().__init(0, byteBuffer(bytes));
  const pose = data.pose();
  const position = pose?.positionEnuM();
  const attitude = pose?.attitude();
  if (!pose || !position || !attitude) return null;
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      position: { x: position.x(), y: position.y(), z: position.z() },
      attitude: { x: attitude.x(), y: attitude.y(), z: attitude.z(), w: attitude.w() }
    }
  };
}

/** Decode Synapse 0.7's authoritative raw quaternion mocap stream. */
function decodeMocapPoseFrame(bytes: Uint8Array): unknown | null {
  if (bytes.byteLength < 4) return null;
  const frame = MocapPoseFrame.getRootAsMocapPoseFrame(byteBuffer(bytes));
  const rigidBodies = Array.from({ length: frame.rigidBodiesLength() }, (_, index) => {
    const body = frame.rigidBodies(index);
    const pose = body?.pose();
    const position = pose?.positionEnuM();
    const attitude = pose?.attitude();
    if (!body || !position || !attitude) return null;
    const flags = body.flags();
    return {
      id: body.id(),
      position: { x: position.x(), y: position.y(), z: position.z() },
      attitude: { x: attitude.x(), y: attitude.y(), z: attitude.z(), w: attitude.w() },
      residual: body.residualMm(),
      tracking_valid: (flags & MocapRawFlags.Valid) !== 0
    };
  }).filter((body) => body !== null);
  const labeledMarkers = Array.from({ length: frame.markersLength() }, (_, index) => {
    const marker = frame.markers(index);
    const position = marker?.positionEnuM();
    if (!marker || !position) return null;
    return {
      id: marker.id(),
      position: { x: position.x(), y: position.y(), z: position.z() },
      residual: marker.residualMm()
    };
  }).filter((marker) => marker !== null);
  return {
    timestamp_us: Number(frame.timestampUs()),
    frame_number: frame.frameNumber(),
    rigid_bodies: rigidBodies,
    labeled_markers: labeledMarkers
  };
}

interface RotationMatrix {
  r11: number;
  r12: number;
  r13: number;
  r21: number;
  r22: number;
  r23: number;
  r31: number;
  r32: number;
  r33: number;
}

function rotationMatrixToQuaternion(matrix: RotationMatrix): {
  x: number;
  y: number;
  z: number;
  w: number;
} {
  const trace = matrix.r11 + matrix.r22 + matrix.r33;
  if (trace > 0) {
    const scale = Math.sqrt(trace + 1) * 2;
    return normalizeQuaternion({
      w: 0.25 * scale,
      x: (matrix.r32 - matrix.r23) / scale,
      y: (matrix.r13 - matrix.r31) / scale,
      z: (matrix.r21 - matrix.r12) / scale
    });
  }
  if (matrix.r11 > matrix.r22 && matrix.r11 > matrix.r33) {
    const scale = Math.sqrt(1 + matrix.r11 - matrix.r22 - matrix.r33) * 2;
    return normalizeQuaternion({
      w: (matrix.r32 - matrix.r23) / scale,
      x: 0.25 * scale,
      y: (matrix.r12 + matrix.r21) / scale,
      z: (matrix.r13 + matrix.r31) / scale
    });
  }
  if (matrix.r22 > matrix.r33) {
    const scale = Math.sqrt(1 + matrix.r22 - matrix.r11 - matrix.r33) * 2;
    return normalizeQuaternion({
      w: (matrix.r13 - matrix.r31) / scale,
      x: (matrix.r12 + matrix.r21) / scale,
      y: 0.25 * scale,
      z: (matrix.r23 + matrix.r32) / scale
    });
  }
  const scale = Math.sqrt(1 + matrix.r33 - matrix.r11 - matrix.r22) * 2;
  return normalizeQuaternion({
    w: (matrix.r21 - matrix.r12) / scale,
    x: (matrix.r13 + matrix.r31) / scale,
    y: (matrix.r23 + matrix.r32) / scale,
    z: 0.25 * scale
  });
}

function normalizeQuaternion(quaternion: { x: number; y: number; z: number; w: number }): {
  x: number;
  y: number;
  z: number;
  w: number;
} {
  const norm = Math.hypot(quaternion.w, quaternion.x, quaternion.y, quaternion.z);
  if (!Number.isFinite(norm) || norm === 0) {
    return { x: 0, y: 0, z: 0, w: 1 };
  }
  return {
    x: quaternion.x / norm,
    y: quaternion.y / norm,
    z: quaternion.z / norm,
    w: quaternion.w / norm
  };
}

// Compact per-rigid-body pose published by mocap bridges (synapse_qualisys_bridge)
// on `synapse/mocap/rigid_body/<name>/pose`: 7 little-endian f32 values
// [px, py, pz, qx, qy, qz, qw] — position in ENU metres, then the attitude
// quaternion with the scalar (w) LAST on the wire. Per the synapse_fbs mocap
// schema the quaternion rotates body FLU vectors into the mocap ENU frame;
// producers (the QTM rigid-body definition, the sim plant) must deliver an
// FLU-aligned body frame — no per-body correction is applied here.
function decodeCompactRigidBodyPose(bytes: Uint8Array): unknown | null {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const values = Array.from({ length: 7 }, (_, index) => view.getFloat32(index * 4, true));
  if (!values.every(Number.isFinite)) {
    return null;
  }
  const [x, y, z, qx, qy, qz, qw] = values;
  return {
    timestamp_us: 0,
    frame_number: 0,
    rigid_bodies: [
      {
        id: 0,
        position: { x, y, z },
        attitude: { x: qx, y: qy, z: qz, w: qw },
        residual: 0,
        tracking_valid: true
      }
    ]
  };
}

const MISSION_STATE_NAMES: Record<number, string> = {
  0: 'unknown',
  1: 'idle',
  2: 'active',
  3: 'paused',
  4: 'complete'
};

function decodeMissionProgress(bytes: Uint8Array): unknown | null {
  const data = new MissionProgressData().__init(0, byteBuffer(bytes));
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      mission_id: data.missionId(),
      current_seq: data.currentSeq(),
      total: data.total(),
      mission_state: MISSION_STATE_NAMES[data.missionState()] ?? 'unknown',
      mission_mode: data.missionMode()
    }
  };
}

function decodeLocalPositionCommand(bytes: Uint8Array): unknown | null {
  const data = new LocalPositionCommandData().__init(0, byteBuffer(bytes));
  const position = data.positionEnuM();
  if (!position) {
    return null;
  }
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      position_enu_m: { x: position.x(), y: position.y(), z: position.z() },
      yaw_rad: data.yawRad(),
      type_mask: data.typeMask(),
      coordinate_frame: data.coordinateFrame()
    }
  };
}

function decodeTrajectorySegment(bytes: Uint8Array): unknown | null {
  const data = new TrajectorySegmentData().__init(0, byteBuffer(bytes));
  const points = [
    pointPayload(data.p0EnuM(), data.yaw0Rad()),
    pointPayload(data.p1EnuM(), data.yaw1Rad()),
    pointPayload(data.p2EnuM(), data.yaw2Rad()),
    pointPayload(data.p3EnuM(), data.yaw3Rad()),
    pointPayload(data.p4EnuM(), data.yaw4Rad()),
    pointPayload(data.p5EnuM(), data.yaw5Rad()),
    pointPayload(data.p6EnuM(), data.yaw6Rad()),
    pointPayload(data.p7EnuM(), data.yaw7Rad())
  ];
  return {
    data: {
      timestamp_us: Number(data.timestampUs()),
      start_time_us: Number(data.startTimeUs()),
      trajectory_id: data.trajectoryId(),
      segment_seq: data.segmentSeq(),
      duration_us: data.durationUs(),
      plan_version: data.planVersion(),
      flags: data.flags(),
      trajectory_type: data.trajectoryType(),
      degree: data.degree(),
      coordinate_frame: data.frame(),
      id: data.id(),
      points
    }
  };
}

function pointPayload(
  point: { x(): number; y(): number; z(): number } | null,
  yawRad: number
): { x: number; y: number; z: number; yaw_rad: number } | null {
  return point ? { x: point.x(), y: point.y(), z: point.z(), yaw_rad: yawRad } : null;
}

export type RuntimeMode = 'zenoh' | 'replay';
export type Priority = 'low' | 'normal' | 'high' | 'critical';
export type Severity = 'info' | 'warning' | 'error';

export interface MessageHeader {
  sequence: number;
  sourceTimeNs: number;
  receiveTimeNs: number;
  expireTimeNs: number;
  vehicleId: string;
  schemaVersion: number;
  messageType: string;
  priority: Priority;
  streamId: string;
}

export interface TelemetryFrame<TPayload = unknown> {
  kind: 'telemetry';
  topic: string;
  header: MessageHeader;
  payload: TPayload;
}

export interface EventFrame {
  kind: 'event';
  topic: string;
  header: MessageHeader;
  payload: EventMessage;
}

export type GcsFrame = TelemetryFrame | EventFrame;

export interface Pose {
  lat: number;
  lon: number;
  altM: number;
  xM: number;
  yM: number;
  zM: number;
}

export interface Velocity {
  northMps: number;
  eastMps: number;
  downMps: number;
  groundSpeedMps: number;
}

export interface Attitude {
  rollDeg: number;
  pitchDeg: number;
  yawDeg: number;
}

/**
 * Normalized control inputs driving the vehicle. Roll/pitch/yaw are bipolar
 * stick/surface commands in [-1, 1]; throttle is [0, 1]. Populated from the
 * live `synapse/v1/topic/manual_control_command` axes (or, in the future, controller surface
 * commands) so the 3D vehicle view can animate control-surface deflection.
 */
export interface ControlInputs {
  aileron: number;
  elevator: number;
  rudder: number;
  throttle: number;
}

/**
 * Full decoded `synapse/v1/topic/manual_control_command` frame: the raw transmitter stick axes
 * plus the arm/kill/mode switches. Unlike {@link ControlInputs} (which is
 * remapped to control-surface names for the 3D view), this preserves the
 * pilot-facing roll/pitch/yaw/throttle stick values and switch states so the
 * RC transmitter view can mirror what the physical sticks are commanding.
 * Roll/pitch/yaw are bipolar in [-1, 1]; throttle is [0, 1].
 */
export interface ManualControlState {
  roll: number;
  pitch: number;
  yaw: number;
  throttle: number;
  flightMode: number;
  armSwitch: boolean;
  killSwitch: boolean;
  active: boolean;
  valid: boolean;
  updatedAtMs: number;
}

export interface Battery {
  voltageV: number;
  currentA: number;
  remainingPct: number;
}

export interface LinkStatus {
  rssiDbm: number;
  latencyMs: number;
  packetLossPct: number;
}

export interface ModeState {
  name: string;
  armed: boolean;
  failsafe: boolean;
}

export interface LocalizationState {
  source: string;
  fresh: boolean;
  quality: number;
  updatedAtMs: number;
}

export interface EventMessage {
  severity: Severity;
  code: string;
  message: string;
  timestampMs: number;
}

export interface TopicDefinition {
  key: string;
  topic: string;
  label: string;
  schema: string;
  expectedRateHz: number;
  staleTimeoutMs: number;
  loggable: boolean;
  display: boolean;
  units: string;
  reliability: 'latest-wins' | 'ordered' | 'acknowledged';
  commandAuthority: 'none' | 'operator' | 'bridge';
}

export interface TopicSnapshot {
  topic: string;
  key: string;
  label: string;
  schema: string;
  expectedRateHz: number;
  staleTimeoutMs: number;
  units: string;
  loggable: boolean;
  lastSequence: number;
  lastReceiveMs: number;
  latencyMs: number;
  rateHz: number;
  stale: boolean;
  samples: number;
  arrivalTimesMs: number[];
}

export interface VehicleState {
  vehicleId: string;
  connected: boolean;
  lastUpdateMs: number;
  pose: Pose | null;
  velocity: Velocity | null;
  attitude: Attitude | null;
  controls: ControlInputs | null;
  manualControl: ManualControlState | null;
  radioControl: number[] | null;
  motors: number[] | null;
  battery: Battery | null;
  link: LinkStatus | null;
  mode: ModeState;
  localization: LocalizationState;
  events: EventMessage[];
  topics: Record<string, TopicSnapshot>;
  commandHistory: CommandResult[];
  /**
   * Last mocap sample (ENU metres + wall-clock ms), retained so the adapter can
   * derive velocity by finite-differencing successive mocap pose frames.
   * positions. Internal to the state store; not published.
   */
  lastMocap: { tMs: number; xM: number; yM: number; altM: number } | null;
}

export type CommandName =
  | 'arm'
  | 'disarm'
  | 'setMode'
  | 'land'
  | 'return'
  | 'clearMission'
  | 'uploadMission'
  | 'setParameter';

export interface CommandDefinition {
  command: CommandName;
  topic: string;
  label: string;
  description: string;
  requiresConnected: boolean;
  requiresLocalizationFresh: boolean;
  requiresNotFailsafe: boolean;
  requiresConfirmation: boolean;
  timeoutMs: number;
  ackTopic: string;
}

export interface CommandIntent {
  kind: 'command';
  commandId: string;
  command: CommandName;
  vehicleId: string;
  topic: string;
  args: Record<string, unknown>;
  createdAtMs: number;
  expiresAtMs: number;
  sequence: number;
}

export interface CommandResult {
  kind: 'commandAck';
  commandId: string;
  command: CommandName;
  status: 'acked' | 'published' | 'rejected' | 'timeout';
  reason: string;
  sequence: number;
  receivedAtMs: number;
}

export interface ConnectionState {
  mode: RuntimeMode;
  status: 'disconnected' | 'connecting' | 'connected' | 'error';
  url: string;
  message: string;
}

export interface ReplayState {
  loaded: boolean;
  playing: boolean;
  cursorMs: number;
  durationMs: number;
  speed: number;
  frameCount: number;
}

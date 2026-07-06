import { gcsUrl } from './capabilities';

/**
 * Client for the local Ground Station backend (`electrode-ground-station`).
 * Only meaningful when {@link isGroundStation} is true.
 */

export type DeviceKind = 'joystick' | 'serial';

export interface DetectedDevice {
  kind: DeviceKind;
  /** Device node, e.g. `/dev/input/js0` or `/dev/ttyACM0`. */
  path: string;
  /** Best-effort human name, e.g. `FrSky Taranis Joystick`. */
  name: string;
}

export interface DevicesResponse {
  joysticks: DetectedDevice[];
  serial: DetectedDevice[];
}

/** List joystick and serial devices the local host currently exposes. */
export async function fetchDevices(signal?: AbortSignal): Promise<DevicesResponse> {
  const response = await fetch(gcsUrl('devices'), { signal });
  if (!response.ok) {
    throw new Error(`gcs/devices responded ${response.status}`);
  }
  return (await response.json()) as DevicesResponse;
}

/** RC mapping profile — mirrors the manual-control-bridge configuration. */
export interface MappingProfile {
  device: string;
  zenohConnect: string;
  rollAxis: number;
  invertRoll: boolean;
  pitchAxis: number;
  invertPitch: boolean;
  yawAxis: number;
  invertYaw: boolean;
  throttleAxis: number;
  invertThrottle: boolean;
  modeAxis: number;
  activeAxis: number;
  invertActive: boolean;
  armButton: number | null;
  killButton: number | null;
  armToggle: boolean;
  killToggle: boolean;
  ppmChannelMap: number[];
  ppmChannelInvert: boolean[];
  ppmForceIdleThrottle: boolean;
  ppmForceStabilizingMode: boolean;
}

export interface BridgeStatus {
  running: boolean;
  bin: string;
  ppmRunning?: boolean;
  ppmBin?: string;
}

export type SimulationBackend = 'rumoca';
export type SimulationMode = 'withAutopilot' | 'directCommands';
export type SimulationVehicleKind = 'fixedWing' | 'quadrotor';

export interface SimulationProfile {
  backend: SimulationBackend;
  mode: SimulationMode;
  vehicleKind: SimulationVehicleKind;
  projectPath: string;
  generatedConfigPath: string;
  modelPath: string;
  modelEditable: boolean;
  modelicaLspCommand: string;
  timingMode: string;
  simulationDt: number;
  lockstepSendRateHz: number;
  lockstepReceiveRateHz: number;
  lockstepMaxStepDt: number;
  zenohConnect: string;
  commandInputTopic: string;
  actuatorOutputTopic: string;
  sensorOutputTopic: string;
  telemetryOutputTopic: string;
}

export interface ModelicaFile {
  path: string;
  text: string;
  editable: boolean;
  lspCommand: string;
}

/** Live raw joystick state pushed over the inspector WebSocket. */
export interface JoystickSnapshot {
  device: string;
  name: string;
  axes: number[];
  buttons: number[];
}

export async function fetchMapping(signal?: AbortSignal): Promise<MappingProfile> {
  const response = await fetch(gcsUrl('mapping'), { signal });
  if (!response.ok) {
    throw new Error(`gcs/mapping responded ${response.status}`);
  }
  return (await response.json()) as MappingProfile;
}

export async function saveMapping(profile: MappingProfile): Promise<MappingProfile> {
  const response = await fetch(gcsUrl('mapping'), {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(profile)
  });
  if (!response.ok) {
    throw new Error(`saving mapping failed (${response.status})`);
  }
  return (await response.json()) as MappingProfile;
}

export async function fetchBridgeStatus(signal?: AbortSignal): Promise<BridgeStatus> {
  const response = await fetch(gcsUrl('bridge'), { signal });
  if (!response.ok) {
    throw new Error(`gcs/bridge responded ${response.status}`);
  }
  return (await response.json()) as BridgeStatus;
}

export async function setBridgeRunning(running: boolean): Promise<BridgeStatus> {
  const response = await fetch(gcsUrl(running ? 'bridge/start' : 'bridge/stop'), { method: 'POST' });
  if (!response.ok) {
    throw new Error(`bridge ${running ? 'start' : 'stop'} failed (${response.status})`);
  }
  return (await response.json()) as BridgeStatus;
}

export async function fetchPpmBridgeStatus(signal?: AbortSignal): Promise<BridgeStatus> {
  const response = await fetch(gcsUrl('ppm'), { signal });
  if (!response.ok) {
    throw new Error(`gcs/ppm responded ${response.status}`);
  }
  return (await response.json()) as BridgeStatus;
}

export async function setPpmBridgeRunning(running: boolean): Promise<BridgeStatus> {
  const response = await fetch(gcsUrl(running ? 'ppm/start' : 'ppm/stop'), { method: 'POST' });
  if (!response.ok) {
    throw new Error(`ppm bridge ${running ? 'start' : 'stop'} failed (${response.status})`);
  }
  return (await response.json()) as BridgeStatus;
}

/** Live state of the daemon-supervised native autopilot (cubs2 native_sim + Zenoh link). */
export interface AutopilotRunStatus {
  running: boolean;
  pid: number | null;
  startedAtMs: number | null;
  message: string;
  binary: string;
  logPath: string;
  /** Frames forwarded autopilot → Zenoh since start. */
  framesOut: number;
  /** Frames forwarded Zenoh → autopilot since start. */
  framesIn: number;
}

export type FirmwareSource = 'localBuild' | 'releaseArtifact' | 'ciArtifact' | 'customFile';
export type FlashMethod = 'usbBootloader' | 'dfu' | 'serialBootloader' | 'sdCard' | 'externalTool';
export type RuntimeTransport = 'zenoh' | 'mavlinkSerial' | 'mavlinkUdp' | 'mavlinkTcp';
export type RuntimeProtocol = 'synapseZenoh' | 'mavlink';

export interface AutopilotProfile {
  stackName: string;
  stackPath: string;
  firmwareSource: FirmwareSource;
  firmwareArtifact: string;
  boardTarget: string;
  flashMethod: FlashMethod;
  runtimeTransport: RuntimeTransport;
  runtimeEndpoint: string;
  missionProtocol: RuntimeProtocol;
  parameterProtocol: RuntimeProtocol;
  calibrationProtocol: RuntimeProtocol;
  nativeBinary: string;
  udpRxPort: number;
  udpTxPort: number;
  inboundTopics: string[];
}

export async function fetchAutopilotProfile(signal?: AbortSignal): Promise<AutopilotProfile> {
  const response = await fetch(gcsUrl('autopilot'), { signal });
  if (!response.ok) {
    throw new Error(`gcs/autopilot responded ${response.status}`);
  }
  return (await response.json()) as AutopilotProfile;
}

export async function saveAutopilotProfile(profile: AutopilotProfile): Promise<AutopilotProfile> {
  const response = await fetch(gcsUrl('autopilot'), {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(profile)
  });
  if (!response.ok) {
    throw new Error(`saving autopilot profile failed (${response.status})`);
  }
  return (await response.json()) as AutopilotProfile;
}

export async function fetchAutopilotRunStatus(signal?: AbortSignal): Promise<AutopilotRunStatus> {
  const response = await fetch(gcsUrl('autopilot/status'), { signal });
  if (!response.ok) {
    throw new Error(`gcs/autopilot/status responded ${response.status}`);
  }
  return (await response.json()) as AutopilotRunStatus;
}

export async function setAutopilotRunning(action: 'start' | 'stop'): Promise<AutopilotRunStatus> {
  const response = await fetch(gcsUrl(`autopilot/${action}`), { method: 'POST' });
  if (!response.ok) {
    const detail = await response.text().catch(() => '');
    throw new Error(detail || `autopilot ${action} failed (${response.status})`);
  }
  return (await response.json()) as AutopilotRunStatus;
}

export async function fetchSimulationProfile(signal?: AbortSignal): Promise<SimulationProfile> {
  const response = await fetch(gcsUrl('simulation'), { signal });
  if (!response.ok) {
    throw new Error(`gcs/simulation responded ${response.status}`);
  }
  return (await response.json()) as SimulationProfile;
}

export async function saveSimulationProfile(profile: SimulationProfile): Promise<SimulationProfile> {
  const response = await fetch(gcsUrl('simulation'), {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(profile)
  });
  if (!response.ok) {
    throw new Error(`saving simulation profile failed (${response.status})`);
  }
  return (await response.json()) as SimulationProfile;
}

export async function fetchSimulationModel(signal?: AbortSignal): Promise<ModelicaFile> {
  const response = await fetch(gcsUrl('simulation/model'), { signal });
  if (!response.ok) {
    throw new Error(`gcs/simulation/model responded ${response.status}`);
  }
  return (await response.json()) as ModelicaFile;
}

export async function saveSimulationModel(path: string, text: string): Promise<ModelicaFile> {
  const response = await fetch(gcsUrl('simulation/model'), {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path, text })
  });
  if (!response.ok) {
    throw new Error(`saving simulation model failed (${response.status})`);
  }
  return (await response.json()) as ModelicaFile;
}

/** Open a WebSocket to the live raw-joystick inspector for a device. */
export function openJoystickSocket(device: string): WebSocket {
  const url = new URL(gcsUrl('joystick'));
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  url.searchParams.set('device', device);
  return new WebSocket(url.toString());
}

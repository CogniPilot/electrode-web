// Browser-side decoding of Synapse FlatBuffer payloads observed on Zenoh.
//
// This is a faithful TypeScript port of the native ground-bridge decoder
// (`crates/electrode-ground-bridge/src/synapse_decode.rs`) so that direct Zenoh
// connections produce byte-identical telemetry frames to the bridge. The
// committed readers were generated from the published `@cognipilot/synapse-fbs`
// schemas; normal development and CI do not require `flatc`.
//
// Wire encoding (synapse_fbs 0.3.0): every topic is `table X { data: XData; }`
// EXCEPT fixed-layout structs, which are transmitted as the *bare* `*Data`
// struct on the wire (raw fixed-size struct bytes, NOT a flatbuffer root table).
// Struct topics are decoded with `new XData().__init(0, bb)`; only `mocap_frame`
// is a real root TABLE.
import * as flatbuffers from 'flatbuffers';

import { AttitudeEstimateData } from './generated/synapse/topic/attitude-estimate-data.js';
import { AttitudeEstimateFlags } from './generated/synapse/topic/attitude-estimate-flags.js';
import { ManualControlData } from './generated/synapse/topic/manual-control-data.js';
import { ManualControlFlags } from './generated/synapse/topic/manual-control-flags.js';
import { MocapFrame } from './generated/synapse/topic/mocap-frame.js';
import { PowerStatusData } from './generated/synapse/topic/power-status-data.js';
import { PwmSignalOutputsData } from './generated/synapse/topic/pwm-signal-outputs-data.js';
import { RadioControlData } from './generated/synapse/topic/radio-control-data.js';
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

/**
 * Classify a Zenoh key into the Synapse schema we expect on it, matching on the
 * `key_suffix` leaf of the `synapse/v1/topic/<suffix>` key (possibly namespaced
 * and/or instance-suffixed, so we test with `includes`).
 */
export function classify(key: string): string {
  if (key.includes('mocap_frame') || key.includes('synapse/mocap/rigid_body/')) {
    return 'MocapFrame';
  }
  if (key.includes('manual_control_command')) {
    return 'ManualControl';
  }
  if (key.includes('radio_control')) {
    return 'RadioControl';
  }
  if (key.includes('pwm_signal_outputs') || key.endsWith('motor_output')) {
    return 'PwmSignalOutputs';
  }
  if (key.includes('attitude_estimate')) {
    return 'AttitudeEstimate';
  }
  if (key.includes('vehicle_health')) {
    return 'VehicleHealth';
  }
  if (key.includes('power_status')) {
    return 'PowerStatus';
  }
  // optical_flow / optical_flow_velocity: raw passthrough for now.
  return 'Raw';
}

/** Decode a Zenoh sample by key, falling back to a raw preview. */
export function decode(key: string, bytes: Uint8Array): Decoded {
  const schema = classify(key);
  switch (schema) {
    case 'AttitudeEstimate':
      return decodeOrRaw(schema, bytes, decodeAttitudeEstimate);
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
    case 'MocapFrame':
      return decodeOrRaw(schema, bytes, decodeMocapFrame);
    default:
      return { schema, payload: rawPayload(bytes), decoded: false };
  }
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
    const attitude = rigid?.attitude();
    if (!rigid || !position || !attitude) {
      continue;
    }
    rigidBodies.push({
      id: rigid.id(),
      position: { x: position.x(), y: position.y(), z: position.z() },
      attitude: { x: attitude.x(), y: attitude.y(), z: attitude.z(), w: attitude.w() },
      residual: rigid.residual(),
      tracking_valid: rigid.trackingValid()
    });
  }

  const labeledMarkers: unknown[] = [];
  for (let i = 0; i < frame.labeledMarkersLength(); i += 1) {
    const marker = frame.labeledMarkers(i);
    const position = marker?.positionEnuM();
    if (!marker || !position) {
      continue;
    }
    labeledMarkers.push({
      id: marker.id(),
      position: { x: position.x(), y: position.y(), z: position.z() },
      residual: marker.residual()
    });
  }

  return {
    timestamp_us: Number(frame.timestampUs()),
    frame_number: frame.frameNumber(),
    rigid_bodies: rigidBodies,
    labeled_markers: labeledMarkers
  };
}

function decodeCompactRigidBodyPose(bytes: Uint8Array): unknown | null {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const values = Array.from({ length: 7 }, (_, index) => view.getFloat32(index * 4, true));
  if (!values.every(Number.isFinite)) {
    return null;
  }
  const [x, y, z, qwRaw, qxRaw, qyRaw, qzRaw] = values;
  // The LAN pose topic carries the tracked rigid-body frame. The cub1 rigid
  // body forward axis is opposite the aircraft FLU forward axis, so convert
  // q_world_rigid to q_world_aircraft with a right-side 180 deg Y correction:
  // q * [0, 0, 1, 0].
  const qw = -qyRaw;
  const qx = -qzRaw;
  const qy = qwRaw;
  const qz = qxRaw;
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

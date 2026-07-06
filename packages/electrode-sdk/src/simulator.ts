import { ELECTRODE_SCHEMA_VERSION } from '@electrode/flatbuffers';
import { DEFAULT_VEHICLE_ID, resolveTopic, TOPIC_DEFINITIONS } from './topics';
import type { EventFrame, GcsFrame, MessageHeader, TelemetryFrame } from './types';

export interface SimulationOptions {
  vehicleId?: string;
  elapsedMs: number;
  sequenceStart: number;
  nowMs?: number;
  armed?: boolean;
  mode?: string;
}

type SynapseFrameBuilder = <TPayload>(
  topic: string,
  messageType: string,
  streamId: string,
  payload: TPayload,
  ttlMs?: number
) => TelemetryFrame<TPayload>;

type SimulationState = {
  vehicleId: string;
  t: number;
  nowMs: number;
  xM: number;
  yM: number;
  altM: number;
  northMps: number;
  eastMps: number;
  rollRad: number;
  pitchRad: number;
  yawRad: number;
  rollInput: number;
  pitchInput: number;
  yawInput: number;
  throttleInput: number;
  voltageV: number;
  currentA: number;
  localizationQuality: number;
  remainingPct: number;
  linkQualityPct: number;
  flightMode: number;
  armed: boolean;
  rc: Record<string, number>;
  motorMix: number[];
  motorPwm: number[];
  timestampUs: number;
};

export function makeHeader(options: {
  vehicleId: string;
  sequence: number;
  nowMs: number;
  ttlMs: number;
  messageType: string;
  streamId: string;
}): MessageHeader {
  return {
    sequence: options.sequence,
    sourceTimeNs: options.nowMs * 1_000_000,
    receiveTimeNs: options.nowMs * 1_000_000,
    expireTimeNs: (options.nowMs + options.ttlMs) * 1_000_000,
    vehicleId: options.vehicleId,
    schemaVersion: ELECTRODE_SCHEMA_VERSION,
    messageType: options.messageType,
    priority: 'normal',
    streamId: options.streamId
  };
}

export function makeSimulatedTelemetryBundle(options: SimulationOptions): TelemetryFrame[] {
  const state = makeSimulationState(options);
  const synapseFrame = createSynapseFrameBuilder(options, state);

  return [
    makePowerStatusFrame(state, synapseFrame),
    makeVehicleHealthFrame(state, synapseFrame),
    makeManualControlFrame(state, synapseFrame),
    makeAttitudeFrame(state, synapseFrame),
    makePwmOutputsFrame(state, synapseFrame),
    makeRadioControlFrame(state, synapseFrame),
    makeMocapPoseFrame(state, synapseFrame),
    makeMocapFrame(options, state, synapseFrame),
    makeOpticalFlowFrame(state, synapseFrame),
    makeOpticalFlowVelocityFrame(state, synapseFrame),
    makeSimInputFrame(options, state, synapseFrame)
  ];
}

function makeSimulationState(options: SimulationOptions): SimulationState {
  const vehicleId = options.vehicleId ?? DEFAULT_VEHICLE_ID;
  const t = options.elapsedMs / 1000;
  const nowMs = options.nowMs ?? Date.now();
  const radiusM = 42;
  const xM = Math.cos(t / 8) * radiusM;
  const yM = Math.sin(t / 8) * radiusM;
  const altM = 18 + Math.sin(t / 5) * 3;
  const northMps = Math.cos(t / 4) * 1.8;
  const eastMps = -Math.sin(t / 4) * 1.8;
  const rollDeg = Math.sin(t * 1.3) * 12;
  const pitchDeg = Math.cos(t * 0.9) * 8;
  const yawDeg = ((t * 18) % 360 + 360) % 360;
  const voltageV = 15.9 - t * 0.002;
  const currentA = 4.5 + Math.sin(t) * 1.2;
  const localizationQuality = 0.91 + Math.sin(t / 6) * 0.05;
  const armed = options.armed ?? true;
  const rollRad = degToRad(rollDeg);
  const pitchRad = degToRad(pitchDeg);
  const yawRad = degToRad(yawDeg);
  const rollInput = Math.sin(t * 0.9) * 0.32;
  const pitchInput = Math.cos(t * 0.72) * 0.26;
  const yawInput = Math.sin(t * 0.43) * 0.22;
  const throttleInput = clamp01(0.58 + Math.sin(t * 0.37) * 0.12);
  const rc = makeRcChannels(rollInput, pitchInput, yawInput, throttleInput, armed);
  const motorMix = [
    throttleInput + rollInput * 0.13 - pitchInput * 0.11 + yawInput * 0.07,
    throttleInput - rollInput * 0.13 - pitchInput * 0.11 - yawInput * 0.07,
    throttleInput - rollInput * 0.13 + pitchInput * 0.11 + yawInput * 0.07,
    throttleInput + rollInput * 0.13 + pitchInput * 0.11 - yawInput * 0.07
  ].map(clamp01);
  const timestampUs = Math.round(nowMs * 1000);
  const motorPwm = motorMix.map((value) => Math.round(1000 + value * 1000));
  const remainingPct = Math.max(22, 94 - t * 0.05);
  const linkQualityPct = Math.round(localizationQuality * 100);
  const flightMode = modeToSynapseFlightMode(options.mode ?? 'hold');

  return {
    vehicleId,
    t,
    nowMs,
    xM,
    yM,
    altM,
    northMps,
    eastMps,
    rollRad,
    pitchRad,
    yawRad,
    rollInput,
    pitchInput,
    yawInput,
    throttleInput,
    voltageV,
    currentA,
    localizationQuality,
    remainingPct,
    linkQualityPct,
    flightMode,
    armed,
    rc,
    motorMix,
    motorPwm,
    timestampUs
  };
}

function createSynapseFrameBuilder(options: SimulationOptions, state: SimulationState): SynapseFrameBuilder {
  let sequence = options.sequenceStart;
  return <TPayload>(topic: string, messageType: string, streamId: string, payload: TPayload, ttlMs = 1000) => ({
    kind: 'telemetry',
    topic,
    header: makeHeader({
      vehicleId: state.vehicleId,
      sequence: sequence++,
      nowMs: state.nowMs,
      ttlMs,
      messageType,
      streamId
    }),
    payload
  });
}

function makePowerStatusFrame(state: SimulationState, frame: SynapseFrameBuilder): TelemetryFrame {
  return frame(
    'synapse/v1/topic/power_status',
    'synapse.topic.PowerStatus',
    'synapse_power_status',
    {
      data: {
        timestamp_us: state.timestampUs,
        voltage_v: state.voltageV,
        current_a: state.currentA,
        remaining_pct: state.remainingPct,
        connected: true,
        cells_mv: [],
        temperature_c: 0
      }
    },
    2500
  );
}

function makeVehicleHealthFrame(state: SimulationState, frame: SynapseFrameBuilder): TelemetryFrame {
  return frame(
    'synapse/v1/topic/vehicle_health',
    'synapse.topic.VehicleHealth',
    'synapse_vehicle_health',
    {
      data: {
        timestamp_us: state.timestampUs,
        flight_mode: state.flightMode,
        link_quality_pct: state.linkQualityPct,
        voltage_battery_v: state.voltageV,
        current_battery_a: state.currentA,
        battery_remaining_pct: Math.round(state.remainingPct),
        armed: state.armed,
        failsafe: false,
        system_state: 0,
        load_pct: Math.round(30 + Math.sin(state.t * 2.1) * 8)
      }
    },
    2500
  );
}

function makeManualControlFrame(state: SimulationState, frame: SynapseFrameBuilder): TelemetryFrame {
  return frame(
    'synapse/v1/topic/manual_control_command',
    'synapse.topic.ManualControlCommand',
    'synapse_manual_control_command',
    {
      data: {
        timestamp_us: state.timestampUs,
        axes: {
          roll: state.rollInput,
          pitch: state.pitchInput,
          yaw: state.yawInput,
          throttle: state.throttleInput
        },
        aux: makeAux(state.t),
        flight_mode: state.flightMode,
        arm_switch: state.armed,
        kill_switch: false,
        active: true,
        valid: true,
        buttons: 0
      }
    },
    180
  );
}

function makeAttitudeFrame(state: SimulationState, frame: SynapseFrameBuilder): TelemetryFrame {
  return frame(
    'synapse/v1/topic/attitude_estimate',
    'synapse.topic.AttitudeEstimate',
    'synapse_attitude_estimate',
    {
      data: {
        timestamp_us: state.timestampUs,
        attitude: eulerToQuaternion(state.rollRad, state.pitchRad, state.yawRad),
        angular_velocity: {
          roll: degToRad(Math.cos(state.t * 1.3) * 15.6),
          pitch: degToRad(-Math.sin(state.t * 0.9) * 7.2),
          yaw: degToRad(18)
        },
        attitude_valid: true,
        rates_valid: true
      }
    },
    180
  );
}

function makePwmOutputsFrame(state: SimulationState, frame: SynapseFrameBuilder): TelemetryFrame {
  return frame(
    'synapse/v1/topic/pwm_signal_outputs',
    'synapse.topic.PwmSignalOutputs',
    'synapse_pwm_signal_outputs',
    {
      data: {
        timestamp_us: state.timestampUs,
        active_mask: state.armed ? 0b1111 : 0,
        port: 0,
        outputs_us: [...state.motorPwm, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
      },
      motors: motorMixObject(state.motorPwm)
    },
    180
  );
}

function makeRadioControlFrame(state: SimulationState, frame: SynapseFrameBuilder): TelemetryFrame {
  return frame(
    'synapse/v1/topic/radio_control',
    'synapse.topic.RadioControl',
    'synapse_radio_control',
    state.rc,
    180
  );
}

/**
 * Compact per-rigid-body pose topic — mirrors what decode() produces for the
 * 28-byte payload real mocap bridges publish (single body, no markers).
 */
function makeMocapPoseFrame(state: SimulationState, frame: SynapseFrameBuilder): TelemetryFrame {
  return frame(
    'synapse/mocap/rigid_body/cub1/pose',
    'synapse.topic.MocapFrame',
    'synapse_mocap_rigid_body_cub1_pose',
    {
      timestamp_us: 0,
      frame_number: 0,
      rigid_bodies: [
        {
          id: 0,
          position: { x: state.xM, y: state.yM, z: state.altM },
          attitude: eulerToQuaternion(state.rollRad, state.pitchRad, state.yawRad),
          residual: 0,
          tracking_valid: true
        }
      ]
    },
    180
  );
}

/** Full mocap frame topic carrying markers alongside the rigid bodies. */
function makeMocapFrame(
  options: SimulationOptions,
  state: SimulationState,
  frame: SynapseFrameBuilder
): TelemetryFrame {
  return frame(
    'synapse/mocap/frame',
    'synapse.topic.MocapFrame',
    'synapse_mocap_frame',
    {
      timestamp_us: state.timestampUs,
      frame_number: Math.floor(options.elapsedMs / 10),
      labeled_markers: [
        {
          id: 1,
          position: { x: state.xM + 0.28, y: state.yM, z: state.altM + 0.08 },
          residual: 0.0009 + Math.abs(Math.sin(state.t)) * 0.0006
        }
      ],
      unlabeled_markers: [],
      rigid_bodies: [
        {
          id: 1,
          position: { x: state.xM, y: state.yM, z: state.altM },
          attitude: eulerToQuaternion(state.rollRad, state.pitchRad, state.yawRad),
          residual: 0.0014 + Math.abs(Math.cos(state.t / 2)) * 0.0008,
          tracking_valid: true
        }
      ],
      skeleton_segments: []
    },
    180
  );
}

function makeOpticalFlowFrame(state: SimulationState, frame: SynapseFrameBuilder): TelemetryFrame {
  return frame(
    'synapse/v1/topic/optical_flow',
    'synapse.topic.OpticalFlow',
    'synapse_optical_flow',
    {
      data: {
        timestamp_us: state.timestampUs,
        pixel_flow: { x: state.eastMps * 0.016, y: state.northMps * 0.016 },
        delta_angle: {
          x: state.rollRad * 0.004,
          y: state.pitchRad * 0.004,
          z: state.yawInput * 0.006
        },
        distance_m: Math.max(0.35, state.altM),
        integration_timespan_us: 10_000,
        quality: Math.round(state.localizationQuality * 100),
        max_flow_rate: 8,
        min_ground_distance: 0.25,
        max_ground_distance: 60
      }
    },
    180
  );
}

function makeOpticalFlowVelocityFrame(state: SimulationState, frame: SynapseFrameBuilder): TelemetryFrame {
  return frame(
    'synapse/v1/topic/optical_flow_velocity',
    'synapse.topic.OpticalFlowVelocity',
    'synapse_optical_flow_velocity',
    {
      data: {
        timestamp_us: state.timestampUs,
        vel_body: {
          x: state.northMps * Math.cos(state.yawRad) + state.eastMps * Math.sin(state.yawRad),
          y: -state.northMps * Math.sin(state.yawRad) + state.eastMps * Math.cos(state.yawRad)
        },
        vel_ne: { x: state.northMps, y: state.eastMps },
        flow_rate_uncompensated: { x: state.eastMps * 0.11, y: state.northMps * 0.11 },
        flow_rate_compensated: { x: state.eastMps * 0.09, y: state.northMps * 0.09 },
        gyro_rate: { x: state.rollInput * 0.12, y: state.pitchInput * 0.12, z: state.yawInput * 0.1 }
      }
    },
    180
  );
}

function makeSimInputFrame(
  options: SimulationOptions,
  state: SimulationState,
  frame: SynapseFrameBuilder
): TelemetryFrame {
  return frame(
    'synapse/v1/sil/sim_input',
    'synapse.sil.SimInput',
    'synapse_sim_input',
    {
      gyro: {
        x: degToRad(Math.cos(state.t * 1.3) * 15.6),
        y: degToRad(-Math.sin(state.t * 0.9) * 7.2),
        z: degToRad(18)
      },
      accel: {
        x: Math.sin(state.pitchRad) * 9.81,
        y: -Math.sin(state.rollRad) * 9.81,
        z: -Math.cos(state.rollRad) * Math.cos(state.pitchRad) * 9.81
      },
      rc: state.rc,
      rc_link_quality: Math.round(state.localizationQuality * 100),
      rc_valid: true,
      imu_valid: true,
      target_boot_time_ns: Math.round(options.elapsedMs * 1_000_000)
    },
    180
  );
}

function makeRcChannels(roll: number, pitch: number, yaw: number, throttle: number, armed: boolean): Record<string, number> {
  return {
    ch0: pwmFromBipolar(roll),
    ch1: pwmFromBipolar(pitch),
    ch2: Math.round(1000 + clamp01(throttle) * 1000),
    ch3: pwmFromBipolar(yaw),
    ch4: armed ? 1900 : 1100,
    ch5: 1500,
    ch6: 1500,
    ch7: 1500,
    ch8: 1500,
    ch9: 1500,
    ch10: 1500,
    ch11: 1500,
    ch12: 1500,
    ch13: 1500,
    ch14: 1500,
    ch15: 1500
  };
}

function makeAux(t: number): number[] {
  return [
    Math.sin(t * 0.31),
    Math.cos(t * 0.29),
    Math.sin(t * 0.19) * 0.5,
    Math.cos(t * 0.17) * 0.5,
    0,
    0
  ];
}

function motorMixObject(values: number[]): Record<string, number> {
  return {
    m0: values[0] ?? 0,
    m1: values[1] ?? 0,
    m2: values[2] ?? 0,
    m3: values[3] ?? 0
  };
}

function eulerToQuaternion(roll: number, pitch: number, yaw: number): Record<string, number> {
  const cr = Math.cos(roll * 0.5);
  const sr = Math.sin(roll * 0.5);
  const cp = Math.cos(pitch * 0.5);
  const sp = Math.sin(pitch * 0.5);
  const cy = Math.cos(yaw * 0.5);
  const sy = Math.sin(yaw * 0.5);

  return {
    x: sr * cp * cy - cr * sp * sy,
    y: cr * sp * cy + sr * cp * sy,
    z: cr * cp * sy - sr * sp * cy,
    w: cr * cp * cy + sr * sp * sy
  };
}

function pwmFromBipolar(value: number): number {
  return Math.round(1500 + clamp(value, -1, 1) * 500);
}

function modeToSynapseFlightMode(mode: string): number {
  // cubs2 convention (producer-defined field): 0 = manual, 1 = auto.
  return mode === 'manual' ? 0 : 1;
}

function degToRad(degrees: number): number {
  return (degrees * Math.PI) / 180;
}

function clamp01(value: number): number {
  return clamp(value, 0, 1);
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

export function makeSimulatedEvent(vehicleId = DEFAULT_VEHICLE_ID, sequence = 1, message = 'Simulator heartbeat'): EventFrame {
  const nowMs = Date.now();
  return {
    kind: 'event',
    topic: resolveTopic(TOPIC_DEFINITIONS.event.topic, vehicleId),
    header: makeHeader({
      vehicleId,
      sequence,
      nowMs,
      ttlMs: 0,
      messageType: 'Event',
      streamId: 'event'
    }),
    payload: {
      severity: 'info',
      code: 'sim',
      message,
      timestampMs: nowMs
    }
  };
}

export function createDemoReplay(vehicleId = DEFAULT_VEHICLE_ID, durationMs = 60_000, stepMs = 100): GcsFrame[] {
  const frames: GcsFrame[] = [];
  let sequence = 1;
  const baseMs = Date.now();

  for (let elapsedMs = 0; elapsedMs <= durationMs; elapsedMs += stepMs) {
    const bundle = makeSimulatedTelemetryBundle({
      vehicleId,
      elapsedMs,
      sequenceStart: sequence,
      nowMs: baseMs + elapsedMs,
      armed: elapsedMs > 5000,
      mode: elapsedMs > 30_000 ? 'mission' : 'hold'
    });
    sequence += bundle.length;
    frames.push(...bundle);

    if (elapsedMs % 10_000 === 0) {
      frames.push(makeSimulatedEvent(vehicleId, sequence++, elapsedMs === 0 ? 'Replay log loaded' : 'Replay marker'));
    }
  }

  return frames;
}

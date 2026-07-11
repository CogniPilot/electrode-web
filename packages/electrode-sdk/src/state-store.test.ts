import { describe, expect, it } from 'vitest';

import { ELECTRODE_SCHEMA_VERSION } from '@electrode/flatbuffers';
import {
  applyGcsFrame,
  createInitialVehicleState,
  refreshStaleTopics,
  setMocapDisplaySource
} from './state-store';
import { makeSimulatedTelemetryBundle } from './simulator';
import type { TelemetryFrame } from './types';

function telemetryFrame(topic: string, payload: unknown, nowMs: number): TelemetryFrame {
  return {
    kind: 'telemetry',
    topic,
    header: {
      sequence: 1,
      sourceTimeNs: nowMs * 1_000_000,
      receiveTimeNs: nowMs * 1_000_000,
      expireTimeNs: 0,
      vehicleId: 'cubs2',
      schemaVersion: ELECTRODE_SCHEMA_VERSION,
      messageType: 'test',
      priority: 'normal',
      streamId: topic
    },
    payload
  };
}

describe('state store telemetry pipeline', () => {
  it('derives vehicle state from raw Synapse telemetry frames', () => {
    let state = setMocapDisplaySource(createInitialVehicleState('cubs2'), 'raw');
    const first = makeSimulatedTelemetryBundle({
      vehicleId: 'cubs2',
      elapsedMs: 0,
      sequenceStart: 1,
      nowMs: 10_000,
      armed: true,
      mode: 'manual'
    });
    const second = makeSimulatedTelemetryBundle({
      vehicleId: 'cubs2',
      elapsedMs: 120,
      sequenceStart: first.length + 1,
      nowMs: 10_120,
      armed: true,
      mode: 'manual'
    });

    for (const frame of [...first, ...second]) {
      state = applyGcsFrame(state, frame, frame.header.receiveTimeNs / 1_000_000);
    }

    expect(state.connected).toBe(true);
    expect(state.pose).toMatchObject({ lat: 0, lon: 0 });
    expect(state.pose?.altM).toBeGreaterThan(17);
    expect(state.velocity?.groundSpeedMps).toBeGreaterThan(0);
    expect(state.attitude).not.toBeNull();
    expect(state.manualControl).toMatchObject({ active: true, valid: true, armSwitch: true });
    expect(state.controls?.throttle).toBeGreaterThan(0);
    expect(state.radioControl).toHaveLength(16);
    expect(state.motors).toHaveLength(4);
    expect(state.battery?.voltageV).toBeGreaterThan(0);
    expect(state.link?.packetLossPct).toBeLessThan(20);
    expect(state.mode).toMatchObject({ name: 'manual', armed: true, failsafe: false });
    expect(state.localization).toMatchObject({ source: 'mocap', fresh: true });
    expect(Object.keys(state.topics)).toContain('manual');
  });

  it('keeps mocap attitude authoritative when estimator attitude also arrives', () => {
    let state = setMocapDisplaySource(createInitialVehicleState('cubs2'), 'raw');
    const mocap = telemetryFrame(
      'synapse/mocap/rigid_body/cub1/pose',
      {
        rigid_bodies: [
          {
            position: { x: 1, y: 2, z: 3 },
            attitude: { w: 1, x: 0, y: 0, z: 0 },
            residual: 0,
            tracking_valid: true
          }
        ]
      },
      10_000
    );
    const estimate = telemetryFrame(
      'att',
      {
        data: {
          attitude: { w: Math.SQRT1_2, x: 0, y: 0, z: Math.SQRT1_2 },
          attitude_valid: true
        }
      },
      10_010
    );

    state = applyGcsFrame(state, mocap, 10_000);
    state = applyGcsFrame(state, estimate, 10_010);

    expect(state.attitude?.rollDeg).toBeCloseTo(0, 6);
    expect(state.attitude?.pitchDeg).toBeCloseTo(0, 6);
    expect(state.attitude?.yawDeg).toBeCloseTo(0, 6);
    expect(state.attitudeEstimate?.yawDeg).toBeCloseTo(90, 6);
    expect(state.localization).toMatchObject({ source: 'mocap', fresh: true });
  });

  it('marks connection and localization stale when topic deadlines pass', () => {
    let state = createInitialVehicleState('cubs2');
    const frames = makeSimulatedTelemetryBundle({
      vehicleId: 'cubs2',
      elapsedMs: 0,
      sequenceStart: 1,
      nowMs: 10_000
    });

    for (const frame of frames) {
      state = applyGcsFrame(state, frame, 10_000);
    }

    expect(state.connected).toBe(true);
    refreshStaleTopics(state, 14_000);

    expect(state.connected).toBe(false);
    expect(state.localization.fresh).toBe(false);
    expect(Object.values(state.topics).some((topic) => topic.stale)).toBe(true);
  });
});

describe('Qualisys raw pose state handling', () => {
  it('uses qualisys/cub1/pose_raw for pose and attitude', () => {
    let state = createInitialVehicleState('cubs2');
    const frame = telemetryFrame(
      'qualisys/cub1/pose_raw',
      {
        data: {
          timestamp_us: 1_000_000,
          position: { x: 10, y: 20, z: 3 },
          attitude: { w: 1, x: 0, y: 0, z: 0 },
        }
      },
      10_000
    );

    state = applyGcsFrame(state, frame, 10_000);

    expect(state.pose).toMatchObject({ xM: 10, yM: 20, altM: 3 });
    expect(state.attitude).toMatchObject({ rollDeg: 0, pitchDeg: -0, yawDeg: 0 });
    expect(state.localization).toMatchObject({
      source: 'mocap',
      fresh: true,
      quality: 1
    });
  });

  it('ignores the bridge estimate and keeps incoming raw mocap', () => {
    let state = createInitialVehicleState('cubs2');
    state = applyGcsFrame(
      state,
      telemetryFrame(
        'qualisys/cub1/pose',
        {
          data: {
            position: { x: 10, y: 20, z: 3 },
            attitude: { w: 1, x: 0, y: 0, z: 0 },
            position_valid: true,
            attitude_valid: true,
            linear_velocity_valid: false,
            lost: false,
            degraded: false,
            extrapolated: false,
            outlier_rejected: false,
            id: 1
          }
        },
        10_000
      ),
      10_000
    );
    state = applyGcsFrame(
      state,
      telemetryFrame(
        'synapse/mocap/frame',
        {
          rigid_bodies: [{
            id: 1,
            position: { x: 99, y: 88, z: 7 },
            attitude: { w: 1, x: 0, y: 0, z: 0 },
            tracking_valid: true
          }]
        },
        10_005
      ),
      10_005
    );

    expect(state.localization).toMatchObject({ source: 'mocap', fresh: true });
    expect(state.pose).toMatchObject({ xM: 99, yM: 88, altM: 7 });
  });

});

describe('mocap display source selection', () => {
  it('ignores lost external odometry and retains raw mocap', () => {
    let state = createInitialVehicleState('cubs2');
    state = applyGcsFrame(
      state,
      telemetryFrame(
        'synapse/mocap/frame',
        { rigid_bodies: [{ id: 1, position: { x: 2, y: 3, z: 4 }, attitude: { w: 1, x: 0, y: 0, z: 0 }, tracking_valid: true }] },
        10_000
      ),
      10_000
    );
    state = applyGcsFrame(
      state,
      telemetryFrame(
        'qualisys/cub1/pose',
        { data: { position_valid: false, lost: true, id: 1 } },
        10_005
      ),
      10_005
    );

    expect(state.localization).toMatchObject({ source: 'mocap', fresh: true });
    expect(state.pose).toMatchObject({ xM: 2, yM: 3, altM: 4 });
  });

  it('keeps raw mocap authoritative over external odometry', () => {
    let state = setMocapDisplaySource(createInitialVehicleState('cubs2'), 'raw');
    state = applyGcsFrame(
      state,
      telemetryFrame(
        'qualisys/cub1/pose',
        { data: { position: { x: 10, y: 20, z: 3 }, position_valid: true, id: 1 } },
        10_000
      ),
      10_000
    );
    state = applyGcsFrame(
      state,
      telemetryFrame(
        'synapse/mocap/frame',
        {
          rigid_bodies: [{
            id: 1,
            position: { x: 99, y: 88, z: 7 },
            attitude: { w: 1, x: 0, y: 0, z: 0 },
            tracking_valid: true
          }]
        },
        10_005
      ),
      10_005
    );

    expect(state.mocapDisplaySource).toBe('raw');
    expect(state.localization).toMatchObject({ source: 'mocap', fresh: true });
    expect(state.pose).toMatchObject({ xM: 99, yM: 88, altM: 7 });
  });
});

describe('mocap state handling', () => {
  it('preserves the last mocap pose when rigid body 0 becomes invalid', () => {
    let state = setMocapDisplaySource(createInitialVehicleState('cubs2'), 'raw');
    const valid = telemetryFrame(
      'synapse/mocap/frame',
      {
        rigid_bodies: [
          {
            position: { x: 7.4, y: -10.3, z: 0.32 },
            attitude: { w: 1, x: 0, y: 0, z: 0 },
            residual: 0.001,
            tracking_valid: true
          }
        ]
      },
      10_000
    );
    const invalid = telemetryFrame(
      'synapse/mocap/frame',
      {
        rigid_bodies: [
          {
            position: { x: Number.NaN, y: Number.NaN, z: Number.NaN },
            attitude: { w: Number.NaN, x: Number.NaN, y: Number.NaN, z: Number.NaN },
            residual: Number.NaN,
            tracking_valid: false
          },
          {
            position: { x: 0, y: 0, z: 0 },
            attitude: { w: 1, x: 0, y: 0, z: 0 },
            residual: 0,
            tracking_valid: true
          }
        ]
      },
      10_050
    );

    state = applyGcsFrame(state, valid, 10_000);
    expect(state.pose).toMatchObject({ xM: 7.4, yM: -10.3, altM: 0.32 });
    expect(state.localization).toMatchObject({ source: 'mocap', fresh: true });

    state = applyGcsFrame(state, invalid, 10_050);

    expect(state.pose).toMatchObject({ xM: 7.4, yM: -10.3, altM: 0.32 });
    expect(state.attitude?.yawDeg).toBeCloseTo(0, 6);
    expect(state.lastMocap).toMatchObject({ xM: 7.4, yM: -10.3, altM: 0.32 });
    expect(state.localization).toMatchObject({ source: 'mocap', fresh: false, quality: 0 });
  });
});

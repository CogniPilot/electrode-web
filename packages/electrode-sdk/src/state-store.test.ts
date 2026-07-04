import { describe, expect, it } from 'vitest';

import { applyGcsFrame, createInitialVehicleState, refreshStaleTopics } from './state-store';
import { makeSimulatedTelemetryBundle } from './simulator';

describe('state store telemetry pipeline', () => {
  it('derives vehicle state from raw Synapse telemetry frames', () => {
    let state = createInitialVehicleState('cubs2');
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
    expect(Object.keys(state.topics)).toContain('synapse/v1/topic/manual_control_command');
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

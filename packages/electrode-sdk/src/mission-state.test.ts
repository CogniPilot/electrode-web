import { describe, expect, it } from 'vitest';

import { ELECTRODE_SCHEMA_VERSION } from '@electrode/flatbuffers';
import { applyGcsFrame, createInitialVehicleState } from './state-store';
import { classify, decode } from './synapse-decode';
import type { TelemetryFrame } from './types';

// The cubs2 fixed-wing mission (generated_fixed_wing model waypoint table).
const WAYPOINTS: Array<[number, number, number]> = [
  [-4.0, -5.0, 3.0],
  [-3.0, 2.0, 3.0],
  [16.2, 2.0, 3.0],
  [16.0, -4.22, 3.0],
  [6.88, -5.1, 3.0],
  [-4.0, -5.0, 3.0]
];
const MISSION_ID = 1;
const CMD_MISSION_ITEM_LOCAL = 32001;

// Wire encoders matching the synapse_fbs 0.3.0 fixed-layout C structs the
// firmware transmits (bare struct bytes, little-endian).
function encodeMissionProgress(currentSeq: number, total: number, state: number): Uint8Array {
  const bytes = new Uint8Array(32);
  const view = new DataView(bytes.buffer);
  view.setBigUint64(0, 123_456n, true);
  view.setUint32(8, MISSION_ID, true);
  view.setUint16(20, currentSeq, true);
  view.setUint16(22, total, true);
  view.setUint8(24, state);
  return bytes;
}

function encodeLocalPositionCommand(east: number, north: number, up: number, yawRad: number): Uint8Array {
  const bytes = new Uint8Array(56);
  const view = new DataView(bytes.buffer);
  view.setBigUint64(0, 123_456n, true);
  view.setFloat32(8, east, true);
  view.setFloat32(12, north, true);
  view.setFloat32(16, up, true);
  view.setFloat32(44, yawRad, true);
  view.setUint8(54, 0); // LocalFrame.LocalEnu
  return bytes;
}

function encodeMissionItemCommand(seq: number): Uint8Array {
  const bytes = new Uint8Array(40);
  const view = new DataView(bytes.buffer);
  view.setBigUint64(0, 123_456n, true);
  const args = [seq, WAYPOINTS.length, ...WAYPOINTS[seq], MISSION_ID, 0];
  args.forEach((value, index) => view.setFloat32(8 + index * 4, value, true));
  view.setUint16(36, CMD_MISSION_ITEM_LOCAL, true);
  return bytes;
}

function frameFor(topic: string, bytes: Uint8Array, sequence: number): TelemetryFrame {
  const decoded = decode(topic, bytes);
  expect(decoded.decoded).toBe(true);
  return {
    kind: 'telemetry',
    topic,
    header: {
      sequence,
      sourceTimeNs: 10_000 * 1_000_000,
      receiveTimeNs: 10_000 * 1_000_000,
      expireTimeNs: 0,
      vehicleId: 'cubs2',
      schemaVersion: ELECTRODE_SCHEMA_VERSION,
      messageType: decoded.schema,
      priority: 'normal',
      streamId: topic
    },
    payload: decoded.payload
  };
}

describe('mission telemetry pipeline', () => {
  it('classifies the mission wire topics', () => {
    expect(classify('synapse/v1/topic/mission_progress')).toBe('MissionProgress');
    expect(classify('synapse/v1/topic/local_position_command')).toBe('LocalPositionCommand');
    expect(classify('synapse/v1/topic/vehicle_command')).toBe('VehicleCommand');
    // Must not shadow neighbouring topics.
    expect(classify('synapse/v1/topic/local_position_estimate')).toBe('Raw');
    expect(classify('synapse/v1/topic/vehicle_health')).toBe('VehicleHealth');
  });

  it('assembles the mission plan from progress, target, and item broadcasts', () => {
    let state = createInitialVehicleState('cubs2');
    let sequence = 1;

    state = applyGcsFrame(
      state,
      frameFor('synapse/v1/topic/mission_progress', encodeMissionProgress(2, WAYPOINTS.length, 2), sequence++),
      10_000
    );
    expect(state.mission).toMatchObject({
      missionId: MISSION_ID,
      currentSeq: 2,
      total: WAYPOINTS.length,
      state: 'active'
    });
    expect(state.mission?.waypoints).toHaveLength(WAYPOINTS.length);

    state = applyGcsFrame(
      state,
      frameFor(
        'synapse/v1/topic/local_position_command',
        encodeLocalPositionCommand(16.2, 2.0, 3.0, 1.25),
        sequence++
      ),
      10_000
    );
    expect(state.mission?.target?.east).toBeCloseTo(16.2, 4);
    expect(state.mission?.target?.north).toBeCloseTo(2.0, 4);
    expect(state.mission?.target?.up).toBeCloseTo(3.0, 4);
    expect(state.mission?.target?.yawRad).toBeCloseTo(1.25, 5);

    // One full revolution of the round-robin broadcast, starting mid-cycle.
    for (let i = 0; i < WAYPOINTS.length; i += 1) {
      const seq = (i + 3) % WAYPOINTS.length;
      state = applyGcsFrame(
        state,
        frameFor('synapse/v1/topic/vehicle_command', encodeMissionItemCommand(seq), sequence++),
        10_000
      );
    }

    const waypoints = state.mission?.waypoints ?? [];
    expect(waypoints.every((wp) => wp !== null)).toBe(true);
    waypoints.forEach((wp, seq) => {
      expect(wp).toMatchObject({ seq });
      expect(wp?.east).toBeCloseTo(WAYPOINTS[seq][0], 4);
      expect(wp?.north).toBeCloseTo(WAYPOINTS[seq][1], 4);
      expect(wp?.up).toBeCloseTo(WAYPOINTS[seq][2], 4);
    });
  });

  it('ignores non-mission vehicle commands and resyncs on a new mission id', () => {
    let state = createInitialVehicleState('cubs2');
    state = applyGcsFrame(
      state,
      frameFor('synapse/v1/topic/vehicle_command', encodeMissionItemCommand(0), 1),
      10_000
    );
    expect(state.mission?.waypoints[0]).toMatchObject({ seq: 0 });

    // A different producer command id must not disturb the plan.
    const other = encodeMissionItemCommand(1);
    new DataView(other.buffer).setUint16(36, 77, true);
    const decoded = decode('synapse/v1/topic/vehicle_command', other);
    state = applyGcsFrame(
      state,
      {
        ...frameFor('synapse/v1/topic/vehicle_command', encodeMissionItemCommand(1), 2),
        payload: decoded.payload
      },
      10_000
    );
    expect(state.mission?.waypoints[1]).toBeNull();

    // A new mission id clears the previously received items.
    const renumbered = encodeMissionItemCommand(1);
    new DataView(renumbered.buffer).setFloat32(28, 9, true); // arg5 = mission_id 9
    state = applyGcsFrame(
      state,
      {
        ...frameFor('synapse/v1/topic/vehicle_command', renumbered, 3),
        payload: decode('synapse/v1/topic/vehicle_command', renumbered).payload
      },
      10_000
    );
    expect(state.mission?.missionId).toBe(9);
    expect(state.mission?.waypoints[0]).toBeNull();
    expect(state.mission?.waypoints[1]).toMatchObject({ seq: 1 });
  });
});

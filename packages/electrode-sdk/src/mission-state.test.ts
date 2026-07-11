import { describe, expect, it } from 'vitest';
import { parseKey } from '@cognipilot/synapse-fbs/topic_catalog';

import { ELECTRODE_SCHEMA_VERSION } from '@electrode/flatbuffers';
import { applyGcsFrame, createInitialVehicleState } from './state-store';
import { classify, decode, expectedTopicEncoding } from './synapse-decode';
import type { TelemetryFrame } from './types';

/** Valid value-contract encoding for a catalog-keyed sample. */
function encodingFor(topic: string): string {
  return expectedTopicEncoding(parseKey(topic)!.topic);
}

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

// Wire encoders matching the synapse_fbs 0.6.0 fixed-layout structs the
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

function encodeTrajectorySegment(seq: number, trajectoryId = MISSION_ID): Uint8Array {
  const bytes = new Uint8Array(168);
  const view = new DataView(bytes.buffer);
  view.setBigUint64(0, 123_456n, true);
  const waypoint = WAYPOINTS[seq] ?? [0, 0, 0];
  view.setFloat32(16, waypoint[0], true); // p0_enu_m.x
  view.setFloat32(20, waypoint[1], true); // p0_enu_m.y
  view.setFloat32(24, waypoint[2], true); // p0_enu_m.z
  view.setUint32(144, trajectoryId, true);
  view.setUint32(148, seq, true);
  view.setUint32(156, 1, true); // plan_version
  view.setUint16(160, seq === 0 ? 1 : seq === WAYPOINTS.length - 1 ? 2 : 0, true);
  view.setUint8(162, 1); // TrajectoryType.Bezier
  view.setUint8(163, 3); // TrajectoryDegree.Cubic
  view.setUint8(164, 0); // LocalFrame.LocalEnu
  return bytes;
}

function frameFor(topic: string, bytes: Uint8Array, sequence: number): TelemetryFrame {
  const decoded = decode(topic, bytes, encodingFor(topic));
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
    expect(classify('mission')).toBe('MissionProgress');
    expect(classify('pos_sp')).toBe('LocalPositionCommand');
    expect(classify('traj')).toBe('TrajectorySegment');
    // Must not shadow neighbouring topics.
    expect(classify('local_pos')).toBe('Raw');
    expect(classify('unknown_topic')).toBe('Raw');
    expect(classify('health')).toBe('VehicleHealth');
  });

  it('assembles the mission plan from progress, target, and item broadcasts', () => {
    let state = createInitialVehicleState('cubs2');
    let sequence = 1;

    state = applyGcsFrame(
      state,
      frameFor('mission', encodeMissionProgress(2, WAYPOINTS.length, 2), sequence++),
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
        'pos_sp',
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
        frameFor('traj', encodeTrajectorySegment(seq), sequence++),
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

  it('ignores invalid trajectory segments and resyncs on a new trajectory id', () => {
    let state = createInitialVehicleState('cubs2');
    state = applyGcsFrame(
      state,
      frameFor('mission', encodeMissionProgress(0, WAYPOINTS.length, 2), 1),
      10_000
    );
    state = applyGcsFrame(
      state,
      frameFor('traj', encodeTrajectorySegment(0), 2),
      10_000
    );
    expect(state.mission?.waypoints[0]).toMatchObject({ seq: 0 });

    // Invalid coordinates must not disturb the plan.
    const other = encodeTrajectorySegment(1);
    new DataView(other.buffer).setFloat32(16, Number.NaN, true);
    const decoded = decode('traj', other, encodingFor('traj'));
    state = applyGcsFrame(
      state,
      {
        ...frameFor('traj', encodeTrajectorySegment(1), 3),
        payload: decoded.payload
      },
      10_000
    );
    expect(state.mission?.waypoints[1]).toBeNull();

    // A new trajectory id clears the previously received items.
    const renumbered = encodeTrajectorySegment(1, 9);
    state = applyGcsFrame(
      state,
      {
        ...frameFor('traj', renumbered, 4),
        payload: decode('traj', renumbered, encodingFor('traj')).payload
      },
      10_000
    );
    expect(state.mission?.missionId).toBe(9);
    expect(state.mission?.waypoints[0]).toBeNull();
    expect(state.mission?.waypoints[1]).toMatchObject({ seq: 1 });
  });
});

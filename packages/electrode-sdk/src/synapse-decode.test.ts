import { describe, expect, it } from 'vitest';
import { parseKey } from '@cognipilot/synapse-fbs/topic_catalog';

import {
  encodeCompactRigidBodyPose,
  encodeMocapFrame,
  encodeMocapPoseFrame,
  encodeRawPose
} from './mocap-encode';
import { classify, decode, expectedTopicEncoding } from './synapse-decode';

const EXTERNAL_ODOMETRY_TOPIC = parseKey('cub1/external_pose')!.topic;

describe('Synapse decoder', () => {
  it('decodes the Synapse 0.7 raw MocapPoseFrame without using an odometry estimate', () => {
    const bytes = encodeMocapPoseFrame(
      {
        position: { x: 1.25, y: -2.5, z: 3.75 },
        attitude: { x: 0, y: 0, z: 0.7071068, w: 0.7071068 }
      },
      { timestampUs: 42, frameNumber: 7, bodyId: 3, residual: 0.5 }
    );
    const topic = parseKey('qualisys/cub1/mocap')!.topic;
    const decoded = decode('qualisys/cub1/mocap', bytes, expectedTopicEncoding(topic));

    expect(decoded.schema).toBe('MocapPoseFrame');
    expect(decoded.decoded).toBe(true);
    expect(decoded.payload).toMatchObject({
      timestamp_us: 42,
      frame_number: 7,
      rigid_bodies: [{ id: 3, position: { x: 1.25, y: -2.5, z: 3.75 }, tracking_valid: true }]
    });
  });

  it('classifies known topic keys', () => {
    expect(classify('robot/manual')).toBe('ManualControl');
    expect(classify('synapse/mocap/rigid_body/cub1/pose')).toBe('MocapFrame');
    expect(classify('synapse/mocap/frame')).toBe('MocapFrame');
    expect(classify('synapse/v1/topic/mocap_frame')).toBe('MocapFrame');
    expect(classify('qualisys/cub1/pose_raw')).toBe('RawPose');
    expect(classify('qualisys/cub1/pose')).toBe('Raw');
    expect(classify('synapse/mocap/definition')).toBe('Raw');
    expect(classify('synapse/v1/topic/unknown')).toBe('Raw');
  });

  it('decodes encoded mocap FlatBuffer samples', () => {
    const bytes = encodeMocapFrame(
      {
        position: { x: 1.25, y: -2.5, z: 3.75 },
        attitude: { x: 0, y: 0, z: 0, w: 1 }
      },
      {
        frameNumber: 42,
        timestampUs: 123_456,
        bodyId: 9,
        residual: 0.01,
        trackingValid: true
      }
    );

    const decoded = decode('synapse/mocap/rigid_body/cub1/pose', bytes);

    expect(decoded.decoded).toBe(true);
    expect(decoded.schema).toBe('MocapFrame');
    expect(decoded.payload).toMatchObject({
      timestamp_us: 123_456,
      frame_number: 42,
      rigid_bodies: [
        {
          id: 9,
          position: { x: 1.25, y: -2.5, z: 3.75 },
          attitude: { x: 0, y: 0, z: 0, w: 1 },
          tracking_valid: true
        }
      ]
    });
  });

  it('falls back to a raw payload preview for unknown topics', () => {
    const decoded = decode('synapse/v1/topic/not_yet_supported', new Uint8Array([0, 1, 2, 255]));

    expect(decoded).toEqual({
      schema: 'Raw',
      decoded: false,
      payload: { bytes: 4, hexPreview: '000102ff' }
    });
  });

  it('decodes the bridge raw pose fixed-layout payload', () => {
    const bytes = encodeRawPose(
      { position: { x: 1, y: 2, z: 3 }, attitude: { w: 1, x: 0, y: 0, z: 0 } },
      123_456
    );
    const topic = parseKey('qualisys/cub1/pose_raw')!.topic;
    const decoded = decode('qualisys/cub1/pose_raw', bytes, expectedTopicEncoding(topic));

    expect(decoded.decoded).toBe(true);
    expect(decoded.schema).toBe('RawPose');
    expect(decoded.payload).toMatchObject({
      data: {
        timestamp_us: 123_456,
        position: { x: 1, y: 2, z: 3 },
        attitude: { w: 1, x: 0, y: 0, z: 0 }
      }
    });
  });
});

describe('Value contract enforcement', () => {
  const MOCAP_TOPIC = parseKey('mocap')!.topic;

  function externalOdometryBytes(): Uint8Array {
    const bytes = new Uint8Array(64);
    const view = new DataView(bytes.buffer);
    view.setBigUint64(0, 42n, true);
    view.setFloat32(20, 1, true); // attitude.w
    return bytes;
  }

  it('accepts a struct topic carrying the exact struct encoding', () => {
    const decoded = decode(
      'external_pose/1',
      externalOdometryBytes(),
      expectedTopicEncoding(EXTERNAL_ODOMETRY_TOPIC)
    );

    expect(decoded.decoded).toBe(true);
    expect(decoded.schema).toBe('ExternalOdometry');
  });

  it('accepts the zenoh-pico byte wrapper around an exact struct encoding', () => {
    const decoded = decode(
      'external_pose/1',
      externalOdometryBytes(),
      `zenoh/bytes;${expectedTopicEncoding(EXTERNAL_ODOMETRY_TOPIC)}`
    );

    expect(decoded.decoded).toBe(true);
    expect(decoded.schema).toBe('ExternalOdometry');
  });

  it('accepts a root-table topic carrying the exact flatbuffers encoding', () => {
    const bytes = encodeMocapPoseFrame(
      { position: { x: 1, y: 2, z: 3 }, attitude: { x: 0, y: 0, z: 0, w: 1 } },
      { frameNumber: 3, timestampUs: 77, bodyId: 1 }
    );

    const decoded = decode('mocap', bytes, expectedTopicEncoding(MOCAP_TOPIC));

    expect(decoded.decoded).toBe(true);
    expect(decoded.schema).toBe('MocapPoseFrame');
    expect(decoded.payload).toMatchObject({ frame_number: 3, timestamp_us: 77 });
  });

  it('rejects a catalog-keyed sample with no encoding', () => {
    const decoded = decode('external_pose/1', externalOdometryBytes());

    expect(decoded.decoded).toBe(false);
    expect(decoded.schema).toBe('ExternalOdometry');
    expect((decoded.payload as { contractError?: string }).contractError).toMatch(/missing encoding/);
  });

  it('rejects a catalog-keyed sample with a mismatched wire type', () => {
    const wrongType = expectedTopicEncoding(MOCAP_TOPIC);
    const decoded = decode('external_pose/1', externalOdometryBytes(), wrongType);

    expect(decoded.decoded).toBe(false);
    expect((decoded.payload as { contractError?: string }).contractError).toMatch(/encoding mismatch/);
  });

  it('rejects a catalog-keyed sample with a mismatched schema hash', () => {
    const staleHash = expectedTopicEncoding(EXTERNAL_ODOMETRY_TOPIC).replace(
      /schema=sha256-128:.*/,
      'schema=sha256-128:00000000000000000000000000000000'
    );
    const decoded = decode('external_pose/1', externalOdometryBytes(), staleHash);

    expect(decoded.decoded).toBe(false);
    expect((decoded.payload as { contractError?: string }).contractError).toMatch(/encoding mismatch/);
  });

  it('exempts custom non-catalog keys from the encoding contract', () => {
    const bytes = encodeCompactRigidBodyPose({
      position: { x: 1, y: 2, z: 3 },
      attitude: { x: 0, y: 0, z: 0, w: 1 }
    });

    const decoded = decode('synapse/mocap/rigid_body/cub1/pose', bytes);

    expect(decoded.decoded).toBe(true);
    expect(decoded.schema).toBe('MocapFrame');
  });
});

describe('Mocap wire contract', () => {
  it('decodes the compact 28-byte pose exactly as synapse_qualisys_bridge encodes it', () => {
    // Hand-built wire payload — 7 little-endian f32 values
    // [px, py, pz, qx, qy, qz, qw], quaternion scalar (w) LAST. This layout is
    // the synapse_qualisys_bridge contract; it must never be read w-first.
    const bytes = new Uint8Array(28);
    const view = new DataView(bytes.buffer);
    [1.5, -2.25, 0.75, 0.1, -0.2, 0.55, 0.8].forEach((value, index) =>
      view.setFloat32(index * 4, value, true)
    );

    const decoded = decode('synapse/mocap/rigid_body/cub1/pose', bytes);

    expect(decoded.decoded).toBe(true);
    expect(decoded.schema).toBe('MocapFrame');
    const body = (decoded.payload as { rigid_bodies: Array<Record<string, unknown>> })
      .rigid_bodies[0];
    expect(body.position).toMatchObject({ x: 1.5, y: -2.25, z: 0.75 });
    const attitude = body.attitude as { x: number; y: number; z: number; w: number };
    expect(attitude.x).toBeCloseTo(0.1, 6);
    expect(attitude.y).toBeCloseTo(-0.2, 6);
    expect(attitude.z).toBeCloseTo(0.55, 6);
    expect(attitude.w).toBeCloseTo(0.8, 6);
  });

  it('round-trips the compact pose encoder through the decoder', () => {
    const pose = {
      position: { x: 4.5, y: 5.5, z: 6.5 },
      attitude: { x: 0.25, y: -0.5, z: 0.125, w: 0.75 }
    };
    const bytes = encodeCompactRigidBodyPose(pose);
    expect(bytes.length).toBe(28);

    const decoded = decode('synapse/mocap/rigid_body/cub1/pose', bytes);
    const body = (decoded.payload as { rigid_bodies: Array<Record<string, unknown>> })
      .rigid_bodies[0];
    expect(body.position).toMatchObject(pose.position);
    expect(body.attitude).toMatchObject(pose.attitude);
  });

  it('decodes raw MocapFrame FlatBuffers on the Qualisys bridge topic', () => {
    const bytes = encodeMocapFrame(
      { position: { x: 1, y: 2, z: 3 }, attitude: { x: 0, y: 0, z: 0, w: 1 } },
      { frameNumber: 7, timestampUs: 99, bodyId: 1 }
    );

    const decoded = decode('synapse/v1/topic/mocap_frame', bytes);

    expect(decoded.decoded).toBe(true);
    expect(decoded.schema).toBe('MocapFrame');
    expect(decoded.payload).toMatchObject({ frame_number: 7, timestamp_us: 99 });
  });
});

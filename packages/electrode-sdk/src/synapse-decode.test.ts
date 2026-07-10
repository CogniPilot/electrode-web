import { describe, expect, it } from 'vitest';

import {
  encodeCompactRigidBodyPose,
  encodeMocapFrame
} from './mocap-encode';
import { classify, decode } from './synapse-decode';

describe('Synapse decoder', () => {
  it('classifies known topic keys', () => {
    expect(classify('robot/synapse/v1/topic/manual_control_command')).toBe('ManualControl');
    expect(classify('synapse/mocap/rigid_body/cub1/pose')).toBe('MocapFrame');
    expect(classify('synapse/mocap/frame')).toBe('MocapFrame');
    expect(classify('synapse/v1/topic/external_odometry/1')).toBe('ExternalOdometry');
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

  it('decodes the bridge external odometry fixed-layout payload', () => {
    const bytes = new Uint8Array(64);
    const view = new DataView(bytes.buffer);
    view.setBigUint64(0, 123_456n, true);
    [1, 2, 3].forEach((value, index) => view.setFloat32(8 + index * 4, value, true));
    [1, 0, 0, 0].forEach((value, index) => view.setFloat32(20 + index * 4, value, true));
    [4, 5, 6].forEach((value, index) => view.setFloat32(36 + index * 4, value, true));
    [0.1, 0.2, 0.3].forEach((value, index) => view.setFloat32(48 + index * 4, value, true));
    view.setUint8(60, 0x0f);
    view.setUint8(61, 1);
    view.setUint8(62, 1);
    view.setUint8(63, 1);

    const decoded = decode('synapse/v1/topic/external_odometry/1', bytes);

    expect(decoded.decoded).toBe(true);
    expect(decoded.schema).toBe('ExternalOdometry');
    expect(decoded.payload).toMatchObject({
      data: {
        timestamp_us: 123_456,
        position: { x: 1, y: 2, z: 3 },
        attitude: { w: 1, x: 0, y: 0, z: 0 },
        linear_velocity: { x: 4, y: 5, z: 6 },
        status: 'Filtered',
        source_id: 1,
        id: 1,
        position_valid: true,
        attitude_valid: true,
        linear_velocity_valid: true,
        angular_velocity_valid: true
      }
    });
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

  it('decodes full MocapFrame FlatBuffers on the synapse/mocap/frame topic', () => {
    const bytes = encodeMocapFrame(
      { position: { x: 1, y: 2, z: 3 }, attitude: { x: 0, y: 0, z: 0, w: 1 } },
      { frameNumber: 7, timestampUs: 99, bodyId: 1 }
    );

    const decoded = decode('synapse/mocap/frame', bytes);

    expect(decoded.decoded).toBe(true);
    expect(decoded.schema).toBe('MocapFrame');
    expect(decoded.payload).toMatchObject({ frame_number: 7, timestamp_us: 99 });
  });
});

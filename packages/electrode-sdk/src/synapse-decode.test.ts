import { describe, expect, it } from 'vitest';

import { encodeMocapFrame } from './mocap-encode';
import { classify, decode } from './synapse-decode';

describe('Synapse decoder', () => {
  it('classifies known topic keys', () => {
    expect(classify('robot/synapse/v1/topic/manual_control_command')).toBe('ManualControl');
    expect(classify('synapse/mocap/rigid_body/cub1/pose')).toBe('MocapFrame');
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
});

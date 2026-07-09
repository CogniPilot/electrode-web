// Encoders for the Synapse mocap wire contract, byte-identical to what the
// Qualisys bridge (synapse_qualisys_bridge) publishes:
//   - `synapse/mocap/frame`                    — MocapFrame FlatBuffer
//   - `synapse/mocap/rigid_body/<name>/pose`   — compact 28-byte pose
// The in-browser rumoca sim emits MocapFrame on a private topic; the Ground
// Station normalizes it onto the public topics above so simulation traffic is
// indistinguishable from a real mocap bridge.
import * as flatbuffers from 'flatbuffers';

import { MocapFrame } from './generated/synapse/topic/mocap-frame.js';
import { MocapRawComponent } from './generated/synapse/topic/mocap-raw-component.js';
import { MocapRawFlags } from './generated/synapse/topic/mocap-raw-flags.js';
import { MocapRigidBodyData } from './generated/synapse/topic/mocap-rigid-body-data.js';

export interface MocapPose {
  /** Rigid-body position, ENU metres (x=east, y=north, z=up). */
  position: { x: number; y: number; z: number };
  /** Attitude quaternion {x, y, z, w}. */
  attitude: { x: number; y: number; z: number; w: number };
}

export interface MocapFrameOptions {
  frameNumber?: number;
  timestampUs?: number;
  bodyId?: number;
  residual?: number;
  trackingValid?: boolean;
}

/** Serialize a single rigid-body pose as a `synapse.topic.MocapFrame`. */
export function encodeMocapFrame(pose: MocapPose, options: MocapFrameOptions = {}): Uint8Array {
  const builder = new flatbuffers.Builder(256);
  MocapFrame.startRigidBodiesVector(builder, 1);
  const rotation = quaternionToRotationMatrix(pose.attitude);
  const flags =
    (options.trackingValid ?? true ? MocapRawFlags.Valid : 0) |
    MocapRawFlags.ResidualValid |
    MocapRawFlags.LabelValid;
  MocapRigidBodyData.createMocapRigidBodyData(
    builder,
    pose.position.x,
    pose.position.y,
    pose.position.z,
    rotation.r11,
    rotation.r12,
    rotation.r13,
    rotation.r21,
    rotation.r22,
    rotation.r23,
    rotation.r31,
    rotation.r32,
    rotation.r33,
    options.residual ?? 0,
    options.bodyId ?? 0,
    flags,
    MocapRawComponent.RigidBody6d
  );
  const bodies = builder.endVector();
  const message = MocapFrame.createMocapFrame(
    builder,
    BigInt(options.timestampUs ?? 0),
    options.frameNumber ?? 0,
    0,
    0,
    0,
    flags,
    0,
    0,
    bodies
  );
  builder.finish(message);
  return builder.asUint8Array();
}

function quaternionToRotationMatrix(quaternion: MocapPose['attitude']): {
  r11: number;
  r12: number;
  r13: number;
  r21: number;
  r22: number;
  r23: number;
  r31: number;
  r32: number;
  r33: number;
} {
  const norm = Math.hypot(quaternion.w, quaternion.x, quaternion.y, quaternion.z);
  const scale = Number.isFinite(norm) && norm > 0 ? 1 / norm : 1;
  const w = quaternion.w * scale;
  const x = quaternion.x * scale;
  const y = quaternion.y * scale;
  const z = quaternion.z * scale;
  return {
    r11: 1 - 2 * (y * y + z * z),
    r12: 2 * (x * y - z * w),
    r13: 2 * (x * z + y * w),
    r21: 2 * (x * y + z * w),
    r22: 1 - 2 * (x * x + z * z),
    r23: 2 * (y * z - x * w),
    r31: 2 * (x * z - y * w),
    r32: 2 * (y * z + x * w),
    r33: 1 - 2 * (x * x + y * y)
  };
}

/**
 * Serialize a pose as the compact 28-byte per-rigid-body payload published on
 * `synapse/mocap/rigid_body/<name>/pose`: little-endian f32
 * `[px, py, pz, qx, qy, qz, qw]` — ENU metres, quaternion scalar (w) last.
 */
export function encodeCompactRigidBodyPose(pose: MocapPose): Uint8Array {
  const bytes = new Uint8Array(28);
  const view = new DataView(bytes.buffer);
  const values = [
    pose.position.x,
    pose.position.y,
    pose.position.z,
    pose.attitude.x,
    pose.attitude.y,
    pose.attitude.z,
    pose.attitude.w
  ];
  values.forEach((value, index) => view.setFloat32(index * 4, value, true));
  return bytes;
}

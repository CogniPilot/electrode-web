// Encoders for the Synapse mocap wire contract, byte-identical to what the
// Qualisys bridge (synapse_qualisys_bridge) publishes:
//   - `synapse/mocap/frame`                    — MocapFrame FlatBuffer
//   - `synapse/mocap/rigid_body/<name>/pose`   — compact 28-byte pose
//   - `synapse/mocap/definition`               — MocapDefinition FlatBuffer
// The in-browser rumoca sim emits MocapFrame on a private topic; the Ground
// Station normalizes it onto the public topics above so simulation traffic is
// indistinguishable from a real mocap bridge.
import * as flatbuffers from 'flatbuffers';

import { MocapDefinition } from './generated/synapse/topic/mocap-definition.js';
import { MocapFrame } from './generated/synapse/topic/mocap-frame.js';
import { MocapRigidBodyDefinition } from './generated/synapse/topic/mocap-rigid-body-definition.js';
import { MocapRigidBodySample } from './generated/synapse/topic/mocap-rigid-body-sample.js';

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
  MocapRigidBodySample.createMocapRigidBodySample(
    builder,
    options.bodyId ?? 0,
    pose.position.x,
    pose.position.y,
    pose.position.z,
    pose.attitude.w,
    pose.attitude.x,
    pose.attitude.y,
    pose.attitude.z,
    options.residual ?? 0,
    options.trackingValid ?? true
  );
  const bodies = builder.endVector();
  const message = MocapFrame.createMocapFrame(
    builder,
    BigInt(options.timestampUs ?? 0),
    options.frameNumber ?? 0,
    0,
    0,
    bodies,
    0
  );
  builder.finish(message);
  return builder.asUint8Array();
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

export interface MocapDefinitionOptions {
  /** Mocap system or producer name, e.g. `electrode-sim`. */
  source: string;
  /** Frame name for the mocap ENU coordinate system. */
  frameId: string;
  /** Tracked rigid bodies (ids match MocapRigidBodySample.id). */
  rigidBodies: Array<{ id: number; name: string }>;
}

/** Serialize the cached `synapse/mocap/definition` metadata packet. */
export function encodeMocapDefinition(options: MocapDefinitionOptions): Uint8Array {
  const builder = new flatbuffers.Builder(256);
  const bodyOffsets = options.rigidBodies.map((body) => {
    const name = builder.createString(body.name);
    MocapRigidBodyDefinition.startMocapRigidBodyDefinition(builder);
    MocapRigidBodyDefinition.addId(builder, body.id);
    MocapRigidBodyDefinition.addName(builder, name);
    return MocapRigidBodyDefinition.endMocapRigidBodyDefinition(builder);
  });
  const bodies = MocapDefinition.createRigidBodiesVector(builder, bodyOffsets);
  const source = builder.createString(options.source);
  const frameId = builder.createString(options.frameId);
  MocapDefinition.startMocapDefinition(builder);
  MocapDefinition.addSource(builder, source);
  MocapDefinition.addFrameId(builder, frameId);
  MocapDefinition.addRigidBodies(builder, bodies);
  builder.finish(MocapDefinition.endMocapDefinition(builder));
  return builder.asUint8Array();
}

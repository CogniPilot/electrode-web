// Encode a MocapFrame FlatBuffer for publishing over Zenoh — the vehicle-pose
// message real hardware (and the in-browser rumoca sim) emit on
// `synapse/mocap/rigid_body/cub1/pose`. Mirrors the schema-faithful shape the decoder reads.
import * as flatbuffers from 'flatbuffers';

import { MocapFrame } from './generated/synapse/topic/mocap-frame.js';
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

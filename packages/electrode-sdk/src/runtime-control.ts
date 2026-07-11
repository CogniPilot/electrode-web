import * as flatbuffers from 'flatbuffers';

import { ParamKind } from './generated/synapse/cmd/param-kind.js';
import { ParamGetReply } from './generated/synapse/cmd/param-get-reply.js';
import { ParamGetRequest } from './generated/synapse/cmd/param-get-request.js';
import { ParamSetRequest } from './generated/synapse/cmd/param-set-request.js';
import { ParamValue } from './generated/synapse/cmd/param-value.js';
import { TrajectorySetRequest } from './generated/synapse/cmd/trajectory-set-request.js';
import { TrajectoryDegree } from './generated/synapse/topic/trajectory-degree.js';
import { TrajectorySegmentData } from './generated/synapse/topic/trajectory-segment-data.js';
import { TrajectoryType } from './generated/synapse/topic/trajectory-type.js';
import { LocalFrame } from './generated/synapse/types/local-frame.js';

export interface RuntimeWaypoint {
  east: number;
  north: number;
  up: number;
}

export function encodeRuntimeParameter(name: string, value: number): Uint8Array {
  const builder = new flatbuffers.Builder(256);
  const nameOffset = builder.createString(name);
  const parameter = ParamValue.createParamValue(builder, nameOffset, ParamKind.Float, value, 0n, 0);
  const request = ParamSetRequest.createParamSetRequest(builder, parameter);
  builder.finish(request);
  return builder.asUint8Array();
}

export function encodeRuntimeParameterGet(name: string): Uint8Array {
  const builder = new flatbuffers.Builder(128);
  const nameOffset = builder.createString(name);
  const request = ParamGetRequest.createParamGetRequest(builder, nameOffset, 0, 1);
  builder.finish(request);
  return builder.asUint8Array();
}

export function decodeRuntimeParameterReply(bytes: Uint8Array): { name: string; value: number } | null {
  const reply = ParamGetReply.getRootAsParamGetReply(new flatbuffers.ByteBuffer(bytes));
  if (reply.valuesLength() !== 1) return null;
  const value = reply.values(0);
  const name = value?.name();
  if (!value || typeof name !== 'string' || value.kind() !== ParamKind.Float) return null;
  return { name, value: value.floatValue() };
}

export function encodeRuntimeTrajectory(waypoints: RuntimeWaypoint[], planVersion = 1): Uint8Array {
  if (waypoints.length < 2 || waypoints.length > 7) {
    throw new Error('trajectory requires 2 to 7 waypoints');
  }
  if (waypoints.some((point) => ![point.east, point.north, point.up].every(Number.isFinite))) {
    throw new Error('waypoint coordinates must be finite');
  }

  const builder = new flatbuffers.Builder(2048);
  const segmentCount = waypoints.length - 1;
  TrajectorySetRequest.startSegmentsVector(builder, segmentCount);
  for (let index = segmentCount - 1; index >= 0; index--) {
    const start = waypoints[index];
    const end = waypoints[index + 1];
    TrajectorySegmentData.createTrajectorySegmentData(
      builder,
      0n,
      0n,
      start.east,
      start.north,
      start.up,
      end.east,
      end.north,
      end.up,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      0,
      1,
      index,
      0,
      planVersion,
      0,
      TrajectoryType.Bezier,
      TrajectoryDegree.Cubic,
      LocalFrame.LocalEnu,
      0
    );
  }
  const segments = builder.endVector();
  const request = TrajectorySetRequest.createTrajectorySetRequest(
    builder,
    1,
    0,
    planVersion,
    0,
    segmentCount,
    0,
    0,
    segments
  );
  builder.finish(request);
  return builder.asUint8Array();
}

import {
  ELECTRODE_GCS_LOG_SCHEMA_ASSET,
  decodeBase64Bytes
} from '@electrode/flatbuffers';
import { Builder, ByteBuffer } from 'flatbuffers';
import { McapStreamReader, McapWriter, hasMcapPrefix, type IWritable, type TypedMcapRecords } from '@mcap/core';
import { decode as decodeSynapseSample } from './synapse-decode';
import { resolveTopicDefinition } from './topics';
import type {
  Attitude,
  Battery,
  EventFrame,
  EventMessage,
  GcsFrame,
  LinkStatus,
  LocalizationState,
  MessageHeader,
  ModeState,
  Pose,
  Priority,
  Severity,
  TelemetryFrame,
  Velocity
} from './types';

type Offset = number;

interface EncodedPayload {
  type: GcsPayload;
  offset: Offset;
}

interface TopicRecordState {
  topic: string;
}

interface McapFrameRecord {
  topic: string;
  payload: Uint8Array;
  sequence: number;
  logTimeNs: bigint;
  publishTimeNs: bigint;
}

export interface SynapseLogExport {
  bytes: Uint8Array;
  filename: string;
  mimeType: string;
  frameCount: number;
}

export interface SynapseLogRecorderOptions {
  vehicleId: string;
  source: string;
  description?: string;
  createdUnixUs?: bigint;
}

const MCAP_LOG_MIME_TYPE = 'application/mcap';
const MCAP_PROFILE = 'synapse';
const MCAP_LIBRARY = 'electrode-web';
const MCAP_SCHEMA_ENCODING = 'flatbuffer';
const MCAP_MESSAGE_ENCODING = 'flatbuffer';
const ELECTRODE_GCS_FILE_IDENTIFIER = 'EGCS';
const ELECTRODE_GCS_SCHEMA_NAME = ELECTRODE_GCS_LOG_SCHEMA_ASSET.rootType;

enum GcsPayload {
  None = 0,
  Pose = 1,
  Velocity = 2,
  Attitude = 3,
  Battery = 4,
  LinkStatus = 5,
  ModeState = 6,
  LocalizationState = 7,
  Event = 8,
  TelemetryJson = 9
}

enum CommonPriority {
  Low = 0,
  Normal = 1,
  High = 2,
  Critical = 3
}

enum EventSeverity {
  Info = 0,
  Warning = 1,
  Error = 2
}

export class SynapseLogRecorder {
  readonly createdUnixUs: bigint;
  readonly source: string;
  readonly description: string;

  #frameRecords: McapFrameRecord[] = [];
  #topics = new Map<string, TopicRecordState>();
  #frameCount = 0;

  constructor(options: SynapseLogRecorderOptions) {
    this.createdUnixUs = options.createdUnixUs ?? unixNowUs();
    this.source = options.source;
    this.description = options.description ?? `electrode ${options.vehicleId} ground-station log`;
  }

  get frameCount(): number {
    return this.#frameCount;
  }

  recordFrame(frame: GcsFrame): boolean {
    const payload = encodeGcsFramePayload(frame);
    if (!payload) {
      return false;
    }

    const topic = this.#ensureTopic(frame.topic);
    const timestamp = this.#frameTimestamp(frame);
    this.#frameRecords.push({
      topic: topic.topic,
      payload,
      sequence: frame.header.sequence,
      logTimeNs: timestamp.logTimeNs,
      publishTimeNs: timestamp.publishTimeNs
    });
    this.#frameCount += 1;
    return true;
  }

  async export(filenamePrefix = 'electrode-log'): Promise<SynapseLogExport> {
    const writable = new MemoryMcapWriter();
    const writer = new McapWriter({ writable, useChunks: true, useMessageIndex: true });
    await writer.start({ profile: MCAP_PROFILE, library: MCAP_LIBRARY });
    const schemaId = await registerGcsSchema(writer);
    const channels = await this.#registerChannels(writer, schemaId);
    await this.#writeMetadata(writer);
    await this.#writeFrames(writer, channels);
    await writer.end();
    const createdMs = Number(this.createdUnixUs / 1000n);
    const timestamp = new Date(createdMs).toISOString().replaceAll(':', '-');
    return {
      bytes: writable.bytes(),
      filename: `${filenamePrefix}-${timestamp}.mcap`,
      mimeType: MCAP_LOG_MIME_TYPE,
      frameCount: this.#frameCount
    };
  }

  #ensureTopic(topic: string): TopicRecordState {
    const current = this.#topics.get(topic);
    if (current) {
      return current;
    }

    const next = {
      topic
    };
    this.#topics.set(topic, next);
    return next;
  }

  #frameTimestamp(frame: GcsFrame): Pick<McapFrameRecord, 'logTimeNs' | 'publishTimeNs'> {
    const fallbackNs = this.createdUnixUs * 1000n + BigInt(this.#frameCount);
    const logTimeNs = nsNumberToOptionalBigInt(frame.header.sourceTimeNs) ?? fallbackNs;
    const publishTimeNs = nsNumberToOptionalBigInt(frame.header.receiveTimeNs) ?? logTimeNs;
    return { logTimeNs, publishTimeNs };
  }

  async #registerChannels(writer: McapWriter, schemaId: number): Promise<Map<string, number>> {
    const channels = new Map<string, number>();
    const topics = [...this.#topics.values()].map((topic) => topic.topic).sort();
    for (const topic of topics) {
      const channelId = await writer.registerChannel({
        schemaId,
        topic,
        messageEncoding: MCAP_MESSAGE_ENCODING,
        metadata: gcsChannelMetadata()
      });
      channels.set(topic, channelId);
    }
    return channels;
  }

  async #writeMetadata(writer: McapWriter): Promise<void> {
    await writer.addMetadata({
      name: 'electrode.recording',
      metadata: new Map([
        ['source', this.source],
        ['description', this.description],
        ['created_unix_us', this.createdUnixUs.toString()],
        ['payload_schema', ELECTRODE_GCS_SCHEMA_NAME]
      ])
    });
  }

  async #writeFrames(writer: McapWriter, channels: Map<string, number>): Promise<void> {
    for (const record of this.#frameRecords) {
      const channelId = channels.get(record.topic);
      if (channelId === undefined) {
        continue;
      }
      await writer.addMessage({
        channelId,
        sequence: record.sequence,
        logTime: record.logTimeNs,
        publishTime: record.publishTimeNs,
        data: record.payload
      });
    }
  }
}

class MemoryMcapWriter implements IWritable {
  #chunks: Uint8Array[] = [];
  #position = 0n;

  async write(buffer: Uint8Array): Promise<void> {
    this.#chunks.push(buffer.slice());
    this.#position += BigInt(buffer.byteLength);
  }

  position(): bigint {
    return this.#position;
  }

  bytes(): Uint8Array {
    return concatBytes(this.#chunks);
  }
}

async function registerGcsSchema(writer: McapWriter): Promise<number> {
  return writer.registerSchema({
    name: ELECTRODE_GCS_SCHEMA_NAME,
    encoding: MCAP_SCHEMA_ENCODING,
    data: decodeBase64Bytes(ELECTRODE_GCS_LOG_SCHEMA_ASSET.bfbsBase64)
  });
}

function gcsChannelMetadata(): Map<string, string> {
  return new Map([
    ['schema_name', ELECTRODE_GCS_LOG_SCHEMA_ASSET.name],
    ['root_type', ELECTRODE_GCS_SCHEMA_NAME],
    ['file_identifier', ELECTRODE_GCS_FILE_IDENTIFIER],
    ['fbs_sha256', ELECTRODE_GCS_LOG_SCHEMA_ASSET.fbsSha256]
  ]);
}

function hasMcapLogPrefix(bytes: Uint8Array): boolean {
  if (bytes.byteLength < 8) {
    return false;
  }
  const prefix = new DataView(bytes.buffer, bytes.byteOffset, 8);
  return hasMcapPrefix(prefix);
}

export function decodeSynapseLogFrames(bytes: Uint8Array): GcsFrame[] {
  if (!hasMcapLogPrefix(bytes)) {
    throw new Error('Invalid MCAP log file');
  }
  return decodeMcapLogFrames(bytes);
}

function decodeMcapLogFrames(bytes: Uint8Array): GcsFrame[] {
  const reader = new McapStreamReader({ validateCrcs: false });
  const channels = new Map<number, TypedMcapRecords['Channel']>();
  const schemas = new Map<number, TypedMcapRecords['Schema']>();
  const frames: GcsFrame[] = [];

  reader.append(bytes);
  for (let record = reader.nextRecord(); record; record = reader.nextRecord()) {
    if (record.type === 'Schema') {
      schemas.set(record.id, record);
    } else if (record.type === 'Channel') {
      channels.set(record.id, record);
    } else if (record.type === 'Message') {
      const frame = decodeMcapMessage(record, channels, schemas);
      if (frame) {
        frames.push(frame);
      }
    }
  }

  return frames;
}

function decodeMcapMessage(
  message: TypedMcapRecords['Message'],
  channels: Map<number, TypedMcapRecords['Channel']>,
  schemas: Map<number, TypedMcapRecords['Schema']>
): GcsFrame | null {
  const channel = channels.get(message.channelId);
  if (!channel || channel.messageEncoding !== MCAP_MESSAGE_ENCODING) {
    return null;
  }

  const schema = schemas.get(channel.schemaId);
  if (schema?.name === ELECTRODE_GCS_SCHEMA_NAME) {
    return decodeGcsFramePayload(message.data);
  }

  return decodeNativeSynapseMcapMessage(channel.topic, message);
}

function decodeNativeSynapseMcapMessage(topic: string, message: TypedMcapRecords['Message']): GcsFrame | null {
  const decoded = decodeSynapseSample(topic, message.data);
  if (!decoded.decoded) {
    return null;
  }

  return telemetryFrame(topic, {
    sequence: message.sequence,
    sourceTimeNs: Number(message.logTime),
    receiveTimeNs: Number(message.publishTime),
    expireTimeNs: 0,
    vehicleId: 'replay',
    schemaVersion: 0,
    messageType: decoded.schema,
    priority: 'normal',
    streamId: topic
  }, decoded.payload);
}

function encodeGcsFramePayload(frame: GcsFrame): Uint8Array | null {
  const builder = new Builder(1024);
  const topicOffset = builder.createString(frame.topic);
  const payload = createGcsPayload(builder, frame);
  if (!payload) {
    return null;
  }

  builder.startObject(3);
  builder.addFieldOffset(0, topicOffset, 0);
  builder.addFieldInt8(1, payload.type, GcsPayload.None);
  builder.addFieldOffset(2, payload.offset, 0);
  const root = builder.endObject();
  builder.finish(root, ELECTRODE_GCS_FILE_IDENTIFIER);
  return builder.asUint8Array().slice();
}

function createGcsPayload(builder: Builder, frame: GcsFrame): EncodedPayload | null {
  if (frame.kind === 'event') {
    return createEventPayload(builder, frame);
  }

  const telemetry = frame as TelemetryFrame;
  const schema = resolveTopicDefinition(frame.topic)?.schema ?? frame.header.messageType;

  if (schema === 'Pose') {
    const payload = telemetry.payload as Pose;
    const headerOffset = createHeader(builder, telemetry.header);
    builder.startObject(7);
    builder.addFieldOffset(0, headerOffset, 0);
    builder.addFieldFloat64(1, payload.lat, 0);
    builder.addFieldFloat64(2, payload.lon, 0);
    builder.addFieldFloat32(3, payload.altM, 0);
    builder.addFieldFloat32(4, payload.xM, 0);
    builder.addFieldFloat32(5, payload.yM, 0);
    builder.addFieldFloat32(6, payload.zM, 0);
    return { type: GcsPayload.Pose, offset: builder.endObject() };
  }

  if (schema === 'Velocity') {
    const payload = telemetry.payload as Velocity;
    const headerOffset = createHeader(builder, telemetry.header);
    builder.startObject(5);
    builder.addFieldOffset(0, headerOffset, 0);
    builder.addFieldFloat32(1, payload.northMps, 0);
    builder.addFieldFloat32(2, payload.eastMps, 0);
    builder.addFieldFloat32(3, payload.downMps, 0);
    builder.addFieldFloat32(4, payload.groundSpeedMps, 0);
    return { type: GcsPayload.Velocity, offset: builder.endObject() };
  }

  if (schema === 'Attitude') {
    const payload = telemetry.payload as Attitude;
    const headerOffset = createHeader(builder, telemetry.header);
    builder.startObject(4);
    builder.addFieldOffset(0, headerOffset, 0);
    builder.addFieldFloat32(1, payload.rollDeg, 0);
    builder.addFieldFloat32(2, payload.pitchDeg, 0);
    builder.addFieldFloat32(3, payload.yawDeg, 0);
    return { type: GcsPayload.Attitude, offset: builder.endObject() };
  }

  if (schema === 'Battery') {
    const payload = telemetry.payload as Battery;
    const headerOffset = createHeader(builder, telemetry.header);
    builder.startObject(4);
    builder.addFieldOffset(0, headerOffset, 0);
    builder.addFieldFloat32(1, payload.voltageV, 0);
    builder.addFieldFloat32(2, payload.currentA, 0);
    builder.addFieldFloat32(3, payload.remainingPct, 0);
    return { type: GcsPayload.Battery, offset: builder.endObject() };
  }

  if (schema === 'LinkStatus') {
    const payload = telemetry.payload as LinkStatus;
    const headerOffset = createHeader(builder, telemetry.header);
    builder.startObject(4);
    builder.addFieldOffset(0, headerOffset, 0);
    builder.addFieldFloat32(1, payload.rssiDbm, 0);
    builder.addFieldFloat32(2, payload.latencyMs, 0);
    builder.addFieldFloat32(3, payload.packetLossPct, 0);
    return { type: GcsPayload.LinkStatus, offset: builder.endObject() };
  }

  if (schema === 'ModeState') {
    const payload = telemetry.payload as ModeState;
    const headerOffset = createHeader(builder, telemetry.header);
    const nameOffset = builder.createString(payload.name);
    builder.startObject(4);
    builder.addFieldOffset(0, headerOffset, 0);
    builder.addFieldOffset(1, nameOffset, 0);
    builder.addFieldInt8(2, payload.armed ? 1 : 0, 0);
    builder.addFieldInt8(3, payload.failsafe ? 1 : 0, 0);
    return { type: GcsPayload.ModeState, offset: builder.endObject() };
  }

  if (schema === 'LocalizationState') {
    const payload = telemetry.payload as LocalizationState;
    const headerOffset = createHeader(builder, telemetry.header);
    const sourceOffset = builder.createString(payload.source);
    builder.startObject(4);
    builder.addFieldOffset(0, headerOffset, 0);
    builder.addFieldOffset(1, sourceOffset, 0);
    builder.addFieldInt8(2, payload.fresh ? 1 : 0, 0);
    builder.addFieldFloat32(3, payload.quality, 0);
    return { type: GcsPayload.LocalizationState, offset: builder.endObject() };
  }

  // Anything else (raw Synapse wire topics: MocapFrame, ManualControl,
  // PwmSignalOutputs, AttitudeEstimate, ...) is recorded as JSON so no
  // stream is dropped; the header's message_type names the schema.
  let json: string;
  try {
    json = JSON.stringify(telemetry.payload ?? null);
  } catch {
    return null;
  }

  const headerOffset = createHeader(builder, telemetry.header);
  const jsonOffset = builder.createString(json);
  builder.startObject(2);
  builder.addFieldOffset(0, headerOffset, 0);
  builder.addFieldOffset(1, jsonOffset, 0);
  return { type: GcsPayload.TelemetryJson, offset: builder.endObject() };
}

function createEventPayload(builder: Builder, frame: EventFrame): EncodedPayload {
  const payload = frame.payload;
  const headerOffset = createHeader(builder, frame.header);
  const codeOffset = builder.createString(payload.code);
  const messageOffset = builder.createString(payload.message);
  builder.startObject(4);
  builder.addFieldOffset(0, headerOffset, 0);
  builder.addFieldInt8(1, severityToFlatbuffer(payload.severity), EventSeverity.Info);
  builder.addFieldOffset(2, codeOffset, 0);
  builder.addFieldOffset(3, messageOffset, 0);
  return { type: GcsPayload.Event, offset: builder.endObject() };
}

function createHeader(builder: Builder, header: MessageHeader): Offset {
  const vehicleIdOffset = builder.createString(header.vehicleId);
  const messageTypeOffset = builder.createString(header.messageType);
  const streamIdOffset = builder.createString(header.streamId);

  builder.startObject(9);
  builder.addFieldInt64(0, BigInt(header.sequence), 0n);
  builder.addFieldInt64(1, nsNumberToBigInt(header.sourceTimeNs), 0n);
  builder.addFieldInt64(2, nsNumberToBigInt(header.receiveTimeNs), 0n);
  builder.addFieldInt64(3, nsNumberToBigInt(header.expireTimeNs), 0n);
  builder.addFieldOffset(4, vehicleIdOffset, 0);
  builder.addFieldInt16(5, header.schemaVersion, 0);
  builder.addFieldOffset(6, messageTypeOffset, 0);
  builder.addFieldInt8(7, priorityToFlatbuffer(header.priority), CommonPriority.Normal);
  builder.addFieldOffset(8, streamIdOffset, 0);
  return builder.endObject();
}

function decodeGcsFramePayload(bytes: Uint8Array): GcsFrame | null {
  const bb = new ByteBuffer(bytes);
  bb.setPosition(0);
  if (!bb.__has_identifier(ELECTRODE_GCS_FILE_IDENTIFIER)) {
    return null;
  }

  const root = bb.readInt32(0);
  const topic = readStringField(bb, root, 4);
  const payloadType = readUint8Field(bb, root, 6);
  const payload = readUnionTable(bb, root, 8);
  if (!topic || payload === null) {
    return null;
  }

  if (payloadType === GcsPayload.Pose) {
    return telemetryFrame(topic, decodeHeaderTable(bb, payload, 4), {
      lat: readFloat64Field(bb, payload, 6),
      lon: readFloat64Field(bb, payload, 8),
      altM: readFloat32Field(bb, payload, 10),
      xM: readFloat32Field(bb, payload, 12),
      yM: readFloat32Field(bb, payload, 14),
      zM: readFloat32Field(bb, payload, 16)
    });
  }

  if (payloadType === GcsPayload.Velocity) {
    return telemetryFrame(topic, decodeHeaderTable(bb, payload, 4), {
      northMps: readFloat32Field(bb, payload, 6),
      eastMps: readFloat32Field(bb, payload, 8),
      downMps: readFloat32Field(bb, payload, 10),
      groundSpeedMps: readFloat32Field(bb, payload, 12)
    });
  }

  if (payloadType === GcsPayload.Attitude) {
    return telemetryFrame(topic, decodeHeaderTable(bb, payload, 4), {
      rollDeg: readFloat32Field(bb, payload, 6),
      pitchDeg: readFloat32Field(bb, payload, 8),
      yawDeg: readFloat32Field(bb, payload, 10)
    });
  }

  if (payloadType === GcsPayload.Battery) {
    return telemetryFrame(topic, decodeHeaderTable(bb, payload, 4), {
      voltageV: readFloat32Field(bb, payload, 6),
      currentA: readFloat32Field(bb, payload, 8),
      remainingPct: readFloat32Field(bb, payload, 10)
    });
  }

  if (payloadType === GcsPayload.LinkStatus) {
    return telemetryFrame(topic, decodeHeaderTable(bb, payload, 4), {
      rssiDbm: readFloat32Field(bb, payload, 6),
      latencyMs: readFloat32Field(bb, payload, 8),
      packetLossPct: readFloat32Field(bb, payload, 10)
    });
  }

  if (payloadType === GcsPayload.ModeState) {
    return telemetryFrame(topic, decodeHeaderTable(bb, payload, 4), {
      name: readStringField(bb, payload, 6) || 'standby',
      armed: readBoolField(bb, payload, 8),
      failsafe: readBoolField(bb, payload, 10)
    });
  }

  if (payloadType === GcsPayload.LocalizationState) {
    const header = decodeHeaderTable(bb, payload, 4);
    return telemetryFrame(topic, header, {
      source: readStringField(bb, payload, 6) || 'unknown',
      fresh: readBoolField(bb, payload, 8),
      quality: readFloat32Field(bb, payload, 10),
      updatedAtMs: Number(header.sourceTimeNs / 1_000_000)
    });
  }

  if (payloadType === GcsPayload.TelemetryJson) {
    const header = decodeHeaderTable(bb, payload, 4);
    const json = readStringField(bb, payload, 6);
    try {
      const decoded = json ? JSON.parse(json) : null;
      return telemetryFrame(topic, header, decoded);
    } catch {
      return null;
    }
  }

  if (payloadType === GcsPayload.Event) {
    const header = decodeHeaderTable(bb, payload, 4);
    const event: EventMessage = {
      severity: severityFromFlatbuffer(readUint8Field(bb, payload, 6)),
      code: readStringField(bb, payload, 8) || 'log',
      message: readStringField(bb, payload, 10) || '',
      timestampMs: Number(header.sourceTimeNs / 1_000_000)
    };
    return { kind: 'event', topic, header, payload: event };
  }

  return null;
}

function decodeHeaderTable(bb: ByteBuffer, parent: number, vtableOffset: number): MessageHeader {
  const header = readTableField(bb, parent, vtableOffset);
  if (header === null) {
    return {
      sequence: 0,
      sourceTimeNs: 0,
      receiveTimeNs: 0,
      expireTimeNs: 0,
      vehicleId: '',
      schemaVersion: 0,
      messageType: 'Unknown',
      priority: 'normal',
      streamId: 'unknown'
    };
  }

  return {
    sequence: Number(readUint64Field(bb, header, 4)),
    sourceTimeNs: Number(readUint64Field(bb, header, 6)),
    receiveTimeNs: Number(readUint64Field(bb, header, 8)),
    expireTimeNs: Number(readUint64Field(bb, header, 10)),
    vehicleId: readStringField(bb, header, 12) || '',
    schemaVersion: readUint16Field(bb, header, 14),
    messageType: readStringField(bb, header, 16) || 'Unknown',
    priority: priorityFromFlatbuffer(readUint8Field(bb, header, 18, CommonPriority.Normal)),
    streamId: readStringField(bb, header, 20) || 'unknown'
  };
}

function telemetryFrame<TPayload>(topic: string, header: MessageHeader, payload: TPayload): TelemetryFrame<TPayload> {
  return {
    kind: 'telemetry',
    topic,
    header,
    payload
  };
}

function readTableField(bb: ByteBuffer, table: number, vtableOffset: number): number | null {
  const offset = bb.__offset(table, vtableOffset);
  return offset ? bb.__indirect(table + offset) : null;
}

function readUnionTable(bb: ByteBuffer, table: number, vtableOffset: number): number | null {
  const offset = bb.__offset(table, vtableOffset);
  if (!offset) {
    return null;
  }
  const field = table + offset;
  return field + bb.readInt32(field);
}

function readStringField(bb: ByteBuffer, table: number, vtableOffset: number): string {
  const offset = bb.__offset(table, vtableOffset);
  return offset ? (bb.__string(table + offset) as string) : '';
}

function readBoolField(bb: ByteBuffer, table: number, vtableOffset: number): boolean {
  return readUint8Field(bb, table, vtableOffset) !== 0;
}

function readUint8Field(bb: ByteBuffer, table: number, vtableOffset: number, defaultValue = 0): number {
  const offset = bb.__offset(table, vtableOffset);
  return offset ? bb.readUint8(table + offset) : defaultValue;
}

function readUint16Field(bb: ByteBuffer, table: number, vtableOffset: number): number {
  const offset = bb.__offset(table, vtableOffset);
  return offset ? bb.readUint16(table + offset) : 0;
}

function readUint64Field(bb: ByteBuffer, table: number, vtableOffset: number): bigint {
  const offset = bb.__offset(table, vtableOffset);
  return offset ? bb.readUint64(table + offset) : 0n;
}

function readFloat32Field(bb: ByteBuffer, table: number, vtableOffset: number): number {
  const offset = bb.__offset(table, vtableOffset);
  return offset ? bb.readFloat32(table + offset) : 0;
}

function readFloat64Field(bb: ByteBuffer, table: number, vtableOffset: number): number {
  const offset = bb.__offset(table, vtableOffset);
  return offset ? bb.readFloat64(table + offset) : 0;
}

function priorityToFlatbuffer(priority: Priority): CommonPriority {
  if (priority === 'low') {
    return CommonPriority.Low;
  }
  if (priority === 'high') {
    return CommonPriority.High;
  }
  if (priority === 'critical') {
    return CommonPriority.Critical;
  }
  return CommonPriority.Normal;
}

function priorityFromFlatbuffer(priority: number): Priority {
  if (priority === CommonPriority.Low) {
    return 'low';
  }
  if (priority === CommonPriority.High) {
    return 'high';
  }
  if (priority === CommonPriority.Critical) {
    return 'critical';
  }
  return 'normal';
}

function severityToFlatbuffer(severity: Severity): EventSeverity {
  if (severity === 'warning') {
    return EventSeverity.Warning;
  }
  if (severity === 'error') {
    return EventSeverity.Error;
  }
  return EventSeverity.Info;
}

function severityFromFlatbuffer(severity: number): Severity {
  if (severity === EventSeverity.Warning) {
    return 'warning';
  }
  if (severity === EventSeverity.Error) {
    return 'error';
  }
  return 'info';
}

function concatBytes(chunks: Uint8Array[]): Uint8Array {
  const length = chunks.reduce((total, chunk) => total + chunk.byteLength, 0);
  const result = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return result;
}

function nsNumberToBigInt(value: number): bigint {
  if (!Number.isFinite(value) || value <= 0) {
    return 0n;
  }
  return BigInt(Math.trunc(value));
}

function nsNumberToOptionalBigInt(value: number): bigint | null {
  const ns = nsNumberToBigInt(value);
  return ns > 0n ? ns : null;
}

function unixNowUs(): bigint {
  return BigInt(Date.now()) * 1000n;
}

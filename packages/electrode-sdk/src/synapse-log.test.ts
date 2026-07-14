import { McapStreamReader, hasMcapPrefix } from '@mcap/core';
import { describe, expect, it } from 'vitest';

import { makeSimulatedTelemetryBundle } from './simulator';
import { SynapseLogRecorder, decodeSynapseLogFrames } from './synapse-log';

describe('Synapse log recorder', () => {
  it('exports replayable MCAP files with FlatBuffer schemas', async () => {
    const frames = makeSimulatedTelemetryBundle({
      vehicleId: 'cubs2',
      elapsedMs: 120,
      sequenceStart: 1,
      nowMs: 1_700_000_000_000
    });
    const recorder = new SynapseLogRecorder({
      vehicleId: 'cubs2',
      source: 'vitest',
      createdUnixUs: 1_700_000_000_000_000n
    });

    for (const frame of frames) {
      expect(recorder.recordFrame(frame)).toBe(true);
    }

    const log = await recorder.export('unit');
    const decoded = decodeSynapseLogFrames(log.bytes);
    const channelTopics = readMcapChannelTopics(log.bytes);

    expect(log.filename).toMatch(/^unit-.*\.mcap$/);
    expect(log.mimeType).toBe('application/mcap');
    expect(log.frameCount).toBe(frames.length);
    expect(hasMcapPrefix(new DataView(log.bytes.buffer, log.bytes.byteOffset, 8))).toBe(true);
    expect(decoded).toHaveLength(frames.length);
    expect(decoded[0]).toMatchObject({
      kind: frames[0].kind,
      topic: frames[0].topic,
      header: frames[0].header,
      payload: frames[0].payload
    });
    expect(channelTopics).toEqual([...new Set(frames.map((frame) => frame.topic))].sort());
  });

  it('rejects non-MCAP log bytes', () => {
    expect(() => decodeSynapseLogFrames(new Uint8Array([0, 1, 2, 3]))).toThrow('Invalid MCAP log file');
  });

  it('preserves structured parameter audit records', async () => {
    const recorder = new SynapseLogRecorder({
      vehicleId: 'cubs2',
      source: 'vitest',
      createdUnixUs: 1_700_000_000_000_000n
    });
    const audit = {
      schema: 'electrode.parameter-audit.v1',
      timestampUnixMs: 1_700_000_000_000,
      source: 'public_lan',
      name: 'velocity.setpoint',
      requestedValue: 4.5,
      effectiveValue: 4.5,
      status: 'accepted'
    };
    expect(
      recorder.recordFrame({
        kind: 'telemetry',
        topic: 'gcs/v1/audit/parameter',
        header: {
          sequence: 1,
          sourceTimeNs: 1_700_000_000_000_000_000,
          receiveTimeNs: 1_700_000_000_000_000_000,
          expireTimeNs: 0,
          vehicleId: 'cubs2',
          schemaVersion: 1,
          messageType: 'ParameterAudit',
          priority: 'normal',
          streamId: 'gcs/v1/audit/parameter'
        },
        payload: audit
      })
    ).toBe(true);

    const log = await recorder.export('audit');
    expect(decodeSynapseLogFrames(log.bytes)[0]).toMatchObject({
      topic: 'gcs/v1/audit/parameter',
      payload: audit
    });
  });
});

function readMcapChannelTopics(bytes: Uint8Array): string[] {
  const reader = new McapStreamReader({ validateCrcs: false });
  const topics = new Set<string>();
  const schemas = new Set<string>();

  reader.append(bytes);
  for (let record = reader.nextRecord(); record; record = reader.nextRecord()) {
    if (record.type === 'Schema') {
      schemas.add(record.name);
    } else if (record.type === 'Channel') {
      topics.add(record.topic);
    }
  }

  expect(schemas.has('electrode.gcs.GcsFrame')).toBe(true);
  return [...topics].sort();
}

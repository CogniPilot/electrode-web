import { beforeEach, describe, expect, it, vi } from 'vitest';
import { encodeCompactRigidBodyPose, encodeRawPose } from './mocap-encode';
import { expectedTopicEncoding } from './synapse-decode';
import { parseKey } from '@cognipilot/synapse-fbs/topic_catalog';
import { ZenohWasmTransport } from './transport';

const zenohMock = vi.hoisted(() => {
  const subscribers = new Map<string, (key: string, payload: Uint8Array, encoding?: string) => void>();
  const session = {
    declareSubscriber: vi.fn(async (
      key: string,
      callback: (sampleKey: string, payload: Uint8Array, encoding?: string) => void
    ) => {
      subscribers.set(key, callback);
      return { undeclare: vi.fn(async () => {}) };
    }),
    isClosed: vi.fn(() => false),
    close: vi.fn(async () => {}),
    putBytes: vi.fn(async () => {})
  };
  return { session, subscribers };
});

vi.mock('@cognipilot/zenoh-wasm', () => ({
  default: vi.fn(async () => {}),
  initPanicHook: vi.fn(),
  version: vi.fn(() => 'test'),
  open: vi.fn(async () => zenohMock.session),
  openWithConfig: vi.fn(async () => zenohMock.session)
}));

describe('Zenoh browser transport subscriptions', () => {
  beforeEach(() => {
    zenohMock.subscribers.clear();
    vi.clearAllMocks();
  });

  it('does not subscribe to the legacy compact mocap pose by default', async () => {
    const messages: unknown[] = [];
    const transport = new ZenohWasmTransport(
      'ws/127.0.0.1:7447',
      (message) => messages.push(message),
      vi.fn()
    );
    await transport.connect();

    const key = 'synapse/mocap/rigid_body/cub1/pose';
    zenohMock.subscribers.get('electrode/catalog/synapse')?.(
      'electrode/catalog/synapse',
      new TextEncoder().encode(JSON.stringify({ key, lastBytes: 28 }))
    );
    zenohMock.subscribers.get(key)?.(
      key,
      encodeCompactRigidBodyPose({
        position: { x: 1, y: 2, z: 3 },
        attitude: { x: 0, y: 0, z: 0, w: 1 }
      })
    );

    expect(messages).toHaveLength(0);
    await transport.disconnect();
  });

  it('streams the Qualisys bridge raw frame and does not require its EKF output', async () => {
    const messages: unknown[] = [];
    const transport = new ZenohWasmTransport(
      'ws/127.0.0.1:7447',
      (message) => messages.push(message),
      vi.fn()
    );
    await transport.connect();

    const key = 'qualisys/cub1/pose_raw';
    const bytes = encodeRawPose(
      { position: { x: 1, y: 2, z: 3 }, attitude: { x: 0, y: 0, z: 0, w: 1 } }
    );
    const encoding = expectedTopicEncoding(parseKey(key)!.topic);
    zenohMock.subscribers.get(key)?.(key, bytes, encoding);

    expect(messages).toHaveLength(1);
    expect(messages[0]).toMatchObject({
      kind: 'telemetry',
      topic: key,
      payload: { data: { position: { x: 1, y: 2, z: 3 } } }
    });
    await transport.disconnect();
  });

  it('subscribes to parameter audit records for MCAP recording', async () => {
    const onRawSample = vi.fn();
    const transport = new ZenohWasmTransport(
      'ws/127.0.0.1:7447',
      vi.fn(),
      vi.fn(),
      undefined,
      { onRawSample }
    );
    await transport.connect();

    const key = 'gcs/v1/audit/parameter';
    const payload = new TextEncoder().encode('{"name":"velocity.setpoint"}');
    zenohMock.subscribers.get(key)?.(key, payload);

    expect(onRawSample).toHaveBeenCalledWith(key, payload);
    await transport.disconnect();
  });
});

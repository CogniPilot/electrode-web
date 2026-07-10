import { beforeEach, describe, expect, it, vi } from 'vitest';

import { encodeCompactRigidBodyPose } from './mocap-encode';
import { ZenohWasmTransport } from './transport';

const zenohMock = vi.hoisted(() => {
  const subscribers = new Map<string, (key: string, payload: Uint8Array) => void>();
  const session = {
    declareSubscriber: vi.fn(async (
      key: string,
      callback: (sampleKey: string, payload: Uint8Array) => void
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

  it('streams cub1 when its catalog announcement arrives before its first payload', async () => {
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

    expect(messages).toHaveLength(1);
    expect(messages[0]).toMatchObject({ kind: 'telemetry', topic: key });
    await transport.disconnect();
  });

  it('streams CUB1 external odometry after catalog discovery', async () => {
    const messages: unknown[] = [];
    const transport = new ZenohWasmTransport(
      'ws/127.0.0.1:7447',
      (message) => messages.push(message),
      vi.fn()
    );
    await transport.connect();

    const key = 'synapse/v1/topic/external_odometry/1';
    zenohMock.subscribers.get('electrode/catalog/synapse')?.(
      'electrode/catalog/synapse',
      new TextEncoder().encode(JSON.stringify({ key, lastBytes: 64 }))
    );
    zenohMock.subscribers.get('synapse/v1/**')?.(key, new Uint8Array(64));

    expect(messages).toHaveLength(1);
    expect(messages[0]).toMatchObject({
      kind: 'telemetry',
      topic: key,
      payload: { data: { id: 0 } }
    });
    await transport.disconnect();
  });
});

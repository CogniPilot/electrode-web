import { classify, decode } from './synapse-decode';
import type { CommandIntent, ConnectionState, GcsFrame } from './types';

export type TransportMessage =
  | GcsFrame
  | {
      kind: 'commandAck';
      commandId: string;
      command: string;
      status: 'acked' | 'published' | 'rejected' | 'timeout';
      reason: string;
      sequence: number;
      receivedAtMs: number;
    };

/** One discovered Zenoh key reported by the transport. */
export interface TopicCatalogEntry {
  key: string;
  schema: string;
  /** True when we can decode this key into structured fields. */
  decodable: boolean;
  /** True when we are currently forwarding this key as telemetry. */
  selected: boolean;
  count: number;
  rateHz: number;
  lastBytes: number;
  lastSeenMs: number;
}

/** Snapshot of everything seen on the Zenoh network. */
export interface TopicCatalog {
  kind: 'topicCatalog';
  connected: boolean;
  endpoint: string;
  generatedAtMs: number;
  topics: TopicCatalogEntry[];
}

/** Optional tuning for the direct-Zenoh transport. */
export interface ZenohTransportOptions {
  /** Vehicle id stamped on forwarded telemetry frames. */
  vehicleId?: string;
  /** Wildcard key expression to discover/subscribe, e.g. `synapse/**`. */
  keyExpr?: string;
  /** Auto-forward decodable topics as soon as they appear. */
  autoSelectKnown?: boolean;
  /**
   * Explicit URL for `zenoh_wasm_bg.wasm`. Supply the bundler-resolved asset URL
   * (e.g. Vite `import wasm from '.../zenoh_wasm_bg.wasm?url'`) so the module is
   * fetched with the correct `application/wasm` MIME type instead of relying on
   * the glue's `import.meta.url` guess (which bundlers rewrite incorrectly).
   */
  wasmUrl?: string;
}

type ZenohWasmModule = typeof import('@cognipilot/zenoh-wasm');
type ZenohSession = import('@cognipilot/zenoh-wasm').ZenohSession;
type WasmSubscriber = import('@cognipilot/zenoh-wasm').WasmSubscriber;

const ZENOH_CONNECT_TIMEOUT_MS = 15_000;
const ZENOH_PUBLISH_TIMEOUT_MS = 2_000;
const CATALOG_INTERVAL_MS = 500;
const DEFAULT_KEY_EXPR = 'synapse/**';
// Matches electrode_web_core::SCHEMA_VERSION on the native side.
const SCHEMA_VERSION = 1;

/** Per-topic discovery statistics (mirrors the native bridge registry). */
interface TopicStat {
  schema: string;
  decodable: boolean;
  count: number;
  prevCount: number;
  lastBytes: number;
  rateHz: number;
  firstSeenMs: number;
  lastSeenMs: number;
}

/**
 * Connects the browser directly to a Zenoh router via the `@cognipilot/zenoh-wasm`
 * client. Publishes commands, subscribes to `synapse/**`, maintains a discovery
 * catalog, and decodes selected topics into telemetry frames — everything the
 * native ground-bridge did, without the bridge process.
 */
export class ZenohWasmTransport {
  #session: ZenohSession | null = null;
  #zenoh: ZenohWasmModule | null = null;
  #subscriber: WasmSubscriber | null = null;
  #version = 'unknown';
  #registry = new Map<string, TopicStat>();
  #selected = new Set<string>();
  #catalogTimer: ReturnType<typeof setInterval> | null = null;
  #connected = false;
  #sequence = 1;
  readonly #vehicleId: string;
  readonly #keyExpr: string;
  readonly #autoSelectKnown: boolean;
  readonly #wasmUrl?: string;

  constructor(
    private readonly endpointOrConfig: string,
    private readonly onMessage: (message: TransportMessage) => void,
    private readonly onConnection: (state: ConnectionState) => void,
    private readonly onCatalog?: (catalog: TopicCatalog) => void,
    options: ZenohTransportOptions = {}
  ) {
    this.#vehicleId = options.vehicleId ?? 'electrode-01';
    this.#keyExpr = options.keyExpr ?? DEFAULT_KEY_EXPR;
    this.#autoSelectKnown = options.autoSelectKnown ?? true;
    this.#wasmUrl = options.wasmUrl;
  }

  async connect(): Promise<void> {
    this.onConnection({
      mode: 'zenoh',
      status: 'connecting',
      url: this.endpointOrConfig,
      message: 'loading zenoh-wasm'
    });

    try {
      this.#zenoh = await import('@cognipilot/zenoh-wasm');
      await this.#zenoh.default(this.#wasmUrl ? { module_or_path: this.#wasmUrl } : undefined);
      this.#zenoh.initPanicHook();
      this.#version = this.#zenoh.version();

      const input = this.endpointOrConfig.trim();
      this.#session = await withTimeout(
        input.startsWith('{') ? this.#zenoh.openWithConfig(input) : this.#zenoh.open(input),
        ZENOH_CONNECT_TIMEOUT_MS,
        `opening Zenoh endpoint ${this.endpointOrConfig}`
      );

      this.#subscriber = await this.#session.declareSubscriber(
        this.#keyExpr,
        (key: string, payload: Uint8Array) => this.#onSample(key, payload)
      );

      this.#connected = true;
      this.#catalogTimer = setInterval(() => this.#emitCatalog(), CATALOG_INTERVAL_MS);
      this.#emitCatalog();

      this.onConnection({
        mode: 'zenoh',
        status: 'connected',
        url: this.endpointOrConfig,
        message: `zenoh-wasm ${this.#version} · ${this.#keyExpr}`
      });
    } catch (error) {
      this.onConnection({
        mode: 'zenoh',
        status: 'error',
        url: this.endpointOrConfig,
        message: error instanceof Error ? error.message : String(error)
      });
      throw error;
    }
  }

  async sendCommand(command: CommandIntent): Promise<void> {
    if (!this.#session || this.#session.isClosed()) {
      throw new Error('zenoh-wasm session is not open');
    }

    await withTimeout(
      this.#session.putString(command.topic, JSON.stringify(command)),
      ZENOH_PUBLISH_TIMEOUT_MS,
      `publishing ${command.command} to ${command.topic}`
    );
  }

  /** Replace the set of discovered keys we forward as telemetry. */
  setSubscriptions(keys: string[]): void {
    this.#selected = new Set(keys);
  }

  async disconnect(): Promise<void> {
    if (this.#catalogTimer) {
      clearInterval(this.#catalogTimer);
      this.#catalogTimer = null;
    }
    this.#connected = false;

    const subscriber = this.#subscriber;
    this.#subscriber = null;
    if (subscriber) {
      try {
        await subscriber.undeclare();
      } catch {
        // Session may already be closing; ignore.
      }
    }

    const session = this.#session;
    this.#session = null;
    if (session && !session.isClosed()) {
      await session.close();
    }

    this.onConnection({
      mode: 'zenoh',
      status: 'disconnected',
      url: this.endpointOrConfig,
      message: 'zenoh-wasm session closed'
    });
  }

  get version(): string {
    return this.#version;
  }

  #onSample(key: string, payload: Uint8Array): void {
    const now = Date.now();
    let stat = this.#registry.get(key);
    if (!stat) {
      const schema = classify(key);
      stat = {
        schema,
        decodable: schema !== 'Raw',
        count: 0,
        prevCount: 0,
        lastBytes: 0,
        rateHz: 0,
        firstSeenMs: now,
        lastSeenMs: now
      };
      this.#registry.set(key, stat);
      // Auto-select decodable topics on first sight so data flows immediately.
      if (this.#autoSelectKnown && schema !== 'Raw') {
        this.#selected.add(key);
      }
    }
    stat.count += 1;
    stat.lastBytes = payload.length;
    stat.lastSeenMs = now;

    if (this.#selected.has(key)) {
      this.onMessage(this.#buildFrame(key, payload));
    }
  }

  #buildFrame(key: string, payload: Uint8Array): TransportMessage {
    const decoded = decode(key, payload);
    const now = Date.now();
    return {
      kind: 'telemetry',
      topic: key,
      header: {
        sequence: this.#sequence++,
        sourceTimeNs: now * 1_000_000,
        receiveTimeNs: now * 1_000_000,
        expireTimeNs: 0,
        vehicleId: this.#vehicleId,
        schemaVersion: SCHEMA_VERSION,
        messageType: decoded.schema,
        priority: 'normal',
        streamId: key
      },
      payload: decoded.payload
    };
  }

  #emitCatalog(): void {
    if (!this.onCatalog) {
      return;
    }

    for (const stat of this.#registry.values()) {
      const delta = Math.max(0, stat.count - stat.prevCount);
      stat.prevCount = stat.count;
      stat.rateHz = delta / (CATALOG_INTERVAL_MS / 1000);
    }

    const topics: TopicCatalogEntry[] = Array.from(this.#registry.entries())
      .map(([key, stat]) => ({
        key,
        schema: stat.schema,
        decodable: stat.decodable,
        selected: this.#selected.has(key),
        count: stat.count,
        rateHz: stat.rateHz,
        lastBytes: stat.lastBytes,
        lastSeenMs: stat.lastSeenMs
      }))
      .sort((a, b) => a.key.localeCompare(b.key));

    this.onCatalog({
      kind: 'topicCatalog',
      connected: this.#connected,
      endpoint: this.endpointOrConfig,
      generatedAtMs: Date.now(),
      topics
    });
  }
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, description: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(`Timed out while ${description}`)), timeoutMs);
  });

  try {
    return await Promise.race([promise, timeout]);
  } finally {
    if (timer) {
      clearTimeout(timer);
    }
  }
}

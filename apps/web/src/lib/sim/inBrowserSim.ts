// Controller for the in-browser rumoca flight sim. Spawns the sim worker,
// hands it the Zenoh endpoint + Modelica source, and relays status. The worker
// runs Rumoca WASM behind Ground Station's private sim wrapper topics.
import zenohWasmUrl from '@cognipilot/zenoh-wasm/zenoh_wasm_bg.wasm?url';

export type SimPhase = 'compiling' | 'compiled' | 'connecting' | 'running' | 'stopped';

export interface InBrowserSimOptions {
  /** Zenoh endpoint the viewer uses (e.g. `ws/127.0.0.1:7447`). */
  endpoint: string;
  /** Self-contained Modelica source. */
  modelSource: string;
  modelName?: string;
  solver?: string;
  inputTopic: string;
  mocapTopic: string;
  pubHz?: number;
  onPhase?: (phase: SimPhase) => void;
  onError?: (message: string) => void;
}

export class InBrowserSim {
  #worker: Worker | null = null;

  get running(): boolean {
    return this.#worker != null;
  }

  start(options: InBrowserSimOptions): void {
    this.stop();
    const worker = new Worker(new URL('../workers/sim.worker.ts', import.meta.url), { type: 'module' });
    worker.addEventListener('message', (event: MessageEvent) => {
      const message = event.data;
      if (message?.type === 'status') {
        options.onPhase?.(message.phase as SimPhase);
      } else if (message?.type === 'error') {
        options.onError?.(String(message.message));
      }
    });
    worker.postMessage({
      type: 'start',
      endpoint: options.endpoint,
      zenohWasmUrl,
      modelSource: options.modelSource,
      modelName: options.modelName ?? 'FixedWingTrueSILFull',
      solver: options.solver ?? 'rk-like',
      inputTopic: options.inputTopic,
      mocapTopic: options.mocapTopic,
      pubHz: options.pubHz ?? 100
    });
    this.#worker = worker;
  }

  stop(): void {
    if (this.#worker) {
      this.#worker.postMessage({ type: 'stop' });
      this.#worker.terminate();
      this.#worker = null;
    }
  }
}

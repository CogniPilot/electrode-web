// Shared controller for the in-browser rumoca flight sim.
//
// One InBrowserSim instance for the whole app, started and stopped from the
// SIM page. The wasm stepper plays the aircraft behind Ground Station's
// wrapper: private PWM in, private pose out; Ground Station owns public Synapse.
import { get, writable } from 'svelte/store';

import { fetchSimulationModel } from '$lib/gcs';
import { InBrowserSim, type SimPhase } from '$lib/sim/inBrowserSim';

/** Canonical Synapse topic prefix (synapse_fbs 0.3.0 key scheme). */
export const SYNAPSE_TOPIC_PREFIX = 'synapse/v1/topic';
const RUMOCA_RADIO_PWM_TOPIC = 'electrode/sim/rumoca/radio_pwm_signal_outputs';
const RUMOCA_MOCAP_TOPIC = 'electrode/sim/rumoca/mocap_frame';

const sim = new InBrowserSim();

/** Live phase of the shared sim ('idle' before the first start). */
export const simPhase = writable<SimPhase | 'idle'>('idle');
/** Last error surfaced by the sim worker, empty when healthy. */
export const simError = writable('');

export function simRunning(): boolean {
  const phase = get(simPhase);
  return phase !== 'idle' && phase !== 'stopped';
}

export interface StartBrowserSimOptions {
  /** Zenoh endpoint the viewer uses (e.g. `ws/127.0.0.1:7447`). */
  endpoint: string;
  /**
   * Modelica source to compile. Omit to fetch the profile's model from the
   * Ground Station backend (the panel passes its edited buffer instead).
   */
  modelSource?: string;
  modelName?: string;
  pubHz?: number;
}

/** Derive the top-level model name from a Modelica file path (`Foo.mo` -> `Foo`). */
function modelNameFromPath(path: string): string {
  const base = path.split('/').pop() ?? path;
  return base.replace(/\.mo(\.in)?$/, '');
}

/**
 * Start (or restart) the shared in-browser sim. Resolves once the start
 * message is posted; progress arrives via {@link simPhase}.
 */
export async function startBrowserSim(options: StartBrowserSimOptions): Promise<void> {
  let source = options.modelSource;
  let name = options.modelName;
  if (!source) {
    const model = await fetchSimulationModel();
    source = model.text;
    name = name ?? modelNameFromPath(model.path);
  }
  simError.set('');
  simPhase.set('compiling');
  sim.start({
    endpoint: options.endpoint,
    modelSource: source,
    modelName: name ?? 'FixedWingTrueSILFull',
    inputTopic: RUMOCA_RADIO_PWM_TOPIC,
    mocapTopic: RUMOCA_MOCAP_TOPIC,
    pubHz: options.pubHz ?? 100,
    onPhase: (phase) => simPhase.set(phase),
    onError: (message) => simError.set(message)
  });
}

export function stopBrowserSim(): void {
  sim.stop();
  simPhase.set('stopped');
}

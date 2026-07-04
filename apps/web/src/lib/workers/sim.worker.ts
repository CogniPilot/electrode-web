// In-browser flight simulator: a rumoca WASM stepper behind the Ground Station.
// It consumes private Ground-Station-provided PWM and publishes private plant
// pose. Ground Station owns the public Synapse/Zenoh hardware-facing topics.
//
// Two WASM modules live in this worker: the rumoca stepper (loaded at runtime
// from /wasm/) and zenoh-wasm (bundled). All sim state stays here; the main
// thread only starts/stops and receives status.

import { decode, encodeMocapFrame } from '@electrode/sdk';

type StartMessage = {
  type: 'start';
  /** Zenoh endpoint (ws/... locator or JSON5 config) — same one the viewer uses. */
  endpoint: string;
  /** Bundler-resolved URL for zenoh_wasm_bg.wasm. */
  zenohWasmUrl: string;
  /** Modelica source (self-contained) and top-level model name. */
  modelSource: string;
  modelName: string;
  /** Solver id, e.g. "rk-like". */
  solver?: string;
  /** Private Ground Station wrapper topic carrying Rumoca PWM input. */
  inputTopic: string;
  /** Private Ground Station wrapper topic carrying Rumoca mocap output. */
  mocapTopic: string;
  /** MocapFrame publish rate, Hz (default 100). */
  pubHz?: number;
};

type IncomingMessage = StartMessage | { type: 'stop' };

// eslint-disable-next-line @typescript-eslint/no-explicit-any
let rumoca: any = null;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let stepper: any = null;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let zenoh: any = null;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let session: any = null;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let subscriber: any = null;

let stepTimer: ReturnType<typeof setInterval> | null = null;
let pubTimer: ReturnType<typeof setInterval> | null = null;
let lastStepMs = 0;
let frameNumber = 0;
let inputTopic = 'electrode/sim/rumoca/radio_pwm_signal_outputs';
let mocapTopic = 'electrode/sim/rumoca/mocap_frame';

// Latest actuator command (PWM microseconds) from pwm_signal_outputs; neutral
// until the autopilot publishes. Matches the model's ail/elev/thr/rud_pwm inputs.
const pwm = { ail_pwm: 1500, elev_pwm: 1500, thr_pwm: 1000, rud_pwm: 1500 };

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function post(message: any): void {
  self.postMessage(message);
}

async function loadRumoca(): Promise<void> {
  if (rumoca) return;
  // Dynamic import via Function() so Vite/Rollup don't statically resolve it —
  // the stepper wasm is served from /wasm/ at runtime, not bundled.
  rumoca = await (new Function('return import("/wasm/rumoca_bind_wasm.js")'))();
  await rumoca.default();
  rumoca.init();
}

function applyMotorOutput(payload: Uint8Array): void {
  try {
    const decoded = decode('synapse/v1/topic/pwm_signal_outputs', payload);
    // `motors` carries the first four PWM outputs in microseconds.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const motors = (decoded.payload as any)?.motors;
    if (motors) {
      pwm.ail_pwm = Number(motors.m0);
      pwm.elev_pwm = Number(motors.m1);
      pwm.thr_pwm = Number(motors.m2);
      pwm.rud_pwm = Number(motors.m3);
    }
  } catch {
    // Ignore undecodable samples.
  }
}

function stepOnce(): void {
  if (!stepper) return;
  const now = performance.now();
  const dt = Math.min((now - lastStepMs) / 1000, 0.05);
  lastStepMs = now;
  try {
    stepper.set_input('ail_pwm', pwm.ail_pwm);
    stepper.set_input('elev_pwm', pwm.elev_pwm);
    stepper.set_input('thr_pwm', pwm.thr_pwm);
    stepper.set_input('rud_pwm', pwm.rud_pwm);
    stepper.step(dt);
  } catch (error) {
    post({ type: 'error', message: `step: ${String(error)}` });
    stop();
  }
}

function publishMocap(): void {
  if (!stepper || !session) return;
  const bytes = encodeMocapFrame(
    {
      position: { x: stepper.get('cer_x'), y: stepper.get('cer_y'), z: stepper.get('cer_z') },
      attitude: { x: stepper.get('mq_x'), y: stepper.get('mq_y'), z: stepper.get('mq_z'), w: stepper.get('mq_w') }
    },
    { frameNumber: frameNumber++, timestampUs: Date.now() * 1000 }
  );
  // Fire-and-forget; a slow put must not stall the step loop.
  session.putBytes(mocapTopic, bytes).catch(() => {});
}

async function start(msg: StartMessage): Promise<void> {
  stop();
  inputTopic = msg.inputTopic;
  mocapTopic = msg.mocapTopic;

  // 1. Compile the flight model (blocks a few seconds — one-time).
  post({ type: 'status', phase: 'compiling' });
  await loadRumoca();
  stepper = new rumoca.WasmStepper(msg.modelSource, msg.modelName, msg.solver ?? 'rk-like');
  post({ type: 'status', phase: 'compiled' });

  // 2. Join the Zenoh network as a peer, like real hardware.
  post({ type: 'status', phase: 'connecting' });
  zenoh = await import('@cognipilot/zenoh-wasm');
  await zenoh.default({ module_or_path: msg.zenohWasmUrl });
  zenoh.initPanicHook();
  const input = msg.endpoint.trim();
  session = input.startsWith('{') ? await zenoh.openWithConfig(input) : await zenoh.open(input);
  subscriber = await session.declareSubscriber(inputTopic, (_key: string, payload: Uint8Array) =>
    applyMotorOutput(payload)
  );

  // 3. Real-time loop: physics decoupled from publish cadence.
  lastStepMs = performance.now();
  frameNumber = 0;
  const pubMs = 1000 / (msg.pubHz ?? 100);
  stepTimer = setInterval(stepOnce, 4);
  pubTimer = setInterval(publishMocap, pubMs);
  post({ type: 'status', phase: 'running' });
}

function stop(): void {
  if (stepTimer != null) clearInterval(stepTimer);
  if (pubTimer != null) clearInterval(pubTimer);
  stepTimer = pubTimer = null;
  if (subscriber) {
    subscriber.undeclare?.().catch?.(() => {});
    subscriber = null;
  }
  if (session) {
    if (!session.isClosed?.()) session.close?.().catch?.(() => {});
    session = null;
  }
  if (stepper) {
    stepper.free?.();
    stepper = null;
  }
  post({ type: 'status', phase: 'stopped' });
}

self.onmessage = async (event: MessageEvent<IncomingMessage>) => {
  try {
    const msg = event.data;
    if (msg.type === 'start') await start(msg);
    else if (msg.type === 'stop') stop();
  } catch (error) {
    post({ type: 'error', message: error instanceof Error ? error.message : String(error) });
    stop();
  }
};

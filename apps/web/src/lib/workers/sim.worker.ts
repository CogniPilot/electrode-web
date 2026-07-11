// In-browser flight simulator: a rumoca WASM stepper behind the Ground Station.
// It consumes private Ground-Station-provided PWM and publishes private plant
// pose. Ground Station owns the public Synapse/Zenoh hardware-facing topics.
//
// Two WASM modules live in this worker: the rumoca simulation session (from
// the @cognipilot/rumoca npm package) and zenoh-wasm. All sim state stays
// here; the main thread only starts/stops and receives status.

import initRumoca, { WasmSimulationSession } from '@cognipilot/rumoca';
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

// advance_to() clamps at the session's t_end; the flight sim runs until the
// user stops it, so compile with an effectively unbounded horizon.
const SIM_HORIZON_S = 1e7;

let rumocaReady: Promise<unknown> | null = null;
let stepper: WasmSimulationSession | null = null;
let zenoh: any = null;
let session: any = null;
let subscriber: any = null;

let stepTimer: ReturnType<typeof setInterval> | null = null;
let pubTimer: ReturnType<typeof setInterval> | null = null;
let lastStepMs = 0;
let frameNumber = 0;
let inputTopic = 'electrode/sim/rumoca/radio_pwm_signal_outputs';
let mocapTopic = 'electrode/sim/rumoca/mocap_frame';

// Latest actuator command (PWM microseconds) from the `pwm` topic; neutral
// until the autopilot publishes. Matches the model's ail/elev/thr/rud_pwm inputs.
const pwm = { ail_pwm: 1500, elev_pwm: 1500, thr_pwm: 1000, rud_pwm: 1500 };

function post(message: any): void {
  self.postMessage(message);
}

async function loadRumoca(): Promise<void> {
  // The wasm-bindgen init re-instantiates on every call; cache the promise.
  rumocaReady ??= initRumoca();
  await rumocaReady;
}

function applyMotorOutput(payload: Uint8Array, encoding?: string): void {
  try {
    // The private wrapper topic relays PwmSignalOutputs samples; decode under
    // the compact catalog key so the strict value contract applies.
    const decoded = decode('pwm', payload, encoding);
    // `motors` carries the first four PWM outputs in microseconds.
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
    stepper.advance_to(stepper.time() + dt);
  } catch (error) {
    post({ type: 'error', message: `step: ${String(error)}` });
    stop();
  }
}

function readVar(name: string): number {
  const value = stepper!.get(name);
  if (value === undefined) throw new Error(`sim variable not found: ${name}`);
  return value;
}

function publishMocap(): void {
  if (!stepper || !session) return;
  const bytes = encodeMocapFrame(
    {
      position: { x: readVar('cer_x'), y: readVar('cer_y'), z: readVar('cer_z') },
      attitude: { x: readVar('mq_x'), y: readVar('mq_y'), z: readVar('mq_z'), w: readVar('mq_w') }
    },
    // QTM-style 1-based rigid-body id so the Ground Station's bridge-parity
    // republisher matches synapse_qualisys_bridge numbering.
    { frameNumber: frameNumber++, timestampUs: Date.now() * 1000, bodyId: 1 }
  );
  // Fire-and-forget; a slow put must not stall the step loop.
  session.putBytes(mocapTopic, bytes).catch(() => {});
}

function publishMocapSafe(): void {
  try {
    publishMocap();
  } catch (error) {
    post({ type: 'error', message: `mocap: ${String(error)}` });
    stop();
  }
}

async function start(msg: StartMessage): Promise<void> {
  stop();
  inputTopic = msg.inputTopic;
  mocapTopic = msg.mocapTopic;

  // 1. Compile the flight model (blocks a few seconds — one-time).
  post({ type: 'status', phase: 'compiling' });
  await loadRumoca();
  // dt/atol/rtol of 0 mean "use the model's experiment metadata or defaults".
  stepper = WasmSimulationSession.withOptions(
    msg.modelSource,
    msg.modelName,
    SIM_HORIZON_S,
    0,
    msg.solver ?? 'rk-like',
    0,
    0
  );
  post({ type: 'status', phase: 'compiled' });

  // 2. Join the Zenoh network as a peer, like real hardware.
  post({ type: 'status', phase: 'connecting' });
  zenoh = await import('@cognipilot/zenoh-wasm');
  await zenoh.default({ module_or_path: msg.zenohWasmUrl });
  zenoh.initPanicHook();
  const input = msg.endpoint.trim();
  session = input.startsWith('{') ? await zenoh.openWithConfig(input) : await zenoh.open(input);
  subscriber = await session.declareSubscriber(
    inputTopic,
    (_key: string, payload: Uint8Array, encoding?: string) => applyMotorOutput(payload, encoding)
  );

  // 3. Real-time loop: physics decoupled from publish cadence.
  lastStepMs = performance.now();
  frameNumber = 0;
  const pubMs = 1000 / (msg.pubHz ?? 100);
  stepTimer = setInterval(stepOnce, 4);
  pubTimer = setInterval(publishMocapSafe, pubMs);
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
    stepper.free();
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

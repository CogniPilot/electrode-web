import {
  applyGcsFrame,
  appendEvent,
  buildCommandIntent,
  createDemoReplay,
  createInitialVehicleState,
  framesThroughCursor,
  normalizeReplayFrames,
  refreshStaleTopics,
  rejectedCommandResult,
  replayDurationMs,
  createPlotPacketCatalog,
  decodeSynapseLogFrames,
  extractPlotSeriesUpdates,
  plotSeriesKey,
  SynapseLogRecorder,
  validateCommandPreconditions,
  ZenohWasmTransport,
  type CommandName,
  type CommandResult,
  type ConnectionState,
  type GcsFrame,
  type PlotPacketDefinition,
  type PlotSeries,
  type RuntimeMode,
  type TopicCatalog,
  type TransportMessage,
  type VehicleState
} from '@electrode/sdk';
import zenohWasmUrl from '../zenohWasmUrl';

type WorkerIn =
  | { type: 'connect'; mode: RuntimeMode; url: string; vehicleId: string }
  | { type: 'disconnect' }
  | { type: 'command'; command: CommandName; args?: Record<string, unknown> }
  | { type: 'setSubscriptions'; keys: string[] }
  | { type: 'startRecording' }
  | { type: 'stopRecording' }
  | { type: 'exportRecording' }
  | { type: 'loadReplay'; bytes?: Uint8Array }
  | { type: 'playReplay' }
  | { type: 'pauseReplay' }
  | { type: 'seekReplay'; cursorMs: number }
  | { type: 'setReplaySpeed'; speed: number };

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const MAX_PLOT_SAMPLES = 240;
const PLOT_POST_INTERVAL_MS = 250;

let vehicleId = 'electrode-01';
let state: VehicleState = createInitialVehicleState(vehicleId);
let mode: RuntimeMode = 'zenoh';
let zenoh: ZenohWasmTransport | null = null;
let staleTimer: ReturnType<typeof setInterval> | null = null;
let replayTimer: ReturnType<typeof setInterval> | null = null;
let commandSequence = 1;
let recording = false;
let recorder: SynapseLogRecorder | null = null;
let replayFrames: GcsFrame[] = [];
let replayCursorMs = 0;
let replaySpeed = 1;
let replayPlaying = false;
let plotCatalog: PlotPacketDefinition[] = createPlotPacketCatalog(vehicleId);
let plotSeries = new Map<string, PlotSeries>();
let lastPlotPostMs = 0;
const pendingCommandArgs = new Map<string, Record<string, unknown>>();

postState();
postReplay();
postPlotState(true);

ctx.addEventListener('message', (event: MessageEvent<WorkerIn>) => {
  const message = event.data;

  if (message.type === 'connect') {
    vehicleId = message.vehicleId || vehicleId;
    connect(message.mode, message.url);
  } else if (message.type === 'disconnect') {
    disconnect();
  } else if (message.type === 'command') {
    sendCommand(message.command, message.args ?? {});
  } else if (message.type === 'setSubscriptions') {
    zenoh?.setSubscriptions(message.keys);
  } else if (message.type === 'startRecording') {
    recording = true;
    recorder = new SynapseLogRecorder({
      vehicleId,
      source: `electrode-web/${mode}`,
      description: `electrode ${vehicleId} ${mode} session`
    });
    ctx.postMessage({ type: 'recording', recording, count: recorder.frameCount });
  } else if (message.type === 'stopRecording') {
    recording = false;
    ctx.postMessage({ type: 'recording', recording, count: recorder?.frameCount ?? 0 });
    // Stopping saves the log immediately — no separate export step needed.
    if (recorder && recorder.frameCount > 0) {
      ctx.postMessage({ type: 'recordingExport', export: recorder.export() });
    }
  } else if (message.type === 'exportRecording') {
    const exported = recorder?.export();
    if (exported) {
      ctx.postMessage({ type: 'recordingExport', export: exported });
    }
  } else if (message.type === 'loadReplay') {
    loadReplay(message.bytes);
  } else if (message.type === 'playReplay') {
    playReplay();
  } else if (message.type === 'pauseReplay') {
    pauseReplay();
  } else if (message.type === 'seekReplay') {
    seekReplay(message.cursorMs);
  } else if (message.type === 'setReplaySpeed') {
    replaySpeed = message.speed;
    postReplay();
  }
});

function connect(nextMode: RuntimeMode, url: string): void {
  disconnect(false);
  mode = nextMode;
  state = createInitialVehicleState(vehicleId);
  resetPlotState();
  startStaleTimer();

  if (nextMode === 'zenoh') {
    zenoh = new ZenohWasmTransport(url, handleTransportMessage, postConnection, postCatalog, {
      vehicleId,
      wasmUrl: zenohWasmUrl
    });
    zenoh.connect().catch((error) => {
      state = appendEvent(state, {
        severity: 'error',
        code: 'zenoh_connect',
        message: error instanceof Error ? error.message : String(error),
        timestampMs: Date.now()
      });
      postState();
    });
    return;
  }

  if (nextMode === 'replay') {
    if (replayFrames.length === 0) {
      loadReplay();
    }
    playReplay();
    return;
  }
}

function disconnect(post = true): void {
  zenoh?.disconnect().catch(() => {});
  zenoh = null;
  clearTimer(staleTimer);
  clearTimer(replayTimer);
  staleTimer = null;
  replayTimer = null;
  replayPlaying = false;
  state.connected = false;

  if (post) {
    postConnection({ mode, status: 'disconnected', url: '', message: 'disconnected' });
    postState();
    postReplay();
  }
}

function handleTransportMessage(message: TransportMessage): void {
  if (message.kind === 'telemetry' || message.kind === 'event') {
    applyFrames([message]);
    return;
  }

  if (message.kind === 'commandAck') {
    applyCommandResult(message as CommandResult);
  }
}

function sendCommand(command: CommandName, args: Record<string, unknown>): void {
  const sequence = commandSequence++;
  const intent = buildCommandIntent({ vehicleId, command, args, sequence });
  const failures = validateCommandPreconditions(state, command);
  pendingCommandArgs.set(intent.commandId, args);

  if (failures.length > 0) {
    applyCommandResult(rejectedCommandResult(intent, failures.join(', ')));
    return;
  }

  if (mode === 'zenoh' && zenoh) {
    zenoh
      .sendCommand(intent)
      .then(() => {
        applyCommandResult({
          kind: 'commandAck',
          commandId: intent.commandId,
          command,
          status: 'published',
          reason: `published to ${intent.topic}`,
          sequence,
          receivedAtMs: Date.now()
        });
      })
      .catch((error) => {
        applyCommandResult(rejectedCommandResult(intent, error instanceof Error ? error.message : String(error)));
      });
    return;
  }

  setTimeout(() => {
    applyCommandResult({
      kind: 'commandAck',
      commandId: intent.commandId,
      command,
      status: 'acked',
      reason: 'simulated acknowledgement',
      sequence,
      receivedAtMs: Date.now()
    });
  }, 220);
}

function applyFrames(frames: GcsFrame[]): void {
  for (const frame of frames) {
    state = applyGcsFrame(state, frame);
    updatePlotSeries(frame);
    if (recording) {
      recorder?.recordFrame(frame);
    }
  }

  postState();
  postPlotState();
  if (recording) {
    ctx.postMessage({ type: 'recording', recording, count: recorder?.frameCount ?? 0 });
  }
}

function applyCommandResult(result: CommandResult): void {
  if (result.status === 'acked' || result.status === 'published') {
    const args = pendingCommandArgs.get(result.commandId) ?? {};
    if (result.command === 'arm') {
      state.mode = { ...state.mode, armed: true };
    } else if (result.command === 'disarm') {
      state.mode = { ...state.mode, armed: false };
    } else if (result.command === 'setMode') {
      state.mode = { ...state.mode, name: typeof args.mode === 'string' ? args.mode : state.mode.name };
    } else if (result.command === 'land') {
      state.mode = { ...state.mode, name: 'land' };
    } else if (result.command === 'return') {
      state.mode = { ...state.mode, name: 'return' };
    }
  }

  pendingCommandArgs.delete(result.commandId);
  state.commandHistory = [result, ...state.commandHistory].slice(0, 48);
  state = appendEvent(state, {
    severity: result.status === 'acked' ? 'info' : 'warning',
    code: `cmd_${result.status}`,
    message: `${result.command}: ${result.reason}`,
    timestampMs: result.receivedAtMs
  });
  ctx.postMessage({ type: 'commandResult', result });
  postState();
}

function loadReplay(bytes?: Uint8Array): void {
  clearTimer(replayTimer);
  resetPlotState(false);
  try {
    const decodedFrames = bytes && bytes.length > 0 ? decodeSynapseLogFrames(bytes) : [];
    replayFrames = normalizeReplayFrames(decodedFrames.length > 0 ? decodedFrames : createDemoReplay(vehicleId));
  } catch (error) {
    replayFrames = normalizeReplayFrames(createDemoReplay(vehicleId));
    state = appendEvent(state, {
      severity: 'error',
      code: 'replay_load',
      message: error instanceof Error ? error.message : String(error),
      timestampMs: Date.now()
    });
  }
  replayCursorMs = 0;
  replayPlaying = false;
  mode = 'replay';
  state = createInitialVehicleState(vehicleId);
  postConnection({ mode: 'replay', status: 'connected', url: 'replay://memory', message: 'replay loaded' });
  postReplay();
  postState();
  postPlotState(true);
}

function playReplay(): void {
  if (replayFrames.length === 0) {
    loadReplay();
  }

  replayPlaying = true;
  postReplay();
  let lastTickMs = Date.now();
  clearTimer(replayTimer);
  replayTimer = setInterval(() => {
    const nowMs = Date.now();
    replayCursorMs += (nowMs - lastTickMs) * replaySpeed;
    lastTickMs = nowMs;
    seekReplay(replayCursorMs, true);

    if (replayCursorMs >= replayDurationMs(replayFrames)) {
      pauseReplay();
    }
  }, 50);
}

function pauseReplay(): void {
  replayPlaying = false;
  clearTimer(replayTimer);
  replayTimer = null;
  postReplay();
}

function seekReplay(cursorMs: number, fromPlayback = false): void {
  replayCursorMs = Math.max(0, Math.min(cursorMs, replayDurationMs(replayFrames)));
  const frames = framesThroughCursor(replayFrames, replayCursorMs);
  state = createInitialVehicleState(vehicleId);
  plotSeries = new Map();
  for (const frame of frames) {
    state = applyGcsFrame(state, frame);
    updatePlotSeries(frame);
  }

  if (!fromPlayback) {
    pauseReplay();
  }

  postReplay();
  postState();
  postPlotState(true);
}

function startStaleTimer(): void {
  staleTimer = setInterval(() => {
    state = refreshStaleTopics(state);
    postState();
  }, 500);
}

function postState(): void {
  ctx.postMessage({ type: 'state', state });
}

function postConnection(connection: ConnectionState): void {
  ctx.postMessage({ type: 'connection', connection });
}

function postCatalog(catalog: TopicCatalog): void {
  ctx.postMessage({ type: 'topicCatalog', catalog });
}

function postReplay(): void {
  ctx.postMessage({
    type: 'replay',
    replay: {
      loaded: replayFrames.length > 0,
      playing: replayPlaying,
      cursorMs: replayCursorMs,
      durationMs: replayDurationMs(replayFrames),
      speed: replaySpeed,
      frameCount: replayFrames.length
    }
  });
}

function resetPlotState(post = true): void {
  plotCatalog = createPlotPacketCatalog(vehicleId);
  plotSeries = new Map();
  lastPlotPostMs = 0;
  if (post) {
    postPlotState(true);
  }
}

function updatePlotSeries(frame: GcsFrame): void {
  for (const update of extractPlotSeriesUpdates(frame)) {
    const key = plotSeriesKey(update.packetKey, update.field.path);
    const current =
      plotSeries.get(key) ??
      ({
        key,
        packetKey: update.packetKey,
        topic: update.topic,
        schema: update.schema,
        packetLabel: update.packetLabel,
        fieldPath: update.field.path,
        fieldLabel: update.field.label,
        units: update.field.units,
        valueType: update.field.valueType,
        samples: [],
        lastValue: null,
        updatedAtMs: 0
      } satisfies PlotSeries);

    current.samples = [...current.samples, { timeMs: update.timeMs, value: update.value }].slice(-MAX_PLOT_SAMPLES);
    current.lastValue = update.value;
    current.updatedAtMs = Date.now();
    plotSeries.set(key, current);
  }
}

function postPlotState(force = false): void {
  const nowMs = Date.now();
  if (!force && nowMs - lastPlotPostMs < PLOT_POST_INTERVAL_MS) {
    return;
  }

  lastPlotPostMs = nowMs;
  const packets = mergePlotPackets();
  ctx.postMessage({
    type: 'plotState',
    plotState: {
      packets,
      series: [...plotSeries.values()].sort((a, b) => a.key.localeCompare(b.key)),
      updatedAtMs: nowMs
    }
  });
}

function mergePlotPackets(): PlotPacketDefinition[] {
  const packets = new Map<string, PlotPacketDefinition>();

  for (const packet of plotCatalog) {
    packets.set(packet.key, { ...packet, fields: [...packet.fields], active: false, samples: 0 });
  }

  for (const series of plotSeries.values()) {
    const packet =
      packets.get(series.packetKey) ??
      ({
        key: series.packetKey,
        topic: series.topic,
        schema: series.schema,
        label: series.packetLabel,
        source: 'live',
        fields: [],
        active: false,
        samples: 0
      } satisfies PlotPacketDefinition);

    packet.active = true;
    packet.samples += series.samples.length;
    if (!packet.fields.some((field) => field.path === series.fieldPath)) {
      packet.fields.push({
        path: series.fieldPath,
        label: series.fieldLabel,
        units: series.units,
        valueType: series.valueType
      });
    }
    packets.set(series.packetKey, packet);
  }

  return [...packets.values()].sort((a, b) => {
    if (a.active !== b.active) {
      return a.active ? -1 : 1;
    }
    if (a.source !== b.source) {
      return a.source.localeCompare(b.source);
    }
    return a.label.localeCompare(b.label);
  });
}

function clearTimer(timer: ReturnType<typeof setInterval> | null): void {
  if (timer) {
    clearInterval(timer);
  }
}

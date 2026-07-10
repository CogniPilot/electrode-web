import {
  applyGcsFrame,
  appendEvent,
  createDemoReplay,
  createInitialVehicleState,
  setMocapDisplaySource,
  framesThroughCursor,
  normalizeReplayFrames,
  refreshStaleTopics,
  replayDurationMs,
  createPlotPacketCatalog,
  decodeSynapseLogFrames,
  extractPlotSeriesUpdates,
  plotSeriesKey,
  SynapseLogRecorder,
  ZenohWasmTransport,
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
  | { type: 'setSubscriptions'; keys: string[] }
  | { type: 'setMocapDisplaySource'; source: 'auto' | 'raw' | 'external' }
  | { type: 'virtualManual'; enabled: boolean; input?: VirtualManualInput }
  | { type: 'startRecording' }
  | { type: 'stopRecording' }
  | { type: 'exportRecording' }
  | { type: 'loadReplay'; bytes?: Uint8Array }
  | { type: 'playReplay' }
  | { type: 'pauseReplay' }
  | { type: 'seekReplay'; cursorMs: number }
  | { type: 'setReplaySpeed'; speed: number };

type VirtualManualInput = {
  roll: number;
  pitch: number;
  yaw: number;
  throttle: number;
  flightMode: number;
  armSwitch: boolean;
  killSwitch: boolean;
  active: boolean;
};

const ctx = self as unknown as DedicatedWorkerGlobalScope;
const MAX_PLOT_SAMPLES = 240;
const PLOT_POST_INTERVAL_MS = 250;
const VIRTUAL_MANUAL_TOPIC = 'gcs/v1/cmd/manual';
const VIRTUAL_MANUAL_PERIOD_MS = 20;
const MANUAL_AXIS_MASK = 0x03ff;
const MANUAL_FLAG_ARM = 1;
const MANUAL_FLAG_KILL = 2;
const MANUAL_FLAG_ACTIVE = 4;
const MANUAL_FLAG_VALID = 8;

let vehicleId = 'electrode-01';
let state: VehicleState = createInitialVehicleState(vehicleId);
let mode: RuntimeMode = 'zenoh';
let zenoh: ZenohWasmTransport | null = null;
let staleTimer: ReturnType<typeof setInterval> | null = null;
let replayTimer: ReturnType<typeof setInterval> | null = null;
let recording = false;
let recorder: SynapseLogRecorder | null = null;
let replayFrames: GcsFrame[] = [];
let replayCursorMs = 0;
let replaySpeed = 1;
let replayPlaying = false;
let plotCatalog: PlotPacketDefinition[] = createPlotPacketCatalog(vehicleId);
let plotSeries = new Map<string, PlotSeries>();
let lastPlotPostMs = 0;
let virtualManualEnabled = false;
let virtualManualTimer: ReturnType<typeof setInterval> | null = null;
let virtualManualInput: VirtualManualInput = {
  roll: 0,
  pitch: 0,
  yaw: 0,
  throttle: 0.5,
  flightMode: 1,
  armSwitch: true,
  killSwitch: false,
  active: true
};

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
  } else if (message.type === 'setSubscriptions') {
    zenoh?.setSubscriptions(message.keys);
  } else if (message.type === 'setMocapDisplaySource') {
    state = setMocapDisplaySource(state, message.source);
    postState();
  } else if (message.type === 'virtualManual') {
    setVirtualManual(message.enabled, message.input);
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
      void postRecordingExport(recorder);
    }
  } else if (message.type === 'exportRecording') {
    void postRecordingExport(recorder);
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

async function postRecordingExport(source: SynapseLogRecorder | null): Promise<void> {
  const exported = await source?.export();
  if (exported) {
    ctx.postMessage({ type: 'recordingExport', export: exported });
  }
}

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
  stopVirtualManualTimer();
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

function setVirtualManual(enabled: boolean, input?: VirtualManualInput): void {
  virtualManualEnabled = enabled;
  if (input) {
    virtualManualInput = { ...input };
  }
  if (enabled) {
    startVirtualManualTimer();
    publishVirtualManual();
  } else {
    stopVirtualManualTimer();
  }
}

function startVirtualManualTimer(): void {
  if (virtualManualTimer) return;
  virtualManualTimer = setInterval(publishVirtualManual, VIRTUAL_MANUAL_PERIOD_MS);
}

function stopVirtualManualTimer(): void {
  clearTimer(virtualManualTimer);
  virtualManualTimer = null;
}

function publishVirtualManual(): void {
  if (!virtualManualEnabled || !zenoh) return;
  const payload = encodeManualControl(virtualManualInput);
  zenoh.publishBytes(VIRTUAL_MANUAL_TOPIC, payload).catch(() => {});
}

function encodeManualControl(input: VirtualManualInput): Uint8Array {
  const payload = new Uint8Array(40);
  const view = new DataView(payload.buffer);
  view.setBigUint64(0, BigInt(Date.now()) * 1000n, true);
  view.setUint32(8, 0, true);
  view.setUint16(12, MANUAL_AXIS_MASK, true);
  view.setInt16(14, toMilli(input.pitch), true);
  view.setInt16(16, toMilli(input.roll), true);
  view.setInt16(18, toMilli(input.throttle), true);
  view.setInt16(20, toMilli(input.yaw), true);
  for (let offset = 22; offset <= 32; offset += 2) {
    view.setInt16(offset, 0, true);
  }
  view.setUint8(34, input.flightMode);
  let flags = MANUAL_FLAG_VALID;
  if (input.armSwitch) flags |= MANUAL_FLAG_ARM;
  if (input.killSwitch) flags |= MANUAL_FLAG_KILL;
  if (input.active) flags |= MANUAL_FLAG_ACTIVE;
  view.setUint8(35, flags);
  return payload;
}

function toMilli(value: number): number {
  const bounded = Math.min(1, Math.max(-1, Number.isFinite(value) ? value : 0));
  return Math.round(bounded * 1000);
}

function handleTransportMessage(message: TransportMessage): void {
  if (message.kind === 'telemetry' || message.kind === 'event') {
    applyFrames([message]);
  }
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

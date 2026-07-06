<script lang="ts">
  import { asset } from '$app/paths';
  import { onMount } from 'svelte';
  import IndoorScene from '$lib/components/IndoorScene.svelte';
  import DeflectionView from '$lib/components/DeflectionView.svelte';
  import ManualLinkView from '$lib/components/ManualLinkView.svelte';
  import GroundStationPanel from '$lib/components/GroundStationPanel.svelte';
  import AutopilotConfigPanel from '$lib/components/AutopilotConfigPanel.svelte';
  import RcMappingPanel from '$lib/components/RcMappingPanel.svelte';
  import PpmHardwarePanel from '$lib/components/PpmHardwarePanel.svelte';
  import SimulationPanel from '$lib/components/SimulationPanel.svelte';
  import { detectGroundStation, isGroundStation } from '$lib/capabilities';
  import {
    fetchDevices,
      fetchAutopilotRunStatus,
      fetchBridgeStatus,
      setAutopilotRunning,
      setBridgeRunning,
      type AutopilotRunStatus
    } from '$lib/gcs';
  import {
    Activity,
    AlertTriangle,
    CirclePause,
    CirclePlay,
    Download,
    Gauge,
    MapPinned,
    Moon,
    Power,
    Radio,
    RotateCcw,
    Settings,
    Square,
    Sun,
    Upload
  } from '@lucide/svelte';
  import {
    TOPIC_DEFINITIONS,
    createPlotPacketCatalog,
    createInitialVehicleState,
    plotPacketKey,
    type ConnectionState,
    type MissionWaypoint,
    type PlotFieldDefinition,
    type PlotPacketDefinition,
    type PlotSeries,
    type PlotState,
    type ReplayState,
    type RuntimeMode,
    type SynapseLogExport,
    type TopicCatalog,
    type TopicSnapshot,
    type VehicleState
  } from '@electrode/sdk';

  type WorkerOut =
    | { type: 'state'; state: VehicleState }
    | { type: 'connection'; connection: ConnectionState }
    | { type: 'recording'; recording: boolean; count: number }
    | { type: 'recordingExport'; export: SynapseLogExport }
    | { type: 'plotState'; plotState: PlotState }
    | { type: 'topicCatalog'; catalog: TopicCatalog }
    | { type: 'replay'; replay: ReplayState };

  type MapViewMode = '2d' | '3d';
  type VehicleKind = 'quadrotor' | 'fixedwing';
  type GroundStationPage = 'dashboard' | 'sim';
  type PlotTraceSelection = {
    packetKey: string;
    fieldPath: string;
  };

  type ResolvedPlotTrace = PlotTraceSelection & {
    color: string;
    packet: PlotPacketDefinition | undefined;
    field: PlotFieldDefinition | undefined;
    series: PlotSeries | undefined;
    path: string;
    lastValue: number | null;
    rangeLabel: string;
  };

  const vehicleId = 'electrode-01';
  const mapViewModes: MapViewMode[] = ['2d', '3d'];
  const groundStationPages: Array<{ key: GroundStationPage; label: string }> = [
    { key: 'dashboard', label: 'Dashboard' },
    { key: 'sim', label: 'SIM' }
  ];
  type ThemeName = 'light' | 'dark';
  const manualLinks = [
    ['Manual', 'synapse/v1/topic/manual_control_command'],
    ['Selected PWM', 'synapse/motor_output'],
    ['Serial', '/dev/ttyACM0 · 57600']
  ] as const;
  const rollTicks = [-60, -45, -30, -20, -10, 0, 10, 20, 30, 45, 60];

  // Head-up display geometry (SVG user units, 560x500 viewBox).
  const HUD_CX = 280;
  const HUD_CY = 250;
  const HUD_PX_PER_DEG_HDG = 4.6;
  const HUD_PX_PER_DEG_PITCH = 4.2;

  function hudHeadingLabel(nd: number): string {
    if (nd === 0) return 'N';
    if (nd === 90) return 'E';
    if (nd === 180) return 'S';
    if (nd === 270) return 'W';
    return String(Math.round(nd / 10));
  }
  function buildHeadingTicks(yaw: number) {
    const ticks: { x: number; label: string; major: boolean }[] = [];
    const base = Math.round(yaw / 5) * 5;
    for (let off = -30; off <= 30; off += 5) {
      const deg = base + off;
      const nd = ((deg % 360) + 360) % 360;
      const dx = ((deg - yaw + 540) % 360) - 180;
      const major = nd % 10 === 0;
      ticks.push({ x: HUD_CX + dx * HUD_PX_PER_DEG_HDG, label: major ? hudHeadingLabel(nd) : '', major });
    }
    return ticks;
  }
  function buildPitchRungs(pitch: number): number[] {
    const rungs: number[] = [];
    const lo = Math.ceil((pitch - 40) / 10) * 10;
    const hi = Math.floor((pitch + 40) / 10) * 10;
    for (let m = lo; m <= hi; m += 10) {
      if (m === 0 || m < -90 || m > 90) continue;
      rungs.push(m);
    }
    return rungs;
  }

  function centeredToPwm(value: number): number {
    return Math.round(clamp(value, -1, 1) * 500 + 1500);
  }

  function throttleToPwm(value: number): number {
    return Math.round(clamp(value, 0, 1) * 1000 + 1000);
  }

  function centeredFromPwm(value: number): number {
    return clamp((value - 1500) / 500, -1, 1);
  }

  function throttleFromPwm(value: number): number {
    return clamp((value - 1000) / 1000, 0, 1);
  }

  function manualControlPwm(manual: VehicleState['manualControl']): number[] | null {
    if (!manual || !manual.valid || manual.killSwitch) return null;
    return [
      centeredToPwm(manual.roll),
      centeredToPwm(manual.pitch),
      throttleToPwm(manual.throttle),
      centeredToPwm(-manual.yaw)
    ];
  }

  function pwmControlInputs(pwm: number[] | null | undefined): VehicleState['controls'] {
    if (!pwm || pwm.length < 4) return null;
    return {
      aileron: centeredFromPwm(pwm[0]),
      elevator: centeredFromPwm(pwm[1]),
      throttle: throttleFromPwm(pwm[2]),
      rudder: centeredFromPwm(pwm[3])
    };
  }

  let worker: Worker | null = null;
  let theme: ThemeName = 'dark';
  let activePage: GroundStationPage = initialGroundStationPage();
  let runtimeMode: RuntimeMode = 'zenoh';
  let mapViewMode: MapViewMode = initialMapViewMode();
  let selectedVehicleType: VehicleKind = 'fixedwing';
  let zenohEndpoint = 'ws/127.0.0.1:7447';
  let vehicle = createInitialVehicleState(vehicleId);
  let replay: ReplayState = { loaded: false, playing: false, cursorMs: 0, durationMs: 0, speed: 1, frameCount: 0 };
  let recording = false;
  let recordingCount = 0;
  let plotState: PlotState = { packets: createPlotPacketCatalog(vehicleId), series: [], updatedAtMs: 0 };
  let topicCatalog: TopicCatalog | null = null;
  let plotTraces: PlotTraceSelection[] = [
    { packetKey: plotPacketKey(`vehicle/${vehicleId}/state/pose`, 'Pose'), fieldPath: 'altM' },
    { packetKey: plotPacketKey(`vehicle/${vehicleId}/state/velocity`, 'Velocity'), fieldPath: 'groundSpeedMps' }
  ];

  $: topicRows = Object.values(vehicle.topics).sort((a, b) => a.label.localeCompare(b.label));
  $: warningCount = vehicle.events.filter((event) => event.severity !== 'info').length;
  $: pose = vehicle.pose;
  $: attitude = vehicle.attitude;
  $: controls = vehicle.controls;
  $: manualControl = vehicle.manualControl;
  $: radioControl = vehicle.radioControl;
  $: motors = vehicle.motors;
  $: link = vehicle.link;
  $: mission = vehicle.mission;
  // 2D map projection of the mission plan, using the same metres→percent
  // mapping as the vehicle marker.
  $: missionWaypoints = (mission?.waypoints ?? [])
    .filter((wp): wp is MissionWaypoint => wp !== null)
    .map((wp) => ({
      ...wp,
      mapX: clamp(50 + wp.east * 0.72, 8, 92),
      mapY: clamp(50 - wp.north * 0.72, 8, 92)
    }));
  $: missionPathPoints = missionWaypoints.map((wp) => `${wp.mapX},${wp.mapY}`).join(' ');
  $: mapSubtitle =
    mapViewMode === '2d'
      ? pose
        ? `${pose.lat.toFixed(6)}, ${pose.lon.toFixed(6)}`
        : 'no fix'
      : `${vehicle.localization.source} · local ${format(pose?.xM)} / ${format(pose?.yM)} m`;
  $: mapX = clamp(50 + (pose?.xM ?? 0) * 0.72, 8, 92);
  $: mapY = clamp(50 - (pose?.yM ?? 0) * 0.72, 8, 92);
  $: yawDeg = attitude?.yawDeg ?? 0;
  $: rollDeg = attitude?.rollDeg ?? 0;
  $: pitchDeg = attitude?.pitchDeg ?? 0;

  // State I/O panel: live per-topic snapshots (rate + staleness) for the
  // state bus around the autopilot, plant, mocap, and manual controller.
  function topicBySuffix(state: VehicleState, suffix: string): TopicSnapshot | null {
    for (const snapshot of Object.values(state.topics)) {
      if (snapshot.topic.endsWith(suffix)) return snapshot;
    }
    return null;
  }
  $: ioHealth = topicBySuffix(vehicle, 'vehicle_health');
  $: ioAttitude = topicBySuffix(vehicle, 'attitude_estimate');
  $: ioSelectedPwm = topicBySuffix(vehicle, 'motor_output');
  $: ioPwm = ioSelectedPwm && !ioSelectedPwm.stale ? ioSelectedPwm : topicBySuffix(vehicle, 'pwm_signal_outputs');
  $: ioManual = topicBySuffix(vehicle, 'manual_control_command');
  $: ioMocap = topicBySuffix(vehicle, 'mocap_frame') ?? topicBySuffix(vehicle, 'pose');
  $: requestedControlSource = manualControl
    ? manualControl.valid
      ? manualControl.flightMode > 0
        ? 'autopilot'
        : manualControl.flightMode === 0
          ? 'manual'
          : `mode ${manualControl.flightMode}`
      : 'failsafe'
    : 'unknown';
  $: requestedControlSourceDetail = manualControl
    ? `${manualControl.active ? 'stabilization on' : 'stabilization off'} · mode ${manualControl.flightMode}`
    : 'waiting for manual_control_command';
  $: autopilotReportedMode = vehicle.mode.name;
  $: controlModeMismatch =
    manualControl?.valid === true &&
    ((manualControl.flightMode > 0 && autopilotReportedMode !== 'auto') ||
      (manualControl.flightMode === 0 && autopilotReportedMode === 'auto'));
  $: manualPwm = requestedControlSource === 'manual' ? manualControlPwm(manualControl) : null;
  $: displayedPwm = motors && motors.length >= 4 ? motors.slice(0, 4) : manualPwm;
  $: displayedPwmLabel = ioSelectedPwm ? 'Selected PWM' : manualPwm ? 'Manual PWM' : 'Autopilot PWM';
  $: deflectionControls = requestedControlSource === 'manual' ? controls : pwmControlInputs(displayedPwm);

  function ioRate(snapshot: TopicSnapshot | null): string {
    if (!snapshot) return 'no data';
    if (snapshot.stale) return 'stale';
    return `${snapshot.rateHz.toFixed(1)} Hz`;
  }
  $: hudSpeedMps = vehicle.velocity?.groundSpeedMps ?? 0;
  $: hudAltMeters = pose?.altM ?? 0;
  $: hudClimbMps = -(vehicle.velocity?.downMps ?? 0);
  $: hudQualityPct = vehicle.localization.quality * 100;
  $: hudHeadingText = String(Math.round(((yawDeg % 360) + 360) % 360) % 360).padStart(3, '0');
  $: headingTicks = buildHeadingTicks(yawDeg);
  $: pitchRungs = buildPitchRungs(pitchDeg);
  $: horizonY = HUD_CY + pitchDeg * HUD_PX_PER_DEG_PITCH;
  $: staleTopics = topicRows.filter((topic) => topic.stale).length;
  $: plotPackets = plotState.packets;
  $: {
    const nextPlotTraces = normalizePlotTraces(plotTraces, plotPackets);
    if (!samePlotTraces(nextPlotTraces, plotTraces)) {
      plotTraces = nextPlotTraces;
    }
  }
  $: plotTraceColors = theme === 'dark' ? ['#fd7719', '#f4fbf7'] : ['#e35f0c', '#12171b'];
  $: resolvedPlotTraces = plotTraces.map((trace, index) =>
    resolvePlotTrace(trace, plotTraceColors[index] ?? '#fd7719', plotState, plotPackets)
  );
  $: plotHasSamples = resolvedPlotTraces.some((trace) => trace.series && trace.series.samples.length > 1);

  // Daemon-supervised native autopilot (cubs2 native_sim + Zenoh link).
  let autopilotRun: AutopilotRunStatus | null = null;
  let autopilotBusy = false;
  let autopilotError = '';
  let manualBridgeRunning = false;
  let manualBridgeBusy = false;
  let manualBridgeStatus = 'checking...';
  let joystickPresent = false;
  let keyboardRevision = 0;
  let lastVirtualManualSignature = '';
  let keyboardRoll = 0;
  let keyboardPitch = 0;
  let keyboardYaw = 0;
  let keyboardThrottle = 0.55;
  let keyboardAuto = true;
  let keyboardStabilize = true;
  const pressedKeys = new Set<string>();
  const KEYBOARD_STICK_STEP = 0.12;
  const KEYBOARD_THROTTLE_STEP = 0.05;
  $: autopilotRunning = autopilotRun?.running ?? false;
  $: keyboardRemoteActive = $isGroundStation && !manualBridgeRunning && !joystickPresent;
  $: keyboardRemoteStatus = keyboardRemoteActive
    ? 'keyboard remote active'
    : joystickPresent
      ? 'hardware remote detected'
      : manualBridgeRunning
        ? 'hardware publisher active'
        : 'keyboard remote idle';
  $: syncKeyboardRemote(keyboardRemoteActive, keyboardRevision);

  async function refreshAutopilotRun(): Promise<void> {
    if (!$isGroundStation) return;
    try {
      autopilotRun = await fetchAutopilotRunStatus();
    } catch {
      // Backend briefly unreachable; keep the last status.
    }
  }

  async function toggleAutopilot(): Promise<void> {
    if (autopilotBusy) return;
    autopilotBusy = true;
    autopilotError = '';
    try {
      autopilotRun = await setAutopilotRunning(autopilotRunning ? 'stop' : 'start');
    } catch (error) {
      autopilotError = error instanceof Error ? error.message : String(error);
    } finally {
      autopilotBusy = false;
    }
  }

  async function refreshManualBridge(): Promise<void> {
    if (!$isGroundStation) return;
    try {
      const [status, devices] = await Promise.all([fetchBridgeStatus(), fetchDevices()]);
      manualBridgeRunning = status.running;
      joystickPresent = devices.joysticks.length > 0;
      manualBridgeStatus = status.running
        ? `manual publisher active · ${status.bin}`
        : status.ppmRunning
          ? 'manual publisher stopped · PPM hardware running'
          : 'stopped';
    } catch {
      manualBridgeStatus = 'manual bridge unavailable';
    }
  }

  async function toggleManualBridge(): Promise<void> {
    if (manualBridgeBusy) return;
    manualBridgeBusy = true;
    try {
      const status = await setBridgeRunning(!manualBridgeRunning);
      manualBridgeRunning = status.running;
      manualBridgeStatus = status.running
        ? `manual publisher active · ${status.bin}`
        : 'stopped';
    } catch (error) {
      manualBridgeStatus = error instanceof Error ? error.message : 'manual bridge toggle failed';
    } finally {
      manualBridgeBusy = false;
    }
  }

  function syncKeyboardRemote(enabled: boolean, _revision: number): void {
    if (!worker) return;
    const input = keyboardManualInput();
    const signature = `${enabled}:${input.roll}:${input.pitch}:${input.yaw}:${input.throttle}:${input.flightMode}:${input.armSwitch}:${input.killSwitch}:${input.active}`;
    if (signature === lastVirtualManualSignature) return;
    lastVirtualManualSignature = signature;
    worker.postMessage({ type: 'virtualManual', enabled, input });
  }

  function keyboardManualInput() {
    return {
      roll: keyboardRoll,
      pitch: keyboardPitch,
      yaw: keyboardYaw,
      throttle: keyboardThrottle,
      flightMode: keyboardAuto ? 1 : 0,
      armSwitch: true,
      killSwitch: false,
      active: keyboardStabilize
    };
  }

  function keyboardTargetIsEditable(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    const tag = target.tagName.toLowerCase();
    return tag === 'input' || tag === 'textarea' || tag === 'select' || target.isContentEditable;
  }

  function handleKeyboardRemote(event: KeyboardEvent, down: boolean): void {
    if (!keyboardRemoteActive || keyboardTargetIsEditable(event.target)) return;
    const keys = ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'KeyW', 'KeyA', 'KeyS', 'KeyD', 'KeyM', 'KeyT'];
    if (!keys.includes(event.code)) return;
    event.preventDefault();
    const hadKey = pressedKeys.has(event.code);
    if (down) {
      pressedKeys.add(event.code);
      if (!hadKey) {
        if (event.code === 'ArrowLeft') keyboardRoll = clamp(keyboardRoll - KEYBOARD_STICK_STEP, -1, 1);
        if (event.code === 'ArrowRight') keyboardRoll = clamp(keyboardRoll + KEYBOARD_STICK_STEP, -1, 1);
        if (event.code === 'ArrowDown') keyboardPitch = clamp(keyboardPitch - KEYBOARD_STICK_STEP, -1, 1);
        if (event.code === 'ArrowUp') keyboardPitch = clamp(keyboardPitch + KEYBOARD_STICK_STEP, -1, 1);
        if (event.code === 'KeyA') keyboardYaw = clamp(keyboardYaw - KEYBOARD_STICK_STEP, -1, 1);
        if (event.code === 'KeyD') keyboardYaw = clamp(keyboardYaw + KEYBOARD_STICK_STEP, -1, 1);
        if (event.code === 'KeyS') keyboardThrottle = clamp(keyboardThrottle - KEYBOARD_THROTTLE_STEP, 0, 1);
        if (event.code === 'KeyW') keyboardThrottle = clamp(keyboardThrottle + KEYBOARD_THROTTLE_STEP, 0, 1);
        if (event.code === 'KeyM') keyboardAuto = !keyboardAuto;
        if (event.code === 'KeyT') keyboardStabilize = !keyboardStabilize;
      }
    } else {
      pressedKeys.delete(event.code);
    }
    if (hadKey !== down || (down && !hadKey)) {
      keyboardRevision += 1;
    }
  }

  onMount(() => {
    const stored = document.documentElement.dataset.theme;
    theme = stored === 'light' ? 'light' : 'dark';

    // Unlock Ground Station (hardware) features when a local backend answers on
    // this origin; otherwise this is the display-only Viewer.
    void detectGroundStation().then(() => {
      void refreshAutopilotRun();
      void refreshManualBridge();
    });
    const autopilotTimer = setInterval(() => void refreshAutopilotRun(), 2000);
    const manualBridgeTimer = setInterval(() => void refreshManualBridge(), 2000);
    const keydown = (event: KeyboardEvent) => handleKeyboardRemote(event, true);
    const keyup = (event: KeyboardEvent) => handleKeyboardRemote(event, false);
    window.addEventListener('keydown', keydown);
    window.addEventListener('keyup', keyup);

    worker = new Worker(new URL('$lib/workers/comms.worker.ts', import.meta.url), { type: 'module' });
    worker.addEventListener('message', (event: MessageEvent<WorkerOut>) => {
      const message = event.data;

      if (message.type === 'state') {
        vehicle = message.state;
      } else if (message.type === 'recording') {
        recording = message.recording;
        recordingCount = message.count;
      } else if (message.type === 'recordingExport') {
        exportRecordingFile(message.export);
      } else if (message.type === 'plotState') {
        plotState = message.plotState;
      } else if (message.type === 'topicCatalog') {
        topicCatalog = message.catalog;
      } else if (message.type === 'replay') {
        replay = message.replay;
      }
    });

    syncKeyboardRemote(keyboardRemoteActive, keyboardRevision);
    connect();

    return () => {
      clearInterval(autopilotTimer);
      clearInterval(manualBridgeTimer);
      window.removeEventListener('keydown', keydown);
      window.removeEventListener('keyup', keyup);
      worker?.postMessage({ type: 'virtualManual', enabled: false });
      worker?.postMessage({ type: 'disconnect' });
      worker?.terminate();
      worker = null;
    };
  });

  function toggleTheme(): void {
    theme = theme === 'dark' ? 'light' : 'dark';
    document.documentElement.dataset.theme = theme;
    try {
      localStorage.setItem('electrode-theme', theme);
    } catch {
      // ignore storage failures (private mode, etc.)
    }
  }

  function toggleThemeFromMenu(event: MouseEvent): void {
    toggleTheme();
    if (event.currentTarget instanceof HTMLElement) {
      event.currentTarget.closest('details')?.removeAttribute('open');
    }
  }

  function connect(): void {
    const url = zenohEndpoint;
    worker?.postMessage({ type: 'connect', mode: runtimeMode, url, vehicleId });
  }

  function initialMapViewMode(): MapViewMode {
    if (typeof window === 'undefined') {
      return '3d';
    }

    const requested = new URLSearchParams(window.location.search).get('map') as MapViewMode | null;
    return requested && mapViewModes.includes(requested) ? requested : '3d';
  }

  function initialGroundStationPage(): GroundStationPage {
    if (typeof window === 'undefined') {
      return 'dashboard';
    }

    const requested = new URLSearchParams(window.location.search).get('page') as GroundStationPage | null;
    return requested && groundStationPages.some((page) => page.key === requested) ? requested : 'dashboard';
  }

  function setGroundStationPage(page: GroundStationPage): void {
    activePage = page;
    if (typeof window === 'undefined') {
      return;
    }

    const url = new URL(window.location.href);
    if (page === 'dashboard') {
      url.searchParams.delete('page');
    } else {
      url.searchParams.set('page', page);
    }
    window.history.replaceState({}, '', url);
  }

  function toggleTopicSubscription(key: string): void {
    if (!topicCatalog) {
      return;
    }
    const selected = new Set(topicCatalog.topics.filter((topic) => topic.selected).map((topic) => topic.key));
    if (selected.has(key)) {
      selected.delete(key);
    } else {
      selected.add(key);
    }
    worker?.postMessage({ type: 'setSubscriptions', keys: [...selected] });
  }

  function startRecording(): void {
    worker?.postMessage({ type: 'startRecording' });
  }

  function stopRecording(): void {
    worker?.postMessage({ type: 'stopRecording' });
  }

  function exportRecording(): void {
    worker?.postMessage({ type: 'exportRecording' });
  }

  function loadReplay(event: Event): void {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];

    if (!file) {
      worker?.postMessage({ type: 'loadReplay' });
      return;
    }

    const reader = new FileReader();
    reader.addEventListener('load', () => {
      if (!(reader.result instanceof ArrayBuffer)) {
        return;
      }
      const bytes = new Uint8Array(reader.result);
      worker?.postMessage({ type: 'loadReplay', bytes }, [bytes.buffer]);
    });
    reader.readAsArrayBuffer(file);
  }

  function playReplay(): void {
    worker?.postMessage({ type: 'playReplay' });
  }

  function pauseReplay(): void {
    worker?.postMessage({ type: 'pauseReplay' });
  }

  function seekReplay(event: Event): void {
    const input = event.currentTarget as HTMLInputElement;
    worker?.postMessage({ type: 'seekReplay', cursorMs: Number(input.value) });
  }

  function setReplaySpeed(event: Event): void {
    const input = event.currentTarget as HTMLInputElement;
    worker?.postMessage({ type: 'setReplaySpeed', speed: Number(input.value) });
  }

  function exportRecordingFile(log: SynapseLogExport): void {
    const buffer = new ArrayBuffer(log.bytes.byteLength);
    new Uint8Array(buffer).set(log.bytes);
    const blob = new Blob([buffer], { type: log.mimeType });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = log.filename;
    anchor.click();
    URL.revokeObjectURL(url);
  }

  function plotSeriesPath(samples: PlotSeries['samples'], width: number, height: number): string {
    if (samples.length < 2) {
      return '';
    }

    const values = samples.map((sample) => sample.value);
    const min = Math.min(...values);
    const max = Math.max(...values);
    const range = Math.max(0.001, max - min);
    const startTime = samples[0].timeMs;
    const timeSpan = Math.max(1, samples.at(-1)!.timeMs - startTime);

    return samples
      .map((sample, index) => {
        const x = ((sample.timeMs - startTime) / timeSpan) * width;
        const y = height - ((sample.value - min) / range) * height;
        return `${index === 0 ? 'M' : 'L'} ${x.toFixed(1)} ${y.toFixed(1)}`;
      })
      .join(' ');
  }

  function normalizePlotTraces(traces: PlotTraceSelection[], packets: PlotPacketDefinition[]): PlotTraceSelection[] {
    return traces.map((trace, index) => {
      const packet = packets.find((candidate) => candidate.key === trace.packetKey) ?? defaultPlotPacket(packets, index);
      const field = packet?.fields.find((candidate) => candidate.path === trace.fieldPath) ?? packet?.fields[0];
      return {
        packetKey: packet?.key ?? trace.packetKey,
        fieldPath: field?.path ?? trace.fieldPath
      };
    });
  }

  function samePlotTraces(left: PlotTraceSelection[], right: PlotTraceSelection[]): boolean {
    return left.length === right.length && left.every((trace, index) => trace.packetKey === right[index].packetKey && trace.fieldPath === right[index].fieldPath);
  }

  function defaultPlotPacket(packets: PlotPacketDefinition[], index: number): PlotPacketDefinition | undefined {
    const preferredSchema = index === 0 ? 'Pose' : 'Velocity';
    return (
      packets.find((packet) => packet.active && packet.schema === preferredSchema) ??
      packets.find((packet) => packet.schema === preferredSchema) ??
      packets.find((packet) => packet.active) ??
      packets[0]
    );
  }

  function fieldsForPacket(packetKey: string): PlotFieldDefinition[] {
    return plotPackets.find((packet) => packet.key === packetKey)?.fields ?? [];
  }

  function updatePlotTrace(index: number, patch: Partial<PlotTraceSelection>): void {
    const next = [...plotTraces];
    const current = next[index];
    const packetKey = patch.packetKey ?? current.packetKey;
    next[index] = {
      packetKey,
      fieldPath: patch.packetKey && !patch.fieldPath ? fieldsForPacket(packetKey)[0]?.path ?? '' : patch.fieldPath ?? current.fieldPath
    };
    plotTraces = next;
  }

  function resolvePlotTrace(
    trace: PlotTraceSelection,
    color: string,
    state: PlotState,
    packets: PlotPacketDefinition[]
  ): ResolvedPlotTrace {
    const packet = packets.find((candidate) => candidate.key === trace.packetKey);
    const field = packet?.fields.find((candidate) => candidate.path === trace.fieldPath);
    const series = state.series.find((candidate) => candidate.packetKey === trace.packetKey && candidate.fieldPath === trace.fieldPath);
    const samples = series?.samples ?? [];
    return {
      ...trace,
      color,
      packet,
      field,
      series,
      path: plotSeriesPath(samples, 360, 132),
      lastValue: series?.lastValue ?? null,
      rangeLabel: formatPlotRange(samples, field?.units ?? '')
    };
  }

  function packetOptionLabel(packet: PlotPacketDefinition): string {
    const prefix = packet.source === 'synapse_fbs' ? 'Synapse' : packet.source === 'electrode' ? 'GCS' : 'Live';
    return `${prefix} · ${packet.label}${packet.active ? ' · live' : ''}`;
  }

  function formatPlotValue(value: number | null, units = ''): string {
    if (value === null || Number.isNaN(value)) {
      return '--';
    }

    const abs = Math.abs(value);
    const digits = abs >= 100 ? 0 : abs >= 10 ? 1 : 2;
    return `${value.toFixed(digits)}${units ? ` ${units}` : ''}`;
  }

  function formatPlotRange(samples: PlotSeries['samples'], units: string): string {
    if (samples.length < 2) {
      return 'no samples';
    }

    const values = samples.map((sample) => sample.value);
    return `${formatPlotValue(Math.min(...values), units)} - ${formatPlotValue(Math.max(...values), units)}`;
  }

  function clamp(value: number, min: number, max: number): number {
    return Math.min(max, Math.max(min, value));
  }

  function format(value: number | undefined, digits = 1): string {
    return value === undefined || Number.isNaN(value) ? '--' : value.toFixed(digits);
  }

  function formatDuration(ms: number): string {
    const seconds = Math.floor(ms / 1000);
    const minutes = Math.floor(seconds / 60);
    return `${minutes}:${String(seconds % 60).padStart(2, '0')}`;
  }

</script>

<svelte:head>
  <title>electrode GCS</title>
  <meta name="description" content="electrode web ground station" />
</svelte:head>

<main class="shell">
  <header class="topbar">
    <div class="brand">
      <div class="brand-mark">
        <img
          src={asset(theme === 'light' ? '/electrode-light.png' : '/electrode-dark.png')}
          alt="electrode — ground station for CogniPilot"
        />
      </div>
      <div class="brand-meta">
        <h1>electrode</h1>
        <p>{vehicle.vehicleId}</p>
      </div>
    </div>

    <div class="header-actions">
      <details class="settings-menu">
        <summary class="icon-button quiet" aria-label="Settings" title="Settings">
          <Settings size={18} />
        </summary>
        <div class="settings-popover">
          <button type="button" class="settings-row" onclick={toggleThemeFromMenu}>
            {#if theme === 'dark'}
              <Sun size={18} />
              <span>Light mode</span>
            {:else}
              <Moon size={18} />
              <span>Dark mode</span>
            {/if}
          </button>
        </div>
      </details>
    </div>
  </header>

  {#if $isGroundStation}
    <nav class="ground-nav" aria-label="Ground Station sections">
      {#each groundStationPages as page}
        <button
          type="button"
          class:active={activePage === page.key}
          onclick={() => setGroundStationPage(page.key)}
        >
          {page.label}
        </button>
      {/each}
    </nav>
  {/if}

  {#if !$isGroundStation || activePage === 'dashboard'}
    {#if $isGroundStation}
      <GroundStationPanel {theme} />
      <details class="config-panel">
        <summary>Dashboard config</summary>
        <AutopilotConfigPanel {theme} />
        <RcMappingPanel {theme} />
        <PpmHardwarePanel {theme} channels={radioControl} />
      </details>
    {/if}

    <div class="dashboard">

    {#if $isGroundStation}
      <section class="panel autopilot-panel">
        <div class="panel-heading">
          <div>
            <h2>Autopilot</h2>
            <p>
              {autopilotError || autopilotRun?.message || 'checking...'}
              — local cubs2 (native_sim); hardware boots on power and shows in Autopilot I/O
            </p>
          </div>
          <Power size={20} />
        </div>

        <div class="metrics">
          <div class="metric">
            <span>State</span>
            <strong>{autopilotRunning ? `running · pid ${autopilotRun?.pid ?? '—'}` : 'stopped'}</strong>
          </div>
          <div class="metric">
            <span>Frames out</span>
            <strong>{autopilotRun?.framesOut ?? 0}</strong>
          </div>
          <div class="metric">
            <span>Frames in</span>
            <strong>{autopilotRun?.framesIn ?? 0}</strong>
          </div>
        </div>

        <div class="button-row">
          <button
            type="button"
            class="icon-button primary"
            onclick={() => void toggleAutopilot()}
            disabled={autopilotBusy}
          >
            {#if autopilotRunning}
              <Square size={18} />
              <span>Stop</span>
            {:else}
              <CirclePlay size={18} />
              <span>Start</span>
            {/if}
          </button>
        </div>
      </section>
    {/if}

    <section class="panel io-panel">
      <div class="panel-heading">
        <div>
          <h2>State I/O</h2>
          <p>mocap, autopilot telemetry, actuator commands, and manual control</p>
        </div>
        <Activity size={20} />
      </div>

      <div class="mode-readout">
        <span>Autopilot reported</span>
        <strong class:on={vehicle.mode.name === 'auto'}>{vehicle.mode.name}</strong>
        <em>{vehicle.mode.armed ? 'armed' : 'disarmed'}{vehicle.mode.failsafe ? ' · FAILSAFE' : ''}</em>
      </div>

      <div class="io-group">
        <h3>Mocap 6DOF pose</h3>
        <div class="io-row">
          <span>Position <em>{ioRate(ioMocap)}</em></span>
          <strong>
            {pose
              ? `x ${format(pose.xM)} · y ${format(pose.yM)} · z ${format(pose.altM)} m`
              : '--'}
          </strong>
        </div>
        <div class="io-row">
          <span>Orientation <em>{vehicle.localization.source}</em></span>
          <strong>r {format(attitude?.rollDeg)}° · p {format(attitude?.pitchDeg)}° · y {format(attitude?.yawDeg)}°</strong>
        </div>
        <div class="io-row">
          <span>Tracking <em>{vehicle.localization.fresh ? 'fresh' : 'stale'}</em></span>
          <strong>{vehicle.localization.source} · quality {format(vehicle.localization.quality * 100, 0)}%</strong>
        </div>
      </div>

      <div class="io-group">
        <h3>Autopilot telemetry received</h3>
        <div class="io-row">
          <span>Flight mode <em>{ioRate(ioHealth)}</em></span>
          <strong class:on={vehicle.mode.name === 'auto'}>
            {vehicle.mode.name} · {vehicle.mode.armed ? 'armed' : 'disarmed'}{vehicle.mode.failsafe ? ' · FAILSAFE' : ''}
          </strong>
        </div>
        <div class="io-row">
          <span>Attitude <em>{ioRate(ioAttitude)}</em></span>
          <strong>r {format(attitude?.rollDeg)}° · p {format(attitude?.pitchDeg)}° · y {format(attitude?.yawDeg)}°</strong>
        </div>
        <div class="io-row">
          <span>Link <em>{ioRate(ioHealth)}</em></span>
          <strong>{format(link?.latencyMs, 0)} ms · {format(link?.rssiDbm, 0)} dBm · loss {format(link?.packetLossPct, 0)}%</strong>
        </div>
      </div>

      <div class="io-group">
        <h3>Selected raw commands to sim/hardware</h3>
        <div class="io-row">
          <span>{displayedPwmLabel} <em>{ioRate(ioPwm)}</em></span>
          <strong>
            {displayedPwm && displayedPwm.length >= 4
              ? `${displayedPwm[0]} · ${displayedPwm[1]} · ${displayedPwm[2]} · ${displayedPwm[3]} µs`
              : '--'}
          </strong>
        </div>
      </div>

      <div class="io-group">
        <h3>Manual control state</h3>
        <div class="io-row">
          <span>Mode switch request <em>{ioRate(ioManual)}</em></span>
          <strong class:on={requestedControlSource === 'autopilot'} class:warn={controlModeMismatch}>
            {requestedControlSource} · {requestedControlSourceDetail}
            {controlModeMismatch ? ' · not accepted by autopilot' : ''}
          </strong>
        </div>
        <div class="io-row">
          <span>Manual control <em>{ioRate(ioManual)}</em></span>
          <strong>
            {#if manualControl}
              r {format(manualControl.roll, 2)} · p {format(manualControl.pitch, 2)} · y {format(manualControl.yaw, 2)} · t {format(manualControl.throttle, 2)}
              {manualControl.armSwitch ? ' · ARM' : ''}{manualControl.killSwitch ? ' · KILL' : ''}
            {:else}
              --
            {/if}
          </strong>
        </div>
      </div>
    </section>

    <section class="panel vehicle-panel">
      <div class="panel-heading">
        <div>
          <h2>Vehicle</h2>
          <p>{vehicle.mode.name} · {vehicle.mode.armed ? 'armed' : 'disarmed'}</p>
        </div>
        <Gauge size={20} />
      </div>

      <div class="metrics">
        <div class="metric">
          <span>Altitude</span>
          <strong>{format(pose?.altM)} m</strong>
        </div>
        <div class="metric">
          <span>Speed</span>
          <strong>{format(vehicle.velocity?.groundSpeedMps)} m/s</strong>
        </div>
        <div class="metric">
          <span>Yaw</span>
          <strong>{format(attitude?.yawDeg, 0)} deg</strong>
        </div>
        <div class="metric">
          <span>Quality</span>
          <strong>{format(vehicle.localization.quality * 100, 0)}%</strong>
        </div>
        <div class="metric">
          <span>Waypoint</span>
          <strong>{mission && mission.total > 0 ? `${mission.currentSeq + 1}/${mission.total}` : '—'}</strong>
        </div>
        <div class="metric">
          <span>Mission</span>
          <strong>{mission?.state ?? '—'}</strong>
        </div>
      </div>
    </section>

    <section class="panel map-panel">
      <div class="panel-heading map-heading">
        <div>
          <h2>Map</h2>
          <p>{mapSubtitle}</p>
        </div>
        <div class="map-tools">
          <div class="map-view-control" aria-label="Map view mode">
            {#each mapViewModes as option}
              <button
                type="button"
                class:active={mapViewMode === option}
                onclick={() => {
                  mapViewMode = option;
                }}
              >
                {option.toUpperCase()}
              </button>
            {/each}
          </div>
          <MapPinned size={20} />
        </div>
      </div>

      {#if mapViewMode === '2d'}
        <div class="map-canvas outdoor-map">
          <div class="map-home">HOME</div>
          {#if missionWaypoints.length > 1}
            <svg class="map-mission-path" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
              <polyline points={missionPathPoints} />
            </svg>
          {/if}
          {#each missionWaypoints as waypoint (waypoint.seq)}
            <div
              class="map-waypoint"
              class:active={mission !== null && waypoint.seq === mission.currentSeq}
              style={`left: ${waypoint.mapX}%; top: ${waypoint.mapY}%;`}
              title={`Waypoint ${waypoint.seq + 1} · E ${waypoint.east.toFixed(1)} m, N ${waypoint.north.toFixed(1)} m, alt ${waypoint.up.toFixed(1)} m`}
            >
              {waypoint.seq + 1}
            </div>
          {/each}
          <div
            class="vehicle-marker"
            style={`left: ${mapX}%; top: ${mapY}%; transform: translate(-50%, -50%) rotate(${90 - yawDeg}deg);`}
            title="Vehicle position"
          >
            <svg class="map-vehicle-marker" viewBox="-24 -24 48 48" aria-hidden="true">
              <!-- inner group leans with roll to hint the bank angle -->
              <g transform={`skewX(${clamp(-rollDeg * 0.35, -20, 20)})`}>
                {#if selectedVehicleType === 'fixedwing'}
                  <path
                    class="map-vehicle-body"
                    d="M0 -19 L2.4 -7 L19 3 L19 6 L2.4 -1.5 L2.4 10 L9 15 L9 17 L0 14 L-9 17 L-9 15 L-2.4 10 L-2.4 -1.5 L-19 6 L-19 3 L-2.4 -7 Z"
                  />
                  <circle class="map-vehicle-nose" cx="0" cy="-16" r="2.1" />
                {:else}
                  <line class="map-vehicle-arm" x1="-12" y1="-12" x2="12" y2="12" />
                  <line class="map-vehicle-arm" x1="12" y1="-12" x2="-12" y2="12" />
                  <circle class="map-vehicle-rotor front" cx="-12" cy="-12" r="5" />
                  <circle class="map-vehicle-rotor front" cx="12" cy="-12" r="5" />
                  <circle class="map-vehicle-rotor" cx="-12" cy="12" r="5" />
                  <circle class="map-vehicle-rotor" cx="12" cy="12" r="5" />
                  <circle class="map-vehicle-hub" cx="0" cy="0" r="3.4" />
                {/if}
              </g>
            </svg>
          </div>
          <div class="map-scale">50 m</div>
        </div>
      {:else}
        <IndoorScene
          {pose}
          {attitude}
          {controls}
          {motors}
          {mission}
          localizationQuality={vehicle.localization.quality}
          {theme}
          bind:vehicleType={selectedVehicleType}
        />
      {/if}
    </section>

    <section class="panel deflect-panel">
      <div class="panel-heading">
        <div>
          <h2>Control Surfaces</h2>
          <p>deflection · top &amp; rear</p>
        </div>
        <Gauge size={20} />
      </div>
      <DeflectionView {attitude} controls={deflectionControls} {motors} {theme} bind:vehicleType={selectedVehicleType} />
    </section>

    <section class="panel manual-panel">
      <div class="panel-heading">
        <div>
          <h2>Manual Link</h2>
          <p>{manualBridgeStatus} · {keyboardRemoteStatus}</p>
        </div>
        <Radio size={20} />
      </div>

      {#if $isGroundStation}
        <div class="button-row manual-actions">
          <button
            type="button"
            class="icon-button"
            class:primary={!manualBridgeRunning}
            class:danger={manualBridgeRunning}
            onclick={() => void toggleManualBridge()}
            disabled={manualBridgeBusy}
            title="Start/stop publishing this gamepad as manual_control over Zenoh"
          >
            {#if manualBridgeRunning}
              <Square size={18} />
              <span>Stop</span>
            {:else}
              <CirclePlay size={18} />
              <span>Start</span>
            {/if}
          </button>
        </div>
      {/if}

      <ManualLinkView manual={manualControl} {theme} hardwareAvailable={joystickPresent} />

      {#if keyboardRemoteActive}
        <div class="keyboard-map" aria-label="Keyboard fallback controls">
          <div><span>Roll</span><strong>Left / Right</strong></div>
          <div><span>Pitch</span><strong>Down / Up</strong></div>
          <div><span>Yaw</span><strong>A / D</strong></div>
          <div><span>Throttle</span><strong>S / W</strong></div>
          <div><span>Mode</span><strong>M · {keyboardAuto ? 'auto' : 'manual'}</strong></div>
          <div><span>Stabilize</span><strong>T · {keyboardStabilize ? 'on' : 'off'}</strong></div>
        </div>
      {/if}

      {#if $isGroundStation}
        <div class="manual-service">
          {#each manualLinks as [label, value]}
            <div>
              <span>{label}</span>
              <strong>{value}</strong>
            </div>
          {/each}
        </div>

      {/if}
    </section>

    <section class="panel attitude-panel">
      <div class="panel-heading">
        <div>
          <h2>HUD</h2>
          <p>{vehicle.mode.name} · {vehicle.mode.armed ? 'armed' : 'disarmed'} · roll {format(attitude?.rollDeg)} · pitch {format(attitude?.pitchDeg)}</p>
        </div>
        <Activity size={20} />
      </div>

      <div class="attitude-display">
        <svg class="hud-svg" viewBox="0 0 560 500" role="img" aria-label="Head-up display">
          <defs>
            <clipPath id="hud-field"><rect x="150" y="94" width="260" height="304" /></clipPath>
            <clipPath id="hud-heading-clip"><rect x="150" y="40" width="260" height="36" /></clipPath>
          </defs>

          <rect class="hud-frame" x="10" y="10" width="540" height="480" rx="16" />

          <!-- heading tape -->
          <g clip-path="url(#hud-heading-clip)">
            <line class="hud-line thin" x1="150" y1="70" x2="410" y2="70" />
            {#each headingTicks as t}
              <line class="hud-line" x1={t.x} y1="70" x2={t.x} y2={t.major ? 60 : 65} />
              {#if t.label}
                <text class="hud-text sm" x={t.x} y="54" text-anchor="middle">{t.label}</text>
              {/if}
            {/each}
          </g>
          <!-- heading index + boxed readout -->
          <path class="hud-fill" d="M280 74 l-6 -9 h12 Z" />
          <rect class="hud-box" x="252" y="16" width="56" height="26" rx="2" />
          <text class="hud-text lg" x="280" y="37" text-anchor="middle">{hudHeadingText}</text>

          <!-- attitude world (rotates with roll, translates with pitch) -->
          <g clip-path="url(#hud-field)">
            <g transform={`rotate(${-rollDeg} 280 250)`}>
              <!-- roll scale (moving) -->
              {#each rollTicks as tick}
                <line
                  class="hud-line"
                  transform={`rotate(${tick} 280 250)`}
                  x1="280"
                  y1="122"
                  x2="280"
                  y2={tick === 0 || Math.abs(tick) === 30 || Math.abs(tick) === 60 ? 111 : 116}
                />
              {/each}
              <!-- horizon line with center gap -->
              <line class="hud-line horizon" x1="150" y1={horizonY} x2="252" y2={horizonY} />
              <line class="hud-line horizon" x1="308" y1={horizonY} x2="410" y2={horizonY} />
              <!-- pitch ladder -->
              {#each pitchRungs as m}
                {@const yy = 250 - (m - pitchDeg) * 4.2}
                {@const tick = m > 0 ? 8 : -8}
                <g class:hud-dash={m < 0}>
                  <line class="hud-line" x1="196" y1={yy} x2="250" y2={yy} />
                  <line class="hud-line" x1="310" y1={yy} x2="364" y2={yy} />
                  <line class="hud-line" x1="250" y1={yy} x2="250" y2={yy + tick} />
                  <line class="hud-line" x1="310" y1={yy} x2="310" y2={yy + tick} />
                </g>
                <text class="hud-text num" x="190" y={yy + 4} text-anchor="end">{Math.abs(m)}</text>
                <text class="hud-text num" x="370" y={yy + 4} text-anchor="start">{Math.abs(m)}</text>
              {/each}
            </g>
          </g>

          <!-- fixed roll pointer -->
          <path class="hud-fill" d="M280 124 l-7 -12 h14 Z" />

          <!-- fixed flight-path / boresight symbol -->
          <g class="hud-line">
            <circle cx="280" cy="250" r="9" fill="none" />
            <line x1="271" y1="250" x2="252" y2="250" />
            <line x1="289" y1="250" x2="308" y2="250" />
            <line x1="280" y1="241" x2="280" y2="230" />
          </g>

          <!-- airspeed box -->
          <path class="hud-box" d="M44 236 H100 L112 250 L100 264 H44 Z" />
          <text class="hud-text lg" x="70" y="256" text-anchor="middle">{Math.round(hudSpeedMps)}</text>
          <text class="hud-text sm dim" x="44" y="282" text-anchor="start">M/S</text>

          <!-- altitude box -->
          <path class="hud-box" d="M516 236 H460 L448 250 L460 264 H516 Z" />
          <text class="hud-text lg" x="490" y="256" text-anchor="middle">{Math.round(hudAltMeters)}</text>
          <text class="hud-text sm dim" x="516" y="282" text-anchor="end">M</text>
          <!-- vertical speed -->
          <text class="hud-text sm" x="516" y="228" text-anchor="end">{hudClimbMps >= 0 ? '+' : ''}{hudClimbMps.toFixed(1)}</text>

          <!-- status line -->
          <text class="hud-text sm dim" x="34" y="472" text-anchor="start">CEREBRI · FLIGHT CONTROL</text>
          <text class="hud-text sm" x="526" y="472" text-anchor="end">{vehicle.mode.armed ? 'ARM' : 'STBY'} · {vehicle.mode.name} · Q {hudQualityPct.toFixed(0)}</text>
        </svg>
      </div>
    </section>

    <section class="panel replay-panel">
      <div class="panel-heading">
        <div>
          <h2>Replay</h2>
          <p>{replay.frameCount} frames · {formatDuration(replay.cursorMs)} / {formatDuration(replay.durationMs)}</p>
        </div>
        <RotateCcw size={20} />
      </div>

      <div class="button-row">
        <button type="button" class="icon-button quiet" onclick={replay.playing ? pauseReplay : playReplay}>
          {#if replay.playing}
            <CirclePause size={18} />
            <span>Pause</span>
          {:else}
            <CirclePlay size={18} />
            <span>Play</span>
          {/if}
        </button>
        <label class="file-button">
          <Upload size={18} />
          <span>Load</span>
          <input
            type="file"
            accept=".mcap,application/mcap,application/octet-stream"
            onchange={loadReplay}
          />
        </label>
      </div>

      <input class="range" type="range" min="0" max={Math.max(1, replay.durationMs)} value={replay.cursorMs} oninput={seekReplay} />
      <label class="field inline">
        <span>Speed</span>
        <input type="range" min="0.25" max="4" step="0.25" value={replay.speed} oninput={setReplaySpeed} />
        <strong>{replay.speed.toFixed(2)}x</strong>
      </label>
    </section>

    <section class="panel recording-panel">
      <div class="panel-heading">
        <div>
          <h2>Logging</h2>
          <p>{recordingCount} frames · all topics · Stop saves .mcap</p>
        </div>
        <Download size={20} />
      </div>

      <div class="button-row">
        {#if recording}
          <button type="button" class="icon-button danger" onclick={stopRecording}>
            <Square size={18} />
            <span>Stop</span>
          </button>
        {:else}
          <button type="button" class="icon-button primary" onclick={startRecording}>
            <CirclePlay size={18} />
            <span>Record</span>
          </button>
        {/if}
        <button type="button" class="icon-button quiet" onclick={exportRecording}>
          <Download size={18} />
          <span>Export</span>
        </button>
      </div>
    </section>

    <section class="panel plot-panel">
      <div class="panel-heading">
        <div>
          <h2>Plots</h2>
          <p>{plotState.series.length} fields · {plotPackets.filter((packet) => packet.active).length} live packets</p>
        </div>
        <Activity size={20} />
      </div>

      <div class="plot-controls">
        {#each plotTraces as trace, index}
          <div class="plot-trace-control">
            <span class="plot-swatch" style={`background: ${plotTraceColors[index]};`}></span>
            <label>
              <span>Packet</span>
              <select
                value={trace.packetKey}
                onchange={(event) => updatePlotTrace(index, { packetKey: (event.currentTarget as HTMLSelectElement).value })}
              >
                {#each plotPackets as packet}
                  <option value={packet.key}>{packetOptionLabel(packet)}</option>
                {/each}
              </select>
            </label>
            <label>
              <span>Field</span>
              <select
                value={trace.fieldPath}
                onchange={(event) => updatePlotTrace(index, { fieldPath: (event.currentTarget as HTMLSelectElement).value })}
              >
                {#each fieldsForPacket(trace.packetKey) as field}
                  <option value={field.path}>{field.label}{field.units ? ` · ${field.units}` : ''}</option>
                {/each}
              </select>
            </label>
          </div>
        {/each}
      </div>

      <div class="plot-frame">
        <svg class="plot" viewBox="0 0 360 132" preserveAspectRatio="none" aria-label="Selected packet field plot">
          {#each [0, 1, 2, 3] as gridLine}
            <line class="plot-grid-line" x1="0" x2="360" y1={gridLine * 44} y2={gridLine * 44} />
          {/each}
          {#each resolvedPlotTraces as trace}
            {#if trace.path}
              <path class="plot-line" d={trace.path} style={`stroke: ${trace.color};`} />
            {/if}
          {/each}
        </svg>
        {#if !plotHasSamples}
          <div class="plot-empty">Waiting for samples</div>
        {/if}
      </div>

      <div class="plot-legend">
        {#each resolvedPlotTraces as trace}
          <div>
            <span class="plot-swatch" style={`background: ${trace.color};`}></span>
            <strong>{trace.packet?.label ?? 'Packet'} · {trace.field?.label ?? trace.fieldPath}</strong>
            <span>{formatPlotValue(trace.lastValue, trace.field?.units)} · {trace.rangeLabel}</span>
          </div>
        {/each}
      </div>
    </section>

    <section class="panel events-panel">
      <div class="panel-heading">
        <div>
          <h2>Events</h2>
          <p>{warningCount} warnings</p>
        </div>
        <AlertTriangle size={20} />
      </div>

      <div class="event-list">
        {#each vehicle.events.slice(0, 8) as event}
          <div class="event-row" class:warn={event.severity === 'warning'} class:danger={event.severity === 'error'}>
            <span>{event.severity}</span>
            <strong>{event.message}</strong>
          </div>
        {:else}
          <div class="empty-row">No events</div>
        {/each}
      </div>
    </section>

    <section class="panel topics-panel">
      <div class="panel-heading">
        <div>
          <h2>Topics</h2>
          <p>{topicRows.length} active · {Object.keys(TOPIC_DEFINITIONS).length} registered</p>
        </div>
        <Activity size={20} />
      </div>

      <div class="topic-table">
        <div class="topic-row header">
          <span>Topic</span>
          <span>Rate</span>
          <span>Latency</span>
          <span>State</span>
        </div>
        {#each topicRows as topic}
          <div class="topic-row">
            <span title={topic.topic}>{topic.label}</span>
            <span>{topic.rateHz.toFixed(1)} Hz</span>
            <span>{topic.latencyMs.toFixed(0)} ms</span>
            <span class:warn={topic.stale} class:ok={!topic.stale}>{topic.stale ? 'stale' : 'fresh'}</span>
          </div>
        {:else}
          <div class="empty-row">No topics</div>
        {/each}
      </div>
    </section>

    <section class="panel discovery-panel">
      <div class="panel-heading">
        <div>
          <h2>Discovery</h2>
          <p>
            {#if topicCatalog}
              {topicCatalog.topics.length} discovered · {topicCatalog.connected ? 'zenoh live' : 'zenoh offline'}
              {#if topicCatalog.endpoint}· {topicCatalog.endpoint}{/if}
            {:else}
              connect over Zenoh to discover
            {/if}
          </p>
        </div>
        <Radio size={20} />
      </div>

      <div class="topic-table discovery-table">
        <div class="topic-row discovery-row header">
          <span>Recv</span>
          <span>Key</span>
          <span>Schema</span>
          <span>Rate</span>
          <span>Bytes</span>
        </div>
        {#each (topicCatalog?.topics ?? []) as topic (topic.key)}
          <div class="topic-row discovery-row">
            <span>
              <input
                type="checkbox"
                checked={topic.selected}
                onchange={() => toggleTopicSubscription(topic.key)}
                title={topic.selected ? 'Streaming — click to stop' : 'Click to stream this topic'}
              />
            </span>
            <span title={topic.key}>{topic.key}</span>
            <span class:ok={topic.decodable} title={topic.decodable ? 'Decoded to fields' : 'Raw bytes only'}>
              {topic.schema}
            </span>
            <span>{topic.rateHz.toFixed(1)} Hz</span>
            <span>{topic.lastBytes} B</span>
          </div>
        {:else}
          <div class="empty-row">
            {topicCatalog ? 'No topics on the network yet' : 'Connect over Zenoh to discover topics'}
          </div>
        {/each}
      </div>
    </section>
  </div>
  {:else}
    <SimulationPanel {theme} {zenohEndpoint} />
  {/if}
</main>

<style>
  :global(html) {
    background: #f6f7f8;
    color: #171b1f;
    font-family:
      Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    letter-spacing: 0;
  }

  :global(body) {
    margin: 0;
    min-width: 320px;
  }

  :global(*) {
    box-sizing: border-box;
  }

  .shell {
    min-height: 100vh;
    padding: 18px;
  }

  .topbar,
  .ground-nav,
  .panel {
    border: 1px solid #d9dee3;
    background: #ffffff;
    border-radius: 8px;
  }

  .topbar {
    display: grid;
    grid-template-columns: 1fr auto auto;
    gap: 16px;
    align-items: center;
    padding: 14px;
  }

  .ground-nav {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0;
    overflow: hidden;
    margin-top: 10px;
    border: 1px solid #d9dee3;
    border-radius: 8px;
    background: #ffffff;
  }

  .ground-nav button {
    min-height: 42px;
    border: 0;
    border-left: 1px solid #d9dee3;
    background: transparent;
    color: #5c6873;
    font: inherit;
    font-size: 0.78rem;
    font-weight: 820;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    cursor: pointer;
  }

  .ground-nav button:first-child {
    border-left: 0;
  }

  .ground-nav button.active {
    background: #12171b;
    color: #ffffff;
  }

  .config-panel {
    margin-top: 10px;
  }

  .config-panel summary {
    display: flex;
    align-items: center;
    min-height: 42px;
    padding: 0 12px;
    border: 1px solid #d9dee3;
    border-radius: 8px;
    background: #ffffff;
    color: #5c6873;
    cursor: pointer;
    font-size: 0.78rem;
    font-weight: 820;
    text-transform: uppercase;
  }

  .config-panel > :global(*:not(summary)) {
    margin-top: 10px;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
  }

  .brand-mark {
    display: flex;
    height: 46px;
    align-items: center;
    justify-content: center;
    overflow: hidden;
  }

  .brand-mark img {
    display: block;
    height: 100%;
    width: auto;
    object-fit: contain;
  }

  .brand-meta h1 {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
    border: 0;
  }

  h1,
  h2,
  p {
    margin: 0;
  }

  h1 {
    font-size: 1.35rem;
    line-height: 1.1;
  }

  h2 {
    font-size: 1rem;
    line-height: 1.2;
  }

  p {
    color: #66717c;
    font-size: 0.86rem;
  }

  .header-actions,
  .button-row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .header-actions {
    justify-content: flex-end;
  }

  button,
  input,
  select,
  .file-button {
    font: inherit;
  }

  button,
  .file-button {
    border: 0;
    min-height: 38px;
    border-radius: 8px;
    cursor: pointer;
  }

  .icon-button,
  .file-button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 0 12px;
    white-space: nowrap;
  }

  .icon-button.primary {
    background: #15836f;
    color: #ffffff;
  }

  .icon-button.quiet,
  .file-button {
    background: #eef2f4;
    color: #263039;
  }

  .icon-button.danger {
    background: #b83e4b;
    color: #ffffff;
  }

  .settings-menu {
    position: relative;
  }

  .settings-menu summary {
    list-style: none;
  }

  .settings-menu summary::-webkit-details-marker {
    display: none;
  }

  .settings-popover {
    position: absolute;
    top: calc(100% + 8px);
    right: 0;
    z-index: 20;
    min-width: 172px;
    padding: 6px;
    border: 1px solid #d9dee3;
    border-radius: 8px;
    background: #ffffff;
    box-shadow: 0 18px 38px rgba(17, 23, 27, 0.18);
  }

  .settings-row {
    display: flex;
    width: 100%;
    align-items: center;
    justify-content: flex-start;
    gap: 8px;
    min-height: 36px;
    padding: 0 10px;
    border-radius: 6px;
    background: transparent;
    color: #263039;
    font-size: 0.78rem;
    font-weight: 760;
    text-align: left;
  }

  .settings-row:hover {
    background: #eef2f4;
  }

  .ok {
    color: #147660;
  }

  .warn {
    color: #a36500;
  }

  .danger {
    color: #ab2f3f;
  }

  .dashboard {
    display: grid;
    grid-template-columns: minmax(260px, 0.85fr) minmax(340px, 1.4fr) minmax(300px, 1fr);
    gap: 12px;
    margin-top: 12px;
    align-items: start;
  }

  .panel {
    min-width: 0;
    padding: 14px;
  }

  .map-panel,
  .topics-panel,
  .events-panel {
    grid-row: span 2;
  }

  .panel-heading {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 12px;
  }

  .map-heading {
    gap: 10px;
  }

  .map-tools {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .map-view-control {
    display: grid;
    grid-template-columns: repeat(2, minmax(58px, 1fr));
    overflow: hidden;
    min-height: 32px;
    border: 1px solid #cdd5dc;
    border-radius: 3px;
    background: #f5f7f8;
  }

  .map-view-control button {
    min-height: 32px;
    padding: 0 9px;
    border-radius: 0;
    background: transparent;
    color: #53606a;
    font-size: 0.72rem;
    font-weight: 760;
    text-transform: uppercase;
  }

  .map-view-control button + button {
    border-left: 1px solid #cdd5dc;
  }

  .map-view-control button.active {
    background: #152129;
    color: #ffffff;
  }

  .field {
    display: grid;
    gap: 6px;
    color: #596570;
    font-size: 0.84rem;
  }

  .field.inline {
    grid-template-columns: auto 1fr auto;
    align-items: center;
    margin-top: 10px;
  }

  input,
  select {
    width: 100%;
    min-height: 38px;
    border: 1px solid #cfd6dd;
    border-radius: 8px;
    padding: 0 10px;
    background: #ffffff;
    color: #1d252c;
  }

  .metrics {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px;
  }


  .metric {
    display: grid;
    gap: 4px;
    min-height: 70px;
    padding: 10px;
    border: 1px solid #e0e5ea;
    border-radius: 8px;
    background: #fafbfc;
  }

  .metric span {
    color: #6b7680;
    font-size: 0.78rem;
  }

  .metric strong {
    align-self: end;
    overflow-wrap: anywhere;
    font-size: 1.18rem;
  }

  .mode-readout {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr);
    gap: 10px;
    align-items: center;
    margin-bottom: 12px;
    padding: 10px;
    border: 1px solid #e0e5ea;
    border-radius: 8px;
    background: #fafbfc;
  }

  .mode-readout span,
  .mode-readout em {
    color: #6b7680;
    font-size: 0.78rem;
    font-style: normal;
    font-weight: 700;
    text-transform: uppercase;
  }

  .mode-readout strong {
    color: #171b1f;
    font-size: 1.25rem;
    font-weight: 820;
    text-transform: uppercase;
  }

  .mode-readout strong.on {
    color: #e35f0c;
  }

  .io-group {
    display: grid;
    gap: 6px;
  }

  .io-group + .io-group {
    margin-top: 12px;
  }

  .io-group h3 {
    margin: 0;
    color: #6b7680;
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .io-row {
    display: grid;
    gap: 2px;
    padding: 8px 10px;
    border: 1px solid #e0e5ea;
    border-radius: 8px;
    background: #fafbfc;
  }

  .io-row span {
    display: flex;
    justify-content: space-between;
    color: #6b7680;
    font-size: 0.78rem;
  }

  .io-row span em {
    font-style: normal;
    font-variant-numeric: tabular-nums;
  }

  .io-row strong {
    overflow-wrap: anywhere;
    font-size: 0.98rem;
    font-variant-numeric: tabular-nums;
  }

  .io-row strong.on {
    color: #e35f0c;
  }

  .map-canvas {
    position: relative;
    overflow: hidden;
    height: 350px;
    border-radius: 8px;
    border: 1px solid #cdd5dc;
    background:
      linear-gradient(90deg, rgba(31, 72, 61, 0.12) 1px, transparent 1px),
      linear-gradient(rgba(31, 72, 61, 0.12) 1px, transparent 1px),
      radial-gradient(circle at 24% 22%, rgba(68, 158, 129, 0.18), transparent 24%),
      linear-gradient(135deg, #edf4ef, #eef2f6 62%, #f8f9f6);
    background-size: 28px 28px, 28px 28px, auto, auto;
  }

  .vehicle-marker {
    position: absolute;
    display: grid;
    width: 42px;
    height: 42px;
    place-items: center;
    background: transparent;
    color: #54d0b5;
  }

  .map-vehicle-marker {
    width: 100%;
    height: 100%;
    overflow: visible;
  }

  .map-home,
  .map-scale {
    position: absolute;
    border-radius: 8px;
    background: rgba(255, 255, 255, 0.86);
    padding: 6px 8px;
    color: #47535f;
    font-size: 0.75rem;
    font-weight: 700;
  }

  .map-home {
    left: 16px;
    bottom: 16px;
  }

  .map-scale {
    right: 16px;
    bottom: 16px;
  }

  .attitude-display {
    position: relative;
    overflow: hidden;
    display: grid;
    height: 260px;
    place-items: center;
    border-radius: 8px;
    border: 1px solid #d3dae0;
    background: #101518;
  }

  .range {
    margin-top: 12px;
  }

  .file-button {
    position: relative;
  }

  .file-button input {
    position: absolute;
    inset: 0;
    opacity: 0;
    cursor: pointer;
  }

  .plot {
    width: 100%;
    height: 128px;
    border: 1px solid #d8dee4;
    border-radius: 8px;
    background: linear-gradient(#ffffff, #f7fafb);
  }

  .plot-line {
    fill: none;
    stroke-linecap: round;
    stroke-width: 2.4;
  }

  .event-list,
  .topic-table {
    display: grid;
    gap: 6px;
  }

  .event-row,
  .topic-row,
  .empty-row {
    display: grid;
    align-items: center;
    min-height: 36px;
    border-radius: 8px;
    background: #f6f8fa;
    padding: 8px 10px;
    font-size: 0.86rem;
  }

  .event-row {
    grid-template-columns: 72px 1fr;
    gap: 8px;
  }

  .event-row span {
    text-transform: capitalize;
  }

  .topic-row {
    grid-template-columns: minmax(96px, 1fr) 76px 76px 60px;
    gap: 8px;
  }

  .topic-row.discovery-row {
    grid-template-columns: 36px minmax(120px, 1.4fr) 92px 66px 60px;
  }

  .discovery-row input[type='checkbox'] {
    cursor: pointer;
    margin: 0;
  }

  .topic-row.header {
    background: #ffffff;
    color: #697580;
    font-size: 0.75rem;
    font-weight: 700;
  }

  .topic-row span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .empty-row {
    color: #697580;
  }

  @media (max-width: 1180px) {
    .dashboard {
      grid-template-columns: minmax(260px, 1fr) minmax(320px, 1fr);
    }

    .map-panel,
    .topics-panel,
    .events-panel {
      grid-row: auto;
    }
  }

  @media (max-width: 780px) {
    .shell {
      padding: 10px;
    }

    .topbar {
      grid-template-columns: 1fr;
    }

    .dashboard {
      grid-template-columns: 1fr;
    }

    .header-actions {
      justify-content: flex-end;
    }

    .button-row .icon-button,
    .file-button {
      flex: 1;
    }

    .map-canvas {
      height: 280px;
    }

    .topic-row {
      grid-template-columns: minmax(92px, 1fr) 68px 68px 56px;
      font-size: 0.8rem;
    }
  }

  /* Operations console refresh */
  :global(html) {
    background:
      linear-gradient(180deg, #0a0d0e 0%, #101314 52%, #080b0c 100%);
    color: #ecf4ef;
    font-family:
      Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    letter-spacing: 0;
  }

  :global(body) {
    background: #0a0d0e;
    overflow-x: hidden;
  }

  .shell {
    min-height: 100vh;
    padding: 12px;
    overflow-x: hidden;
    background:
      linear-gradient(90deg, rgba(255, 255, 255, 0.024) 1px, transparent 1px),
      linear-gradient(rgba(255, 255, 255, 0.018) 1px, transparent 1px),
      linear-gradient(180deg, #0b0f10, #111516 48%, #090c0d);
    background-size: 36px 36px, 36px 36px, auto;
  }

  .topbar,
  .panel {
    border: 1px solid #263239;
    border-radius: 8px;
    background: rgba(17, 23, 25, 0.92);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.055),
      0 16px 42px rgba(0, 0, 0, 0.24);
  }

  .topbar {
    grid-template-columns: minmax(240px, 1fr) minmax(360px, auto) auto;
    min-height: 72px;
    padding: 10px 12px;
    background: rgba(14, 20, 22, 0.96);
  }

  .ground-nav {
    background: rgba(14, 20, 22, 0.96);
  }

  .ground-nav button {
    border-left-color: #263239;
    color: #8fa09a;
  }

  .ground-nav button.active {
    background: #dfe9e4;
    color: #0a1111;
  }

  .config-panel summary {
    border-color: #263239;
    border-radius: 8px;
    background: rgba(14, 20, 22, 0.96);
    color: #8fa09a;
  }

  .brand {
    gap: 12px;
  }

  .brand-mark {
    height: 46px;
    border: 0;
    background: transparent;
    box-shadow: none;
  }

  h1 {
    color: #f4fbf7;
    font-size: 1.15rem;
    font-weight: 760;
  }

  h2 {
    color: #f4fbf7;
    font-size: 0.86rem;
    font-weight: 760;
    letter-spacing: 0.02em;
    text-transform: uppercase;
  }

  p {
    color: #84938d;
    font-size: 0.78rem;
  }

  .icon-button,
  .file-button {
    min-height: 42px;
    border: 1px solid transparent;
    border-radius: 3px;
    font-size: 0.88rem;
    font-weight: 720;
    transition:
      background 140ms ease,
      border-color 140ms ease,
      color 140ms ease,
      transform 140ms ease;
  }

  .icon-button:hover,
  .file-button:hover {
    transform: translateY(-1px);
  }

  .icon-button.primary {
    border-color: rgba(253, 119, 25, 0.5);
    background: #fd7719;
    color: #1a0d02;
  }

  .icon-button.quiet,
  .file-button {
    border-color: #2b383f;
    background: #172023;
    color: #d9e4df;
  }

  .settings-popover {
    border-color: #2b383f;
    background: #111719;
    box-shadow: 0 18px 38px rgba(0, 0, 0, 0.28);
  }

  .settings-row {
    color: #d9e4df;
  }

  .settings-row:hover {
    background: #172023;
  }

  .icon-button.danger {
    border-color: rgba(255, 93, 115, 0.34);
    background: #c94158;
    color: #fff5f6;
  }

  .ok {
    color: #3ad29a;
  }

  .warn {
    color: #ffbf57;
  }

  .danger {
    color: #ff6f82;
  }

  .dashboard {
    grid-template-columns: minmax(280px, 0.66fr) minmax(560px, 1.55fr) minmax(390px, 0.95fr);
    grid-template-areas:
      "replay vehicle map"
      "replay attitude map"
      "replay plot events"
      "recording plot topics";
    gap: 10px;
    margin-top: 10px;
  }

  .vehicle-panel {
    grid-area: vehicle;
  }

  .map-panel {
    grid-area: map;
  }

  .attitude-panel {
    grid-area: attitude;
  }

  .replay-panel {
    grid-area: replay;
  }

  .recording-panel {
    grid-area: recording;
  }

  .plot-panel {
    grid-area: plot;
  }

  .events-panel {
    grid-area: events;
  }

  .topics-panel {
    grid-area: topics;
  }

  .map-panel,
  .topics-panel,
  .events-panel {
    grid-row: auto;
  }

  .panel {
    position: relative;
    overflow: hidden;
    min-width: 0;
    padding: 12px;
  }

  .panel::before {
    position: absolute;
    inset: 0 0 auto;
    height: 2px;
    background: linear-gradient(90deg, #fd7719, rgba(253, 119, 25, 0.55), transparent 78%);
    content: "";
    opacity: 0.62;
  }

  .panel-heading {
    align-items: center;
    margin-bottom: 10px;
  }

  .panel-heading > :global(svg) {
    color: #697c75;
  }

  .map-view-control {
    border-color: #2d3a41;
    background: #0b1012;
  }

  .map-view-control button {
    color: #8fa09a;
  }

  .map-view-control button + button {
    border-left-color: #2d3a41;
  }

  .map-view-control button.active {
    background: #dfe9e4;
    color: #0a1111;
  }

  .field {
    gap: 7px;
    color: #91a39c;
    font-size: 0.76rem;
    font-weight: 700;
    text-transform: uppercase;
  }

  .field.inline {
    min-width: 0;
  }

  .manual-service {
    display: grid;
    gap: 8px;
    margin-top: 8px;
  }

  .manual-actions {
    margin-bottom: 10px;
  }

  .keyboard-map {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 6px;
    margin-top: 8px;
  }

  .keyboard-map > div {
    display: grid;
    gap: 4px;
    min-height: 48px;
    padding: 8px 10px;
    border: 1px solid rgba(253, 119, 25, 0.24);
    border-radius: 8px;
    background: rgba(253, 119, 25, 0.08);
  }

  .keyboard-map span {
    color: #91a39c;
    font-size: 0.58rem;
    font-weight: 820;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .keyboard-map strong {
    color: #ffd9b8;
    font-size: 0.76rem;
    font-weight: 780;
  }

  .manual-service > div {
    display: grid;
    gap: 6px;
    min-height: 56px;
    padding: 10px 12px;
    border: 1px solid #27343a;
    border-radius: 8px;
    background: #0c1214;
  }

  .manual-service span {
    color: #91a39c;
    font-size: 0.7rem;
    font-weight: 760;
    text-transform: uppercase;
  }

  .manual-service strong {
    color: #edf6f1;
    overflow-wrap: anywhere;
    font-size: 0.9rem;
  }

  input,
  select {
    min-height: 42px;
    border: 1px solid #314148;
    border-radius: 8px;
    background: #0b1113;
    color: #edf6f1;
    outline: none;
  }

  input:focus,
  select:focus {
    border-color: rgba(253, 119, 25, 0.72);
    box-shadow: 0 0 0 3px rgba(253, 119, 25, 0.13);
  }

  select {
    color-scheme: dark;
  }

  .metrics {
    gap: 8px;
  }


  .metric {
    min-height: 82px;
    border: 1px solid #27343a;
    border-radius: 8px;
    background:
      linear-gradient(180deg, rgba(255, 255, 255, 0.035), transparent),
      #0e1517;
  }

  .metric span {
    color: #84938d;
    font-size: 0.72rem;
    font-weight: 760;
    text-transform: uppercase;
  }

  .io-group h3 {
    color: #84938d;
  }

  .mode-readout {
    border: 1px solid #27343a;
    background:
      linear-gradient(180deg, rgba(255, 255, 255, 0.035), transparent),
      #0e1517;
  }

  .mode-readout span,
  .mode-readout em {
    color: #84938d;
  }

  .mode-readout strong {
    color: #f3fbf7;
  }

  .mode-readout strong.on {
    color: #fd7719;
  }

  .io-row {
    border: 1px solid #27343a;
    background:
      linear-gradient(180deg, rgba(255, 255, 255, 0.035), transparent),
      #0e1517;
  }

  .io-row span {
    color: #84938d;
  }

  .io-row strong.on {
    color: #fd7719;
  }

  .metric strong {
    color: #f3fbf7;
    font-size: 1.42rem;
    font-weight: 780;
  }

  .map-canvas {
    height: 420px;
    border: 1px solid #2a383f;
    border-radius: 8px;
    background:
      linear-gradient(90deg, rgba(99, 178, 161, 0.12) 1px, transparent 1px),
      linear-gradient(rgba(99, 178, 161, 0.12) 1px, transparent 1px),
      linear-gradient(135deg, rgba(253, 119, 25, 0.18), transparent 34%),
      linear-gradient(155deg, #162124, #10181b 58%, #0d1315);
    background-size: 30px 30px, 30px 30px, auto, auto;
  }

  .map-panel .map-canvas,
  :global(.map-panel .indoor-scene) {
    height: calc(100% - 50px);
    min-height: 420px;
  }

  .vehicle-marker {
    width: 46px;
    height: 46px;
    background: transparent;
    color: #fd7719;
  }

  .map-vehicle-body {
    fill: #fd7719;
    stroke: #050808;
    stroke-width: 1.1;
    stroke-linejoin: round;
  }

  .map-vehicle-nose {
    fill: #f9fffc;
  }

  .map-vehicle-arm {
    stroke: #d3ddd7;
    stroke-width: 3;
    stroke-linecap: round;
  }

  .map-vehicle-rotor {
    fill: rgba(211, 221, 215, 0.28);
    stroke: #d3ddd7;
    stroke-width: 1.6;
  }

  .map-vehicle-rotor.front {
    fill: rgba(253, 119, 25, 0.5);
    stroke: #fd7719;
  }

  .map-vehicle-hub {
    fill: #050808;
    stroke: #fd7719;
    stroke-width: 1.6;
  }

  .map-home,
  .map-scale {
    border: 1px solid #2a383f;
    border-radius: 8px;
    background: rgba(9, 14, 15, 0.82);
    color: #dfe9e4;
    font-size: 0.72rem;
  }

  .map-mission-path {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }

  .map-mission-path polyline {
    fill: none;
    stroke: rgba(255, 195, 90, 0.6);
    stroke-width: 1.5px;
    stroke-dasharray: 4 3;
    vector-effect: non-scaling-stroke;
  }

  .map-waypoint {
    position: absolute;
    display: grid;
    width: 22px;
    height: 22px;
    place-items: center;
    transform: translate(-50%, -50%);
    border: 1.5px solid #d3ddd7;
    border-radius: 50%;
    background: rgba(9, 14, 15, 0.82);
    color: #dfe9e4;
    font-size: 0.68rem;
    font-weight: 760;
    font-variant-numeric: tabular-nums;
  }

  .map-waypoint.active {
    border-color: #fd7719;
    background: rgba(253, 119, 25, 0.28);
    color: #ffd9b8;
    box-shadow: 0 0 0 4px rgba(253, 119, 25, 0.18);
  }

  .attitude-display {
    min-height: 360px;
    height: calc(100% - 78px);
    padding: 10px;
    border: 1px solid #2a383f;
    border-radius: 8px;
    background: radial-gradient(circle at 50% 42%, #0c1614, #05090a 78%);
  }

  /* Symbolic head-up display — phosphor-orange strokes on a dark combiner. */
  .hud-svg {
    --hud: #ff8324;
    width: 100%;
    height: 100%;
    max-width: 540px;
    max-height: 100%;
    filter: drop-shadow(0 0 3px rgba(255, 131, 36, 0.5));
  }

  .hud-line {
    fill: none;
    stroke: var(--hud);
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .hud-line.thin {
    stroke-width: 1.4;
  }

  .hud-line.horizon {
    stroke-width: 2.6;
  }

  .hud-dash .hud-line {
    stroke-dasharray: 7 6;
  }

  .hud-fill {
    fill: var(--hud);
    stroke: none;
  }

  .hud-box {
    fill: rgba(5, 9, 10, 0.55);
    stroke: var(--hud);
    stroke-width: 2;
  }

  .hud-frame {
    fill: none;
    stroke: var(--hud);
    stroke-width: 1.4;
    stroke-opacity: 0.35;
  }

  .hud-text {
    fill: var(--hud);
    font-family: ui-monospace, "SFMono-Regular", "JetBrains Mono", Menlo, monospace;
    font-weight: 600;
  }

  .hud-text.sm {
    font-size: 13px;
    letter-spacing: 0.5px;
  }

  .hud-text.num {
    font-size: 13px;
    font-weight: 700;
  }

  .hud-text.lg {
    font-size: 21px;
    font-weight: 700;
    letter-spacing: 1px;
  }

  .hud-text.dim {
    fill-opacity: 0.6;
  }

  /* Light theme — dark-orange strokes on a pale combiner, no phosphor glow. */
  :global(html[data-theme='light']) .attitude-display {
    border-color: #d9dee3;
    background: radial-gradient(circle at 50% 42%, #f6f2ec, #eef1f4 78%);
  }

  :global(html[data-theme='light']) .hud-svg {
    --hud: #c2510a;
    filter: none;
  }

  :global(html[data-theme='light']) .hud-box {
    fill: rgba(255, 255, 255, 0.7);
  }

  .range,
  input[type="range"] {
    min-width: 0;
    width: 100%;
    accent-color: #fd7719;
  }

  .plot {
    height: 154px;
    border: 1px solid #2a383f;
    border-radius: 8px;
    background:
      linear-gradient(90deg, rgba(255, 255, 255, 0.045) 1px, transparent 1px),
      linear-gradient(rgba(255, 255, 255, 0.035) 1px, transparent 1px),
      #0b1113;
    background-size: 32px 32px, 32px 32px, auto;
  }

  .plot-line {
    stroke-width: 2.6;
  }

  .plot-controls {
    display: grid;
    gap: 8px;
    margin-bottom: 10px;
  }

  .plot-trace-control {
    display: grid;
    grid-template-columns: 10px minmax(0, 1fr) minmax(0, 0.9fr);
    gap: 8px;
    align-items: end;
  }

  .plot-trace-control label {
    display: grid;
    gap: 4px;
    min-width: 0;
  }

  .plot-trace-control label span {
    overflow: hidden;
    color: #778982;
    font-size: 0.64rem;
    font-weight: 760;
    text-overflow: ellipsis;
    text-transform: uppercase;
    white-space: nowrap;
  }

  .plot-trace-control select {
    min-height: 34px;
    padding: 0 8px;
    font-size: 0.76rem;
    font-weight: 680;
  }

  .plot-swatch {
    display: block;
    width: 10px;
    height: 10px;
    align-self: center;
    border-radius: 999px;
    box-shadow: 0 0 14px currentColor;
  }

  .plot-frame {
    position: relative;
    overflow: hidden;
    border-radius: 8px;
  }

  .plot {
    height: 132px;
  }

  .plot-grid-line {
    stroke: rgba(255, 255, 255, 0.08);
    stroke-width: 1;
  }

  .plot-empty {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    color: #778982;
    font-size: 0.78rem;
    font-weight: 700;
    pointer-events: none;
  }

  .plot-legend {
    display: grid;
    gap: 6px;
    margin-top: 10px;
  }

  .plot-legend > div {
    display: grid;
    grid-template-columns: 10px minmax(0, 1fr) auto;
    gap: 8px;
    align-items: center;
    min-width: 0;
    color: #91a39c;
    font-size: 0.72rem;
  }

  .plot-legend strong {
    overflow: hidden;
    color: #edf6f1;
    font-size: 0.78rem;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .plot-legend span:last-child {
    overflow: hidden;
    text-align: right;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .event-list,
  .topic-table {
    gap: 4px;
  }

  .event-row,
  .topic-row,
  .empty-row {
    min-height: 34px;
    border: 1px solid #233035;
    border-radius: 8px;
    background: #0c1214;
    color: #d7e2dd;
    font-size: 0.78rem;
  }

  .event-row strong {
    font-weight: 680;
  }

  .topic-row {
    grid-template-columns: minmax(96px, 1fr) 72px 72px 60px;
  }

  .topic-row.header {
    border-color: transparent;
    background: transparent;
    color: #778982;
    font-size: 0.68rem;
    text-transform: uppercase;
  }

  .empty-row {
    color: #778982;
  }

  @media (max-width: 1240px) {
    .dashboard {
      grid-template-columns: minmax(280px, 0.9fr) minmax(360px, 1.1fr);
      grid-template-areas:
        "replay vehicle"
        "replay map"
        "attitude map"
        "replay events"
        "recording topics"
        "plot plot";
    }

    .map-canvas {
      height: 380px;
    }
  }

  @media (max-width: 820px) {
    .shell {
      padding: 8px;
    }

    .topbar,
    .dashboard {
      grid-template-columns: 1fr;
    }

    .topbar {
      gap: 10px;
    }

    .dashboard {
      grid-template-areas:
        "vehicle"
        "map"
        "attitude"
        "replay"
        "recording"
        "plot"
        "events"
        "topics";
    }

    .map-canvas {
      height: 300px;
    }

    .metrics {
      grid-template-columns: 1fr 1fr;
    }

    .topic-row {
      grid-template-columns: minmax(82px, 1fr) 62px 62px 52px;
      font-size: 0.74rem;
    }
  }

  .dashboard {
    grid-template-areas: none;
    grid-auto-flow: dense;
    grid-auto-rows: 8px;
    gap: 0 10px;
    align-items: stretch;
  }

  .panel {
    height: calc(100% - 10px);
  }

  .autopilot-panel {
    grid-area: auto;
    grid-column: 1;
    grid-row: span 42;
  }

  .io-panel {
    grid-area: auto;
    grid-column: 3;
    grid-row: 1 / span 106;
  }

  .manual-panel {
    grid-area: auto;
    grid-column: 3;
    grid-row: 107 / span 76;
  }

  .replay-panel {
    grid-area: auto;
    grid-column: 1;
    grid-row: span 27;
  }

  .recording-panel {
    grid-area: auto;
    grid-column: 1;
    grid-row: span 14;
  }

  .vehicle-panel {
    grid-area: auto;
    grid-column: 3;
    grid-row: span 30;
  }

  .attitude-panel {
    grid-area: auto;
    grid-column: 1;
    grid-row: 1 / span 68;
  }

  .plot-panel {
    grid-area: auto;
    grid-column: 2;
    grid-row: span 48;
  }

  .map-panel {
    grid-area: auto;
    grid-column: 2;
    grid-row: span 70;
  }

  .deflect-panel {
    grid-area: auto;
    grid-column: 2;
    grid-row: span 55;
  }

  .events-panel {
    grid-area: auto;
    grid-column: 1;
    grid-row: span 18;
  }

  .topics-panel {
    grid-area: auto;
    grid-column: 1;
    grid-row: span 35;
  }

  .discovery-panel {
    grid-area: auto;
    grid-column: 2;
    grid-row: span 30;
  }

  .events-panel .event-list,
  .topics-panel .topic-table,
  .discovery-panel .discovery-table {
    max-height: calc(100% - 42px);
    overflow: auto;
    padding-right: 2px;
  }

  @media (max-width: 1240px) {
    .dashboard {
      grid-template-columns: minmax(280px, 0.85fr) minmax(380px, 1.15fr);
      grid-auto-rows: 8px;
      gap: 0 10px;
    }

    .autopilot-panel,
    .replay-panel,
    .recording-panel {
      grid-column: 1;
    }

    .attitude-panel {
      grid-column: 1;
      grid-row: 1 / span 68;
    }

    .vehicle-panel,
    .io-panel,
    .manual-panel,
    .plot-panel,
    .map-panel,
    .deflect-panel,
    .events-panel,
    .topics-panel,
    .discovery-panel {
      grid-column: 2;
    }

    .map-panel {
      grid-row: span 55;
    }

    .io-panel {
      grid-row: 1 / span 106;
    }

    .manual-panel {
      grid-row: 107 / span 60;
    }

    .deflect-panel {
      grid-row: span 52;
    }

    .plot-panel {
      grid-row: span 49;
    }
  }

  @media (max-width: 820px) {
    .dashboard {
      display: grid;
      grid-template-columns: 1fr;
      grid-auto-rows: auto;
    }

    .autopilot-panel,
    .io-panel,
    .manual-panel,
    .replay-panel,
    .recording-panel,
    .vehicle-panel,
    .attitude-panel,
    .plot-panel,
    .map-panel,
    .deflect-panel,
    .events-panel,
    .topics-panel,
    .discovery-panel {
      grid-column: 1;
      grid-row: auto;
      height: auto;
    }

    .attitude-panel {
      order: -30;
    }

    .io-panel {
      order: -20;
    }

    .manual-panel {
      order: -19;
    }

    .events-panel .event-list,
    .topics-panel .topic-table,
    .discovery-panel .discovery-table {
      max-height: none;
      overflow: visible;
    }

    .map-heading {
      display: grid;
      grid-template-columns: 1fr;
    }

    .map-tools {
      width: 100%;
      justify-content: space-between;
    }

    .map-view-control {
      flex: 1;
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  /* Light theme — matches the light electrode logo (orange accent on near-white) */
  :global(html[data-theme='light']) {
    background: linear-gradient(180deg, #eef1f3 0%, #f4f6f8 52%, #e9edf0 100%);
    color: #12171b;
  }

  :global(html[data-theme='light'] body) {
    background: #eef1f3;
  }

  :global(html[data-theme='light']) .shell {
    background:
      linear-gradient(90deg, rgba(20, 30, 40, 0.03) 1px, transparent 1px),
      linear-gradient(rgba(20, 30, 40, 0.025) 1px, transparent 1px),
      linear-gradient(180deg, #eef1f3, #f4f6f8 48%, #e9edf0);
    background-size: 36px 36px, 36px 36px, auto;
  }

  :global(html[data-theme='light']) .topbar,
  :global(html[data-theme='light']) .ground-nav,
  :global(html[data-theme='light']) .panel {
    border-color: #d9dee3;
    background: #ffffff;
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.6),
      0 10px 26px rgba(20, 30, 40, 0.06);
  }

  :global(html[data-theme='light']) .topbar {
    background: #ffffff;
  }

  :global(html[data-theme='light']) .ground-nav {
    background: #ffffff;
  }

  :global(html[data-theme='light']) .ground-nav button {
    border-left-color: #d9dee3;
    color: #5c6873;
  }

  :global(html[data-theme='light']) .ground-nav button.active {
    background: #12171b;
    color: #ffffff;
  }

  :global(html[data-theme='light']) .config-panel summary {
    border-color: #d9dee3;
    background: #ffffff;
    color: #5c6873;
  }

  :global(html[data-theme='light']) h1,
  :global(html[data-theme='light']) h2 {
    color: #12171b;
  }

  :global(html[data-theme='light']) p {
    color: #5c6873;
  }

  :global(html[data-theme='light']) .icon-button.quiet,
  :global(html[data-theme='light']) .file-button {
    border-color: #d3dade;
    background: #eef2f4;
    color: #263039;
  }

  :global(html[data-theme='light']) .icon-button.primary {
    border-color: rgba(227, 95, 12, 0.5);
    background: #e35f0c;
    color: #ffffff;
  }

  :global(html[data-theme='light']) .icon-button.danger {
    border-color: rgba(194, 59, 72, 0.4);
    background: #c23b48;
    color: #ffffff;
  }

  :global(html[data-theme='light']) .ok {
    color: #12885c;
  }

  :global(html[data-theme='light']) .warn {
    color: #b7791f;
  }

  :global(html[data-theme='light']) .danger {
    color: #c23b48;
  }

  :global(html[data-theme='light']) .panel::before {
    background: linear-gradient(90deg, #e35f0c, rgba(227, 95, 12, 0.5), transparent 78%);
    opacity: 0.5;
  }

  :global(html[data-theme='light']) .panel-heading > :global(svg) {
    color: #97a2ab;
  }

  :global(html[data-theme='light']) .map-view-control {
    border-color: #cdd5dc;
    background: #f5f7f8;
  }

  :global(html[data-theme='light']) .map-view-control button {
    color: #53606a;
  }

  :global(html[data-theme='light']) .map-view-control button + button {
    border-left-color: #cdd5dc;
  }

  :global(html[data-theme='light']) .map-view-control button.active {
    background: #12171b;
    color: #ffffff;
  }

  :global(html[data-theme='light']) .field {
    color: #5c6873;
  }

  :global(html[data-theme='light']) .manual-service > div {
    border-color: #e0e5ea;
    background: #f5f7f8;
  }

  :global(html[data-theme='light']) .keyboard-map > div {
    border-color: rgba(227, 95, 12, 0.24);
    background: rgba(227, 95, 12, 0.08);
  }

  :global(html[data-theme='light']) .keyboard-map span {
    color: #6b7680;
  }

  :global(html[data-theme='light']) .keyboard-map strong {
    color: #a04208;
  }

  :global(html[data-theme='light']) .manual-service span {
    color: #6b7680;
  }

  :global(html[data-theme='light']) .manual-service strong {
    color: #263039;
  }

  :global(html[data-theme='light']) input,
  :global(html[data-theme='light']) select {
    border-color: #cfd6dd;
    background: #ffffff;
    color: #1d252c;
  }

  :global(html[data-theme='light']) input:focus,
  :global(html[data-theme='light']) select:focus {
    border-color: rgba(227, 95, 12, 0.7);
    box-shadow: 0 0 0 3px rgba(227, 95, 12, 0.14);
  }

  :global(html[data-theme='light']) select {
    color-scheme: light;
  }

  :global(html[data-theme='light']) .metric {
    border-color: #e0e5ea;
    background: linear-gradient(180deg, #ffffff, #f7fafc);
  }

  :global(html[data-theme='light']) .metric span {
    color: #6b7680;
  }

  :global(html[data-theme='light']) .metric strong {
    color: #12171b;
  }

  :global(html[data-theme='light']) .mode-readout {
    border-color: #e0e5ea;
    background: linear-gradient(180deg, #ffffff, #f7fafc);
  }

  :global(html[data-theme='light']) .mode-readout span,
  :global(html[data-theme='light']) .mode-readout em {
    color: #6b7680;
  }

  :global(html[data-theme='light']) .mode-readout strong {
    color: #12171b;
  }

  :global(html[data-theme='light']) .mode-readout strong.on {
    color: #e35f0c;
  }

  :global(html[data-theme='light']) .map-canvas {
    border-color: #cdd5dc;
    background:
      linear-gradient(90deg, rgba(31, 72, 61, 0.1) 1px, transparent 1px),
      linear-gradient(rgba(31, 72, 61, 0.1) 1px, transparent 1px),
      linear-gradient(135deg, #edf4ef, #eef2f6 62%, #f8f9f6);
    background-size: 30px 30px, 30px 30px, auto;
  }

  :global(html[data-theme='light']) .vehicle-marker {
    background: transparent;
    color: #fd7719;
  }

  :global(html[data-theme='light']) .map-home,
  :global(html[data-theme='light']) .map-scale {
    border-color: #cdd5dc;
    background: rgba(255, 255, 255, 0.86);
    color: #47535f;
  }

  :global(html[data-theme='light']) .map-mission-path polyline {
    stroke: rgba(196, 131, 28, 0.7);
  }

  :global(html[data-theme='light']) .map-waypoint {
    border-color: #9aa7b0;
    background: rgba(255, 255, 255, 0.88);
    color: #47535f;
  }

  :global(html[data-theme='light']) .map-waypoint.active {
    border-color: #e35f0c;
    background: rgba(227, 95, 12, 0.16);
    color: #a04208;
    box-shadow: 0 0 0 4px rgba(227, 95, 12, 0.14);
  }

  :global(html[data-theme='light']) .plot {
    border-color: #d8dee4;
    background:
      linear-gradient(90deg, rgba(20, 30, 40, 0.05) 1px, transparent 1px),
      linear-gradient(rgba(20, 30, 40, 0.04) 1px, transparent 1px),
      #ffffff;
    background-size: 32px 32px, 32px 32px, auto;
  }

  :global(html[data-theme='light']) .plot-grid-line {
    stroke: rgba(20, 30, 40, 0.08);
  }

  :global(html[data-theme='light']) .plot-empty,
  :global(html[data-theme='light']) .plot-trace-control label span,
  :global(html[data-theme='light']) .plot-legend > div {
    color: #6b7680;
  }

  :global(html[data-theme='light']) .plot-legend strong {
    color: #12171b;
  }

  :global(html[data-theme='light']) .event-row,
  :global(html[data-theme='light']) .topic-row,
  :global(html[data-theme='light']) .empty-row {
    border-color: #e0e5ea;
    background: #f6f8fa;
    color: #35424b;
  }

  :global(html[data-theme='light']) .topic-row.header {
    border-color: transparent;
    background: transparent;
    color: #6b7680;
  }

  :global(html[data-theme='light']) .empty-row {
    color: #6b7680;
  }
</style>

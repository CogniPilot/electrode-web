<script lang="ts">
  export let theme: 'light' | 'dark' = 'dark';
  export let autopilotRunning = false;
  export let status = '';
  export let values: Record<string, number> = {};
  export let onParameter: (name: string, value: number) => void;
  export let onRefresh: (names: string[]) => void;
  export let onTrajectory: (waypoints: Array<{ east: number; north: number; up: number }>) => void;

  type Parameter = { name: string; label: string; value: number; min: number; max: number; step: number };
  let parameters: Parameter[] = [
    { name: 'velocity.setpoint', label: 'Cruise speed / velocity setpoint', value: 4.5, min: 1, max: 12, step: 0.1 },
    { name: 'route.altitudeToFlightPathGain', label: 'Altitude-to-flight-path gain', value: 2, min: 0, max: 10, step: 0.05 },
    { name: 'route.altitudeLookaheadDistance', label: 'Altitude lookahead distance', value: 8, min: 0.5, max: 50, step: 0.1 },
    { name: 'route.flightPathAngleLimit', label: 'Flight-path angle limit', value: 0.12, min: 0.02, max: 0.7, step: 0.01 },
    { name: 'route.crossTrackSteeringDistance', label: 'Cross-track steering distance', value: 4.25, min: 0.25, max: 50, step: 0.1 },
    { name: 'route.waypointSwitchingDistance', label: 'Waypoint switching distance', value: 4, min: 0.1, max: 50, step: 0.1 },
    { name: 'tecs.thrustKp', label: 'TECS thrust Kp', value: 0.05, min: 0, max: 2, step: 0.001 },
    { name: 'tecs.thrustKi', label: 'TECS thrust Ki', value: 0.004854, min: 0, max: 2, step: 0.001 },
    { name: 'tecs.pitchKp', label: 'TECS pitch Kp', value: 0.075, min: 0, max: 2, step: 0.001 },
    { name: 'tecs.pitchKi', label: 'TECS pitch Ki', value: 0, min: 0, max: 2, step: 0.001 },
    { name: 'attitude.rollLimit', label: 'Roll limit', value: 0.523599, min: 0.05, max: 1.2, step: 0.01 },
    { name: 'attitude.rollRateLimit', label: 'Roll-rate limit', value: 2.094395, min: 0.1, max: 8, step: 0.1 },
    { name: 'attitude.headingPid.kp', label: 'Heading PID Kp', value: 0.5, min: 0, max: 10, step: 0.001 },
    { name: 'attitude.headingPid.ki', label: 'Heading PID Ki', value: 0, min: 0, max: 10, step: 0.001 },
    { name: 'attitude.headingPid.kd', label: 'Heading PID Kd', value: 0.5, min: 0, max: 10, step: 0.001 },
    { name: 'attitude.pitchPid.kp', label: 'Pitch PID Kp', value: 2.3, min: 0, max: 10, step: 0.001 },
    { name: 'attitude.pitchPid.ki', label: 'Pitch PID Ki', value: 0, min: 0, max: 10, step: 0.001 },
    { name: 'attitude.pitchPid.kd', label: 'Pitch PID Kd', value: 0, min: 0, max: 10, step: 0.001 }
  ];

  let localStatus = '';
  let waypointText = `0, 0, 0
-8, -8, 3
-8, 2, 3
18, 2, 3
18, -8, 3
5, -8, 3
-8, -8, 3`;

  function syncRuntimeValues(nextValues: Record<string, number>): void {
    parameters = parameters.map((parameter) => {
      const current = nextValues[parameter.name];
      return Number.isFinite(current) ? { ...parameter, value: current } : parameter;
    });
  }

  $: syncRuntimeValues(values);

  function applyParameter(parameter: Parameter): boolean {
    if (!autopilotRunning) {
      localStatus = 'Start the autopilot before applying parameter configuration';
      return false;
    }
    if (!Number.isFinite(parameter.value)) {
      localStatus = `${parameter.label} must be a finite number`;
      return false;
    }
    localStatus = `sending ${parameter.label}…`;
    onParameter(parameter.name, parameter.value);
    return true;
  }

  function applyAll(): void {
    for (const parameter of parameters) {
      if (!applyParameter(parameter)) {
        return;
      }
    }
    if (parameters.length === 0) {
      localStatus = 'no parameters to send';
      return;
    }
    localStatus = `sent ${parameters.length} parameters`;
  }

  function applyTrajectory(): void {
    if (!autopilotRunning) {
      localStatus = 'Start the autopilot before uploading a trajectory';
      return;
    }
    try {
      const waypoints = waypointText
        .split('\n')
        .map((line) => line.trim())
        .filter(Boolean)
        .map((line, index) => {
          const values = line.split(',').map((value) => Number(value.trim()));
          if (values.length !== 3 || values.some((value) => !Number.isFinite(value))) {
            throw new Error(`line ${index + 1} must be east, north, up`);
          }
          return { east: values[0], north: values[1], up: values[2] };
        });
      if (waypoints.length < 2 || waypoints.length > 7) {
        throw new Error('enter 2 to 7 waypoints');
      }
      localStatus = 'sending trajectory…';
      onTrajectory(waypoints);
    } catch (error) {
      localStatus = error instanceof Error ? error.message : 'invalid waypoints';
    }
  }
</script>

<section class="runtime-tuning" class:light={theme === 'light'}>
  <div class="head">
    <div>
      <h3>Live Autopilot Configuration</h3>
      <p>All supported values are shown below. Changes are sent over Zenoh and applied atomically at the next control-loop boundary, including while armed.</p>
    </div>
    <span>{status || localStatus || 'ready'}</span>
  </div>

  <div class="parameter-head">
    <strong>Parameters</strong>
    <div class="parameter-actions">
      <button type="button" class="secondary" disabled={!autopilotRunning} onclick={() => onRefresh(parameters.map(({ name }) => name))}>Refresh</button>
      <button type="button" disabled={!autopilotRunning} onclick={applyAll}>Apply all</button>
    </div>
  </div>
  {#if !autopilotRunning}
    <div class="autopilot-warning">The autopilot must be running to read or apply parameter configuration.</div>
  {/if}
  <div class="parameter-table">
    {#each parameters as parameter (parameter.name)}
      <div class="parameter-row">
        <label for={`runtime-${parameter.name}`}>
          <strong>{parameter.label}</strong>
          <span>{parameter.name}</span>
        </label>
        <input id={`runtime-${parameter.name}`} type="number" step={parameter.step} bind:value={parameter.value} />
        <button type="button" disabled={!autopilotRunning} onclick={() => applyParameter(parameter)}>Apply</button>
      </div>
    {/each}
  </div>

  <div class="grid">
    <label class="wide">
      <span>Waypoints — one ENU “east, north, up” point per line; 2–7 points</span>
      <textarea bind:value={waypointText} spellcheck="false"></textarea>
    </label>
    <button class="wide" type="button" disabled={!autopilotRunning} onclick={applyTrajectory}>Upload trajectory</button>
  </div>
</section>

<style>
  .runtime-tuning {
    display: grid;
    gap: 12px;
    padding: 12px;
    border: 1px solid rgba(145, 163, 156, 0.24);
    border-radius: 8px;
    background: rgba(255, 255, 255, 0.04);
    color: #edf6f1;
  }
  .runtime-tuning.light { color: #12171b; background: rgba(255, 255, 255, 0.74); }
  .head { display: flex; justify-content: space-between; gap: 16px; }
  h3, p { margin: 0; }
  h3 { font-size: 0.86rem; }
  p, .head span, label span { color: #91a39c; font-size: 0.68rem; font-weight: 700; }
  p { margin-top: 4px; }
  .parameter-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .parameter-actions { display: flex; gap: 8px; }
  .parameter-head strong { font-size: 0.72rem; text-transform: uppercase; letter-spacing: 0.06em; }
  .autopilot-warning { padding: 9px 11px; border: 1px solid rgba(221, 107, 32, 0.55); border-radius: 7px; background: rgba(221, 107, 32, 0.12); color: #f6ad55; font-size: 0.72rem; font-weight: 750; }
  .parameter-table { display: grid; gap: 6px; }
  .parameter-row {
    display: grid;
    grid-template-columns: minmax(240px, 1fr) minmax(110px, 180px) auto;
    gap: 10px;
    align-items: center;
    padding: 8px;
    border: 1px solid rgba(145, 163, 156, 0.16);
    border-radius: 7px;
    background: rgba(0, 0, 0, 0.12);
  }
  .parameter-row label { gap: 2px; }
  .parameter-row label strong { font-size: 0.72rem; }
  .parameter-row label span { font-family: ui-monospace, monospace; font-weight: 500; }
  .grid { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 10px; align-items: end; }
  label { display: grid; gap: 5px; min-width: 0; }
  .wide { grid-column: 1 / -1; }
  input, textarea, button {
    min-width: 0;
    border: 1px solid rgba(145, 163, 156, 0.28);
    border-radius: 7px;
    background: rgba(0, 0, 0, 0.26);
    color: inherit;
    padding: 0 9px;
    font: inherit;
    font-size: 0.72rem;
  }
  input, button { height: 34px; }
  textarea { min-height: 150px; padding: 9px; resize: vertical; font-family: ui-monospace, monospace; }
  button { padding: 0 16px; cursor: pointer; font-weight: 800; background: #dd6b20; color: white; border-color: #dd6b20; }
  button:disabled { cursor: not-allowed; opacity: 0.45; }
  button.secondary { background: transparent; color: inherit; border-color: rgba(145, 163, 156, 0.4); }
  .light input, .light textarea { background: rgba(255, 255, 255, 0.8); color: #12171b; }
  @media (max-width: 760px) {
    .grid { grid-template-columns: 1fr; }
    .wide { grid-column: auto; }
    .parameter-row { grid-template-columns: 1fr minmax(90px, 140px); }
    .parameter-row button { grid-column: 1 / -1; }
  }
</style>

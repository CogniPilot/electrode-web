<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import {
    fetchMapping,
    saveMapping,
    fetchDevices,
    openJoystickSocket,
    type MappingProfile,
    type JoystickSnapshot,
    type DetectedDevice
  } from '$lib/gcs';
  import { mappingProfile } from '$lib/mappingStore';

  export let theme: 'light' | 'dark' = 'dark';

  type AxisFn = { key: string; label: string; axis: keyof MappingProfile; invert: keyof MappingProfile | null };
  type ButtonFn = { key: string; label: string; field: keyof MappingProfile; toggle: keyof MappingProfile };

  // AETR order: Aileron (Roll), Elevator (Pitch), Throttle, Rudder (Yaw).
  const AXIS_FUNCTIONS: AxisFn[] = [
    { key: 'roll', label: 'Roll (A)', axis: 'rollAxis', invert: 'invertRoll' },
    { key: 'pitch', label: 'Pitch (E)', axis: 'pitchAxis', invert: 'invertPitch' },
    { key: 'throttle', label: 'Throttle (T)', axis: 'throttleAxis', invert: 'invertThrottle' },
    { key: 'yaw', label: 'Yaw (R)', axis: 'yawAxis', invert: 'invertYaw' },
    { key: 'mode', label: 'Mode', axis: 'modeAxis', invert: null },
    { key: 'active', label: 'Stabilization', axis: 'activeAxis', invert: 'invertActive' }
  ];
  const BUTTON_FUNCTIONS: ButtonFn[] = [
    { key: 'arm', label: 'Arm', field: 'armButton', toggle: 'armToggle' },
    { key: 'kill', label: 'Kill', field: 'killButton', toggle: 'killToggle' }
  ];
  const POLL_MS = 2000;

  let profile: MappingProfile | null = null;
  let joysticks: DetectedDevice[] = [];
  let snapshot: JoystickSnapshot | null = null;
  let status = '';
  let socket: WebSocket | null = null;
  let connectedDevice: string | null = null;
  let showInspector = true;
  let saveTimer: ReturnType<typeof setTimeout> | null = null;
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let inFlight: AbortController | null = null;

  // Derive reactive arrays so the template references `axes`/`buttons` directly
  // — a function call would hide the `snapshot` dependency from Svelte.
  $: axes = snapshot?.axes ?? [];
  $: buttons = snapshot?.buttons ?? [];
  $: axisCount = Math.max(axes.length, 6);
  $: buttonCount = Math.max(buttons.length, 4);
  $: controllerMissing = joysticks.length === 0;

  function setField<K extends keyof MappingProfile>(field: K, value: MappingProfile[K]): void {
    if (!profile) return;
    profile = { ...profile, [field]: value };
    mappingProfile.set(profile); // reflect edits immediately in the Manual Link hardware view
    scheduleSave();
  }

  // Auto-save: debounce so rapid edits collapse into one PUT.
  function scheduleSave(): void {
    status = 'saving…';
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => void doSave(), 400);
  }

  async function doSave(): Promise<void> {
    if (!profile) return;
    try {
      profile = await saveMapping(profile);
      mappingProfile.set(profile);
      status = 'saved';
    } catch (err) {
      status = err instanceof Error ? err.message : 'save failed';
    }
  }

  function setDevice(path: string): void {
    if (!profile || profile.device === path) return;
    profile = { ...profile, device: path };
    mappingProfile.set(profile);
    scheduleSave();
  }

  function connect(device: string): void {
    if (socket && connectedDevice === device) return;
    socket?.close();
    connectedDevice = null;
    snapshot = null;
    if (!device || joysticks.length === 0) return;
    const ws = openJoystickSocket(device);
    socket = ws;
    connectedDevice = device;
    ws.onmessage = (event) => {
      try {
        snapshot = JSON.parse(event.data) as JoystickSnapshot;
      } catch {
        // ignore malformed frames
      }
    };
    ws.onclose = () => {
      if (socket === ws) {
        socket = null;
        connectedDevice = null;
      }
    };
  }

  async function refreshDevices(): Promise<void> {
    inFlight?.abort();
    const controller = new AbortController();
    inFlight = controller;
    try {
      const devices = await fetchDevices(controller.signal);
      if (controller.signal.aborted) return;
      joysticks = devices.joysticks;
      if (!profile) return;
      if (joysticks.length === 0) {
        socket?.close();
        socket = null;
        connectedDevice = null;
        snapshot = null;
        return;
      }
      connect(profile.device);
    } catch (err) {
      if (!controller.signal.aborted) {
        status = err instanceof Error ? err.message : 'device query failed';
      }
    }
  }

  onMount(async () => {
    try {
      profile = await fetchMapping();
      mappingProfile.set(profile);
      await refreshDevices();
      pollTimer = setInterval(() => void refreshDevices(), POLL_MS);
    } catch (err) {
      status = err instanceof Error ? err.message : 'failed to load mapping';
    }
  });

  onDestroy(() => {
    if (pollTimer) clearInterval(pollTimer);
    inFlight?.abort();
    socket?.close();
    if (saveTimer) clearTimeout(saveTimer);
  });
</script>

<section class="rc-map" class:light={theme === 'light'} class:unavailable={controllerMissing}>
  <div class="head">
    <div class="title">
      <h3>RC Mapping</h3>
      <span class="dev">{controllerMissing ? 'no RC controller detected' : (snapshot?.name ?? profile?.device ?? '—')}</span>
      {#if status}<span class="status">{status}</span>{/if}
    </div>
    <div class="actions">
      {#if joysticks.length > 1}
        <select
          class="device"
          value={profile?.device ?? ''}
          onchange={(e) => {
            const d = e.currentTarget.value;
            setDevice(d);
            connect(d);
          }}
        >
          {#each joysticks as js}
            <option value={js.path}>{js.path}</option>
          {/each}
        </select>
      {/if}
      <button
        type="button"
        class="btn"
        disabled={controllerMissing}
        onclick={() => (showInspector = !showInspector)}
        title="Show/hide the live raw-channel monitor"
      >
        {showInspector ? 'Hide channels' : 'Show channels'}
      </button>
    </div>
  </div>

  {#if !profile}
    <div class="status block">loading mapping…</div>
  {:else}
    <div class="grid" class:solo={!showInspector}>
      <!-- Editor -->
      <div class="editor">
        <p class="hint">
          {#if controllerMissing}
            Plug in an RC controller to edit and inspect its mapping. Keyboard fallback is shown in Manual Link.
          {:else}
            Map each control to a controller <b>Axis</b> (<b>Inv</b> flips direction). Buttons can be
            <b>mom</b> (high while held) or <b>tog</b> (press to latch high/low). Changes save
            automatically.
          {/if}
        </p>
        <div class="col-head"><span>Function</span><span>Axis</span><span>Live</span><span>Inv</span></div>
        {#each AXIS_FUNCTIONS as fn}
          {@const p = profile}
          {@const ax = p ? (p[fn.axis] as number) : 0}
          {@const inv = p && fn.invert ? (p[fn.invert] as boolean) : false}
          {@const raw = axes[ax] ?? 0}
          {@const live = inv ? -raw : raw}
          <div class="row">
            <span class="fn">{fn.label}</span>
            <select disabled={controllerMissing} value={ax} onchange={(e) => setField(fn.axis, Number(e.currentTarget.value))}>
              {#each Array(axisCount) as _, i}
                <option value={i}>{i}</option>
              {/each}
            </select>
            <div class="live-bar">
              <div class="center"></div>
              <div
                class="fill"
                style={`left:${50 + (live < 0 ? live * 50 : 0)}%; width:${Math.abs(live) * 50}%;`}
              ></div>
            </div>
            {#if fn.invert}
              <input
                type="checkbox"
                checked={inv}
                disabled={controllerMissing}
                title="Invert this axis"
                onchange={(e) => fn.invert && setField(fn.invert, e.currentTarget.checked)}
              />
            {:else}
              <span></span>
            {/if}
          </div>
        {/each}

        {#each BUTTON_FUNCTIONS as fn}
          {@const p = profile}
          {@const bi = p ? (p[fn.field] as number | null) : null}
          {@const tog = p ? (p[fn.toggle] as boolean) : false}
          <div class="row">
            <span class="fn">{fn.label}</span>
            <select
              disabled={controllerMissing}
              value={bi ?? -1}
              onchange={(e) => {
                const v = Number(e.currentTarget.value);
                setField(fn.field, v < 0 ? null : v);
              }}
            >
              <option value={-1}>none</option>
              {#each Array(buttonCount) as _, i}
                <option value={i}>btn {i}</option>
              {/each}
            </select>
            <div class="live-bar btn-live">
              {#if bi !== null}
                <span class="led" class:on={buttons[bi ?? 0] === 1}></span>
              {/if}
            </div>
            <button
              type="button"
              class="mode-toggle"
              class:tog
              disabled={controllerMissing || bi === null}
              title={tog
                ? 'Toggle: each press latches high/low'
                : 'Momentary: high only while held'}
              onclick={() => setField(fn.toggle, !tog)}
            >
              {tog ? 'tog' : 'mom'}
            </button>
          </div>
        {/each}
      </div>

      <!-- Live raw channels -->
      {#if showInspector}
        <div class="inspector">
          <div class="insp-head">
            Controller input · received <em>(raw from the gamepad)</em>
            {#if controllerMissing}<em>· not plugged in</em>{:else if !snapshot}<em>· waiting…</em>{/if}
          </div>
          <div class="axes">
            {#each Array(axisCount) as _, i}
              {@const v = axes[i] ?? 0}
              <div class="axrow">
                <span class="ai">ax {i}</span>
                <div class="track">
                  <div class="center"></div>
                  <div
                    class="fill"
                    style={`left:${50 + (v < 0 ? v * 50 : 0)}%; width:${Math.abs(v) * 50}%;`}
                  ></div>
                </div>
                <em>{v.toFixed(2)}</em>
              </div>
            {/each}
          </div>
          <div class="buttons">
            {#each Array(buttonCount) as _, i}
              <span class="pill" class:on={buttons[i] === 1}>{i}</span>
            {/each}
          </div>
        </div>
      {/if}
    </div>
  {/if}
</section>

<style>
  .rc-map {
    margin-top: 8px;
    padding: 10px 12px;
    border: 1px solid rgba(253, 119, 25, 0.28);
    border-radius: 10px;
    background: rgba(5, 8, 8, 0.4);
    overflow: hidden;
  }
  .editor {
    min-width: 0;
  }
  .rc-map.light {
    border-color: rgba(227, 95, 12, 0.3);
    background: rgba(255, 255, 255, 0.75);
  }
  .rc-map.unavailable {
    border-color: rgba(145, 163, 156, 0.22);
    background: rgba(8, 12, 13, 0.28);
  }
  .rc-map.unavailable .editor,
  .rc-map.unavailable .inspector {
    opacity: 0.48;
  }
  .rc-map.unavailable .title h3,
  .rc-map.unavailable .dev {
    color: #6f7f79;
  }
  .rc-map.light.unavailable {
    border-color: rgba(120, 132, 140, 0.28);
    background: rgba(238, 242, 244, 0.55);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
  }
  .title {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .title h3 {
    margin: 0;
    font-size: 0.82rem;
    font-weight: 800;
    letter-spacing: 0.04em;
  }
  .dev {
    color: #91a39c;
    font-size: 0.66rem;
    font-weight: 700;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .btn {
    padding: 4px 10px;
    border: 1px solid rgba(253, 119, 25, 0.3);
    border-radius: 7px;
    background: rgba(5, 8, 8, 0.4);
    color: #edf6f1;
    font-size: 0.68rem;
    font-weight: 760;
    cursor: pointer;
  }
  .btn:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .rc-map.light .btn {
    color: #12171b;
    background: rgba(255, 255, 255, 0.8);
  }
  .device {
    padding: 3px 6px;
    border-radius: 6px;
    font-size: 0.66rem;
  }
  .status {
    color: #7ee0ac;
    font-size: 0.6rem;
    font-weight: 700;
  }
  .status.block {
    display: block;
    margin-top: 6px;
  }
  .grid {
    display: grid;
    grid-template-columns: minmax(0, 1.3fr) minmax(0, 1fr);
    gap: 14px;
    margin-top: 10px;
  }
  .grid.solo {
    grid-template-columns: 1fr;
  }
  @media (max-width: 720px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
  .hint {
    margin: 0 0 8px;
    color: #91a39c;
    font-size: 0.62rem;
    line-height: 1.35;
  }
  .hint b {
    color: #fd7719;
    font-weight: 800;
  }
  .rc-map.light .hint {
    color: #5c6873;
  }
  .col-head,
  .row {
    display: grid;
    grid-template-columns: 62px 54px 1fr 42px;
    align-items: center;
    gap: 8px;
  }
  .col-head {
    color: #697c75;
    font-size: 0.54rem;
    font-weight: 800;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin-bottom: 4px;
  }
  .row {
    padding: 3px 0;
  }
  .fn {
    font-size: 0.68rem;
    font-weight: 760;
  }
  .row select,
  .device {
    min-width: 0;
    width: 100%;
    box-sizing: border-box;
    background: #16242a;
    color: #edf6f1;
    border: 1px solid rgba(253, 119, 25, 0.2);
    border-radius: 6px;
  }
  .device {
    width: auto;
    max-width: 150px;
  }
  .rc-map.light .row select,
  .rc-map.light .device {
    background: #eef2f4;
    color: #12171b;
  }
  .live-bar,
  .track {
    position: relative;
    height: 8px;
    border-radius: 3px;
    background: #16242a;
    overflow: hidden;
  }
  .rc-map.light .live-bar,
  .rc-map.light .track {
    background: #dbe2e7;
  }
  .live-bar .center,
  .track .center {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 50%;
    width: 1px;
    background: #3a4c52;
  }
  .live-bar .fill,
  .track .fill {
    position: absolute;
    top: 1px;
    bottom: 1px;
    border-radius: 2px;
    background: #fd7719;
  }
  .btn-live {
    display: flex;
    align-items: center;
    justify-content: center;
    background: transparent;
  }
  .mode-toggle {
    padding: 2px 5px;
    border: 1px solid rgba(253, 119, 25, 0.3);
    border-radius: 6px;
    background: transparent;
    color: #91a39c;
    font-size: 0.56rem;
    font-weight: 800;
    text-transform: uppercase;
    cursor: pointer;
  }
  .mode-toggle.tog {
    background: #fd7719;
    color: #10171a;
    border-color: transparent;
  }
  .mode-toggle:disabled {
    opacity: 0.35;
    cursor: default;
  }
  .led {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: #33454c;
  }
  .led.on {
    background: #35d07f;
    box-shadow: 0 0 6px rgba(53, 208, 127, 0.8);
  }
  .inspector {
    border-left: 1px solid rgba(253, 119, 25, 0.15);
    padding-left: 12px;
  }
  @media (max-width: 720px) {
    .inspector {
      border-left: none;
      padding-left: 0;
      border-top: 1px solid rgba(253, 119, 25, 0.15);
      padding-top: 8px;
    }
  }
  .insp-head {
    color: #697c75;
    font-size: 0.56rem;
    font-weight: 800;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin-bottom: 6px;
  }
  .insp-head em {
    color: #8fa09a;
    font-style: normal;
    font-weight: 600;
    text-transform: none;
  }
  .axrow {
    display: grid;
    grid-template-columns: 34px 1fr 34px;
    align-items: center;
    gap: 6px;
    margin-bottom: 3px;
  }
  .ai {
    color: #91a39c;
    font-size: 0.58rem;
    font-weight: 760;
  }
  .axrow em {
    color: #edf6f1;
    font-style: normal;
    font-size: 0.62rem;
    font-weight: 700;
    text-align: right;
  }
  .rc-map.light .axrow em {
    color: #12171b;
  }
  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 8px;
  }
  .pill {
    min-width: 18px;
    padding: 2px 5px;
    border-radius: 5px;
    background: #16242a;
    color: #8797a0;
    font-size: 0.58rem;
    font-weight: 760;
    text-align: center;
  }
  .pill.on {
    background: #35d07f;
    color: #06231a;
  }
</style>

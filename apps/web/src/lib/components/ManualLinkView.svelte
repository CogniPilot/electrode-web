<script lang="ts">
  import { onDestroy } from 'svelte';
  import { get } from 'svelte/store';
  import RcTransmitterView from './RcTransmitterView.svelte';
  import { isGroundStation } from '$lib/capabilities';
  import { fetchMapping, openJoystickSocket, type JoystickSnapshot } from '$lib/gcs';
  import { mappingProfile } from '$lib/mappingStore';
  import { advanceLatch, emptyLatch, mapJoystickToManual, type LatchState } from '$lib/manualMapping';
  import type { ManualControlState } from '@electrode/sdk';

  // Zenoh-derived manual control (decoded from the `manual` catalog topic).
  export let manual: ManualControlState | null = null;
  export let theme: 'light' | 'dark' = 'dark';
  export let hardwareAvailable = true;

  type Source = 'hardware' | 'zenoh';

  let source: Source = 'zenoh';
  let sourcePinned = false;
  // Default to the hardware path in Ground Station mode (it works locally,
  // without waiting on the Zenoh round trip); the user can pin either.
  $: if (!sourcePinned) source = $isGroundStation && hardwareAvailable ? 'hardware' : 'zenoh';

  // Hardware path state.
  let socket: WebSocket | null = null;
  let connecting = false;
  let connectedDevice: string | null = null;
  let hwManual: ManualControlState | null = null;
  let latch: LatchState = emptyLatch();
  let prevButtons: number[] = [];

  function pick(next: Source): void {
    source = next;
    sourcePinned = true;
  }

  // (Re)connect the raw-joystick feed as the hardware path is used, and whenever
  // the mapped device changes. Axis/invert/button edits need no reconnect — the
  // WS callback reads the latest mapping from the store on every frame.
  $: syncHardware(source, $isGroundStation, hardwareAvailable, $mappingProfile?.device ?? null);

  function syncHardware(src: Source, gs: boolean, available: boolean, device: string | null): void {
    if (src !== 'hardware' || !gs || !available) {
      disconnectHardware();
      return;
    }
    if (!get(mappingProfile)) {
      void ensureMapping();
      return;
    }
    if (socket && connectedDevice === device) return;
    connectHardware(device);
  }

  async function ensureMapping(): Promise<void> {
    if (get(mappingProfile)) return;
    try {
      mappingProfile.set(await fetchMapping());
    } catch {
      // no backend / not ground station
    }
  }

  function connectHardware(device: string | null): void {
    if (connecting || !device) return;
    connecting = true;
    socket?.close();
    latch = emptyLatch();
    prevButtons = [];
    const ws = openJoystickSocket(device);
    socket = ws;
    connectedDevice = device;
    ws.onmessage = (event) => {
      try {
        const map = get(mappingProfile); // always the latest mapping
        if (!map) return;
        const snap = JSON.parse(event.data) as JoystickSnapshot;
        latch = advanceLatch(snap, map, latch, prevButtons);
        prevButtons = snap.buttons;
        hwManual = mapJoystickToManual(snap, map, latch, Date.now());
      } catch {
        // ignore malformed frames
      }
    };
    ws.onclose = () => {
      if (socket === ws) socket = null;
    };
    connecting = false;
  }

  function disconnectHardware(): void {
    socket?.close();
    socket = null;
    connectedDevice = null;
    hwManual = null;
  }

  $: shown = source === 'hardware' ? hwManual : manual;

  onDestroy(disconnectHardware);
</script>

<div class="manual-link">
  {#if $isGroundStation}
    <div class="src-toggle" role="group" aria-label="Manual control source">
      <button type="button" class:active={source === 'hardware'} disabled={!hardwareAvailable} onclick={() => pick('hardware')}>
        Hardware
      </button>
      <button type="button" class:active={source === 'zenoh'} onclick={() => pick('zenoh')}>
        Zenoh
      </button>
    </div>
    <p class="src-note">
      {source === 'hardware'
        ? 'Controller → mapping, computed locally (not via Zenoh)'
        : !hardwareAvailable
          ? 'keyboard fallback → manual over Zenoh'
        : 'manual received over Zenoh'}
    </p>
  {/if}

  <RcTransmitterView manual={shown} {theme} />
</div>

<style>
  .manual-link {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .src-toggle {
    display: inline-flex;
    align-self: flex-start;
    border: 1px solid rgba(253, 119, 25, 0.28);
    border-radius: 7px;
    overflow: hidden;
  }
  .src-toggle button {
    padding: 3px 11px;
    border: none;
    background: transparent;
    color: #91a39c;
    font-size: 0.64rem;
    font-weight: 800;
    cursor: pointer;
  }
  .src-toggle button:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .src-toggle button + button {
    border-left: 1px solid rgba(253, 119, 25, 0.2);
  }
  .src-toggle button.active {
    background: #fd7719;
    color: #10171a;
  }
  :global(html[data-theme='light']) .src-toggle button.active {
    background: #e35f0c;
    color: #fff;
  }
  .src-note {
    margin: 0;
    color: #697c75;
    font-size: 0.56rem;
    font-weight: 600;
  }
</style>

<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { fetchDevices, type DetectedDevice } from '$lib/gcs';
  import { groundStation } from '$lib/capabilities';

  export let theme: 'light' | 'dark' = 'dark';

  const POLL_MS = 2000;

  let joysticks: DetectedDevice[] = [];
  let serial: DetectedDevice[] = [];
  let error = '';
  let loading = true;
  let timer: ReturnType<typeof setInterval> | null = null;
  let inFlight: AbortController | null = null;

  async function refresh(): Promise<void> {
    inFlight?.abort();
    const controller = new AbortController();
    inFlight = controller;
    try {
      const devices = await fetchDevices(controller.signal);
      joysticks = devices.joysticks;
      serial = devices.serial;
      error = '';
    } catch (err) {
      if (!controller.signal.aborted) {
        error = err instanceof Error ? err.message : 'device query failed';
      }
    } finally {
      if (!controller.signal.aborted) {
        loading = false;
      }
    }
  }

  onMount(() => {
    void refresh();
    timer = setInterval(() => void refresh(), POLL_MS);
  });

  onDestroy(() => {
    if (timer) {
      clearInterval(timer);
    }
    inFlight?.abort();
  });
</script>

<section class="gcs-bar" class:light={theme === 'light'}>
  <div class="lead">
    <span class="badge">GROUND STATION</span>
    <span class="host">{$groundStation?.host ?? 'localhost'}</span>
  </div>

  <div class="groups">
    <div class="group">
      <span class="label">RC input</span>
      {#if joysticks.length === 0}
        <span class="chip empty">no joystick</span>
      {:else}
        {#each joysticks as device}
          <span class="chip ok" title={device.path}>
            <span class="dot"></span>{device.name}
            <em>{device.path}</em>
          </span>
        {/each}
      {/if}
    </div>

    <div class="group">
      <span class="label">Serial / PPM</span>
      {#if serial.length === 0}
        <span class="chip empty">no serial device</span>
      {:else}
        {#each serial as device}
          <span class="chip ok" title={device.path}>
            <span class="dot"></span>{device.name}
            <em>{device.path}</em>
          </span>
        {/each}
      {/if}
    </div>
  </div>

  <div class="status">
    {#if error}
      <span class="msg err">{error}</span>
    {:else if loading}
      <span class="msg">scanning…</span>
    {:else}
      <span class="msg live">connected</span>
    {/if}
  </div>
</section>

<style>
  .gcs-bar {
    display: flex;
    align-items: center;
    gap: 16px;
    flex-wrap: wrap;
    margin-top: 10px;
    padding: 8px 12px;
    border: 1px solid rgba(253, 119, 25, 0.28);
    border-radius: 10px;
    background: linear-gradient(90deg, rgba(253, 119, 25, 0.1), rgba(5, 8, 8, 0.5) 40%);
  }

  .gcs-bar.light {
    border-color: rgba(227, 95, 12, 0.3);
    background: linear-gradient(90deg, rgba(227, 95, 12, 0.12), rgba(255, 255, 255, 0.85) 40%);
  }

  .lead {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .badge {
    padding: 3px 8px;
    border-radius: 6px;
    background: #fd7719;
    color: #10171a;
    font-size: 0.6rem;
    font-weight: 800;
    letter-spacing: 0.08em;
  }

  .gcs-bar.light .badge {
    background: #e35f0c;
    color: #fff;
  }

  .host {
    color: #91a39c;
    font-size: 0.66rem;
    font-weight: 700;
  }

  .groups {
    display: flex;
    align-items: center;
    gap: 18px;
    flex-wrap: wrap;
    flex: 1;
    min-width: 0;
  }

  .group {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .label {
    color: #697c75;
    font-size: 0.58rem;
    font-weight: 800;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 9px;
    border: 1px solid rgba(253, 119, 25, 0.2);
    border-radius: 7px;
    background: rgba(5, 8, 8, 0.45);
    color: #edf6f1;
    font-size: 0.68rem;
    font-weight: 700;
  }

  .gcs-bar.light .chip {
    border-color: rgba(227, 95, 12, 0.24);
    background: rgba(255, 255, 255, 0.7);
    color: #12171b;
  }

  .chip em {
    color: #8fa09a;
    font-style: normal;
    font-weight: 600;
    font-size: 0.6rem;
  }

  .chip .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #35d07f;
    box-shadow: 0 0 6px rgba(53, 208, 127, 0.75);
  }

  .chip.empty {
    color: #8797a0;
    border-style: dashed;
  }

  .status .msg {
    font-size: 0.64rem;
    font-weight: 700;
    color: #7ee0ac;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .status .msg.err {
    color: #ff8f6f;
  }

  .status .msg:not(.live):not(.err) {
    color: #91a39c;
  }
</style>

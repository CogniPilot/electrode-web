<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import {
    fetchMapping,
    saveMapping,
    fetchPpmBridgeStatus,
    type BridgeStatus,
    type MappingProfile
  } from '$lib/gcs';
  import { mappingProfile } from '$lib/mappingStore';

  export let theme: 'light' | 'dark' = 'dark';
  export let channels: number[] | null = null;

  const CHANNELS = [
    { index: 0, label: 'Throttle', short: 'T' },
    { index: 1, label: 'Aileron / Roll', short: 'A' },
    { index: 2, label: 'Elevator / Pitch', short: 'E' },
    { index: 3, label: 'Rudder / Yaw', short: 'R' },
    { index: 4, label: 'Stabilization', short: 'S' }
  ] as const;

  let profile: MappingProfile | null = null;
  let bridge: BridgeStatus | null = null;
  let status = '';
  let saveTimer: ReturnType<typeof setTimeout> | null = null;
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  $: ppmRunning = bridge?.ppmRunning ?? false;
  $: serialLabel = '/dev/ttyACM0 · 57600';
  $: wireChannels = ppmRunning && channels && channels.length >= 5 ? channels : [1000, 1000, 1000, 1000, 1000];
  $: liveSource = ppmRunning && channels && channels.length >= 5;

  function normalized(profile: MappingProfile): MappingProfile {
    return {
      ...profile,
      ppmChannelMap: profile.ppmChannelMap?.length === 5 ? profile.ppmChannelMap : [1, 2, 0, 3, 4],
      ppmChannelInvert:
        profile.ppmChannelInvert?.length === 5
          ? profile.ppmChannelInvert
          : [false, false, false, false, false],
      ppmForceIdleThrottle: profile.ppmForceIdleThrottle ?? false,
      ppmForceStabilizingMode: profile.ppmForceStabilizingMode ?? false
    };
  }

  function clamp(value: number, min: number, max: number): number {
    return Math.min(max, Math.max(min, value));
  }

  function barWidth(pwm: number): number {
    return clamp((pwm - 1000) / 10, 0, 100);
  }

  function setOutputSource(output: number, source: number): void {
    if (!profile) return;
    const ppmChannelMap = [...profile.ppmChannelMap];
    ppmChannelMap[output] = source;
    profile = { ...profile, ppmChannelMap };
    scheduleSave();
  }

  function setOutputInvert(output: number, invert: boolean): void {
    if (!profile) return;
    const ppmChannelInvert = [...profile.ppmChannelInvert];
    ppmChannelInvert[output] = invert;
    profile = { ...profile, ppmChannelInvert };
    scheduleSave();
  }

  function setField<K extends keyof MappingProfile>(field: K, value: MappingProfile[K]): void {
    if (!profile) return;
    profile = { ...profile, [field]: value };
    scheduleSave();
  }

  function scheduleSave(): void {
    status = 'saving...';
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => void doSave(), 400);
  }

  async function doSave(): Promise<void> {
    if (!profile) return;
    try {
      profile = normalized(await saveMapping(profile));
      mappingProfile.set(profile);
      status = ppmRunning ? 'saved · hardware restarted' : 'saved';
      bridge = await fetchPpmBridgeStatus();
    } catch (err) {
      status = err instanceof Error ? err.message : 'save failed';
    }
  }

  async function refresh(): Promise<void> {
    try {
      bridge = await fetchPpmBridgeStatus();
    } catch (err) {
      status = err instanceof Error ? err.message : 'ppm status unavailable';
    }
  }

  onMount(async () => {
    try {
      profile = normalized(await fetchMapping());
      mappingProfile.set(profile);
      await refresh();
      pollTimer = setInterval(() => void refresh(), 2000);
    } catch (err) {
      status = err instanceof Error ? err.message : 'failed to load ppm mapping';
    }
  });

  onDestroy(() => {
    if (saveTimer) clearTimeout(saveTimer);
    if (pollTimer) clearInterval(pollTimer);
  });
</script>

<section class="ppm-map" class:light={theme === 'light'}>
  <div class="head">
    <div class="title">
      <h3>PPM Hardware</h3>
      <span class="dev">{serialLabel}</span>
      <span class="dev">{liveSource ? 'ppm wire live' : 'ppm wire low'}</span>
      {#if status}<span class="status">{status}</span>{/if}
    </div>
  </div>

  {#if !profile}
    <div class="status block">loading ppm mapping...</div>
  {:else}
    <div class="toggles">
      <label>
        <input
          type="checkbox"
          checked={profile.ppmForceIdleThrottle}
          onchange={(e) => setField('ppmForceIdleThrottle', e.currentTarget.checked)}
        />
        <span>Force throttle idle</span>
      </label>
      <label>
        <input
          type="checkbox"
          checked={profile.ppmForceStabilizingMode}
          onchange={(e) => setField('ppmForceStabilizingMode', e.currentTarget.checked)}
        />
        <span>Force stabilize</span>
      </label>
    </div>

    <div class="col-head">
      <span>Serial</span>
      <span>Source</span>
      <span>Live</span>
      <span>Inv</span>
      <span>Wire</span>
    </div>
    {#each profile.ppmChannelMap as source, output}
      {@const channel = CHANNELS.find((item) => item.index === source) ?? CHANNELS[0]}
      {@const pwm = wireChannels[output] ?? 1000}
      {@const forced =
        (source === 0 && profile.ppmForceIdleThrottle) ||
        (source === 4 && profile.ppmForceStabilizingMode)}
      <div class="row">
        <span class="fn">Ch {output + 1}</span>
        <select value={source} onchange={(e) => setOutputSource(output, Number(e.currentTarget.value))}>
          {#each CHANNELS as option}
            <option value={option.index}>{option.label}</option>
          {/each}
        </select>
        <div class="live-bar" title={`${pwm} us`}>
          <div class="fill" style={`width:${barWidth(pwm)}%;`}></div>
          <span>{pwm}</span>
        </div>
        <input
          type="checkbox"
          checked={profile.ppmChannelInvert[output]}
          disabled={forced}
          title={forced ? 'Forced safety channel is not inverted' : 'Invert this serial output'}
          onchange={(e) => setOutputInvert(output, e.currentTarget.checked)}
        />
        <span class="wire" class:forced>{channel.short}{profile.ppmChannelInvert[output] && !forced ? '-' : '+'}</span>
      </div>
    {/each}
  {/if}
</section>

<style>
  .ppm-map {
    margin-top: 8px;
    padding: 10px 12px;
    border: 1px solid rgba(53, 208, 127, 0.28);
    border-radius: 8px;
    background: rgba(5, 8, 8, 0.4);
    overflow: hidden;
  }
  .ppm-map.light {
    border-color: rgba(27, 145, 91, 0.28);
    background: rgba(255, 255, 255, 0.75);
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
    flex-wrap: wrap;
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
  .status {
    color: #7ee0ac;
    font-size: 0.6rem;
    font-weight: 700;
  }
  .status.block {
    display: block;
    margin-top: 6px;
  }
  .toggles {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
    margin: 10px 0 8px;
  }
  .toggles label {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: #cfe0d8;
    font-size: 0.68rem;
    font-weight: 760;
  }
  .ppm-map.light .toggles label {
    color: #12171b;
  }
  .col-head,
  .row {
    display: grid;
    grid-template-columns: 48px minmax(120px, 1fr) minmax(74px, 0.8fr) 34px 46px;
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
  .row select {
    min-width: 0;
    width: 100%;
    box-sizing: border-box;
    background: #16242a;
    color: #edf6f1;
    border: 1px solid rgba(53, 208, 127, 0.22);
    border-radius: 6px;
    font-size: 0.68rem;
  }
  .live-bar {
    position: relative;
    min-width: 0;
    height: 20px;
    border-radius: 5px;
    background: #16242a;
    overflow: hidden;
  }
  .live-bar .fill {
    position: absolute;
    inset: 0 auto 0 0;
    background: #35d07f;
  }
  .live-bar span {
    position: relative;
    z-index: 1;
    display: block;
    padding: 3px 5px;
    color: #edf6f1;
    font-size: 0.58rem;
    font-weight: 850;
    text-align: right;
  }
  .ppm-map.light .live-bar {
    background: #dbe2e7;
  }
  .ppm-map.light .live-bar span {
    color: #12171b;
  }
  .ppm-map.light .row select {
    background: #eef2f4;
    color: #12171b;
  }
  .wire {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-height: 20px;
    border-radius: 5px;
    background: #16242a;
    color: #7ee0ac;
    font-size: 0.62rem;
    font-weight: 850;
  }
  .wire.forced {
    color: #10171a;
    background: #35d07f;
  }
  .ppm-map.light .wire {
    background: #dbe2e7;
    color: #167a4a;
  }
  .ppm-map.light .wire.forced {
    color: #10171a;
    background: #35d07f;
  }
  @media (max-width: 520px) {
    .col-head,
    .row {
      grid-template-columns: 42px minmax(92px, 1fr) minmax(64px, 0.8fr) 30px 40px;
      gap: 6px;
    }
  }
</style>

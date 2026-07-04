<script lang="ts">
  import type { ManualControlState } from '@electrode/sdk';

  export let manual: ManualControlState | null = null;
  export let theme: 'light' | 'dark' = 'dark';

  const FLIGHT_MODES = ['—', 'hold', 'manual', 'mission', 'return', 'land'];
  // Stick throw as a fraction of half the gimbal pad, leaving a margin so the
  // dot never clips the border.
  const THROW = 40;

  function clamp(value: number, min: number, max: number): number {
    return Math.min(max, Math.max(min, value));
  }

  $: roll = clamp(manual?.roll ?? 0, -1, 1);
  $: pitch = clamp(manual?.pitch ?? 0, -1, 1);
  $: yaw = clamp(manual?.yaw ?? 0, -1, 1);
  $: throttle = clamp(manual?.throttle ?? 0, 0, 1);

  // Mode 2 layout: left stick = throttle (vertical, self-parks at bottom) + yaw
  // (horizontal); right stick = pitch (vertical) + roll (horizontal).
  $: leftX = 50 + yaw * THROW;
  $: leftY = 90 - throttle * 80;
  $: rightX = 50 + roll * THROW;
  $: rightY = 50 - pitch * THROW;

  $: hasSignal = manual !== null;
  $: armed = manual?.armSwitch ?? false;
  $: killed = manual?.killSwitch ?? false;
  $: active = manual?.active ?? false;
  $: valid = manual?.valid ?? false;
  $: modeLabel = FLIGHT_MODES[manual?.flightMode ?? 0] ?? `mode ${manual?.flightMode}`;
  $: linkLabel = !hasSignal ? 'no signal' : !valid ? 'invalid' : 'live';

  // Reactive so Svelte re-renders the bars when the axes change; a static array
  // of getter closures would not be tracked as a dependency.
  $: bars = [
    { label: 'Roll', value: roll, bipolar: true },
    { label: 'Pitch', value: pitch, bipolar: true },
    { label: 'Yaw', value: yaw, bipolar: true },
    { label: 'Thr', value: throttle, bipolar: false }
  ];

  // Switch channels shown as high/low bars: full when on, empty when off. The
  // on/off value already reflects toggle vs momentary (computed upstream).
  $: switchBars = [
    { label: 'Arm', on: armed, danger: false },
    { label: 'Kill', on: killed, danger: true },
    { label: 'Mode', on: (manual?.flightMode ?? 0) > 0, danger: false },
    { label: 'Stabilization', on: active, danger: false }
  ];
</script>

<div class="rc-view" class:light={theme === 'light'}>
  <div class="rc-shell">
    <div class="antennas" aria-hidden="true">
      <span></span>
      <span></span>
    </div>

    <div class="gimbals">
      <div class="gimbal">
        <svg viewBox="0 0 100 100" role="img" aria-label="Left stick: throttle and yaw">
          <rect class="pad" x="4" y="4" width="92" height="92" rx="10" />
          <line class="grid" x1="50" y1="8" x2="50" y2="92" />
          <line class="grid" x1="8" y1="50" x2="92" y2="50" />
          <line class="stem" x1="50" y1="90" x2={leftX} y2={leftY} />
          <circle class="knob" cx={leftX} cy={leftY} r="9" class:dead={!hasSignal} />
        </svg>
        <span class="axis-label">THR · YAW</span>
      </div>

      <div class="gimbal">
        <svg viewBox="0 0 100 100" role="img" aria-label="Right stick: pitch and roll">
          <rect class="pad" x="4" y="4" width="92" height="92" rx="10" />
          <line class="grid" x1="50" y1="8" x2="50" y2="92" />
          <line class="grid" x1="8" y1="50" x2="92" y2="50" />
          <line class="stem" x1="50" y1="50" x2={rightX} y2={rightY} />
          <circle class="knob" cx={rightX} cy={rightY} r="9" class:dead={!hasSignal} />
        </svg>
        <span class="axis-label">PITCH · ROLL</span>
      </div>
    </div>

    <div class="switch-row">
      <div class="led" class:on={armed}>
        <span class="dot"></span>ARM
      </div>
      <div class="led kill" class:on={killed}>
        <span class="dot"></span>KILL
      </div>
      <div class="mode-chip">{modeLabel}</div>
      <div class="link" class:warn={hasSignal && !valid} class:off={!hasSignal}>
        {linkLabel}
      </div>
    </div>
  </div>

  <div class="readout">
    {#each bars as bar}
      <div class="bar">
        <span>{bar.label}</span>
        <div class="track" class:bipolar={bar.bipolar}>
          {#if bar.bipolar}
            <div class="center-tick"></div>
            <div
              class="fill"
              style={`left:${50 + (bar.value < 0 ? bar.value * 50 : 0)}%; width:${Math.abs(bar.value) * 50}%;`}
            ></div>
          {:else}
            <div class="fill" style={`left:0%; width:${bar.value * 100}%;`}></div>
          {/if}
        </div>
        <em>{bar.value.toFixed(2)}</em>
      </div>
    {/each}

    {#each switchBars as sw}
      <div class="bar">
        <span>{sw.label}</span>
        <div class="track">
          <div
            class="fill switch"
            class:on={sw.on}
            class:danger={sw.danger}
            style={`left:0%; width:${sw.on ? 100 : 0}%;`}
          ></div>
        </div>
        <em class:lo={!sw.on}>{sw.on ? 'HI' : 'LO'}</em>
      </div>
    {/each}
  </div>

  {#if !hasSignal}
    <div class="empty">Waiting for <code>synapse/v1/topic/manual_control_command</code></div>
  {/if}
</div>

<style>
  .rc-view {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .rc-shell {
    position: relative;
    padding: 14px 12px 12px;
    border: 1px solid #2a383f;
    border-radius: 14px;
    background: linear-gradient(180deg, #0c1417 0%, #070d0f 100%);
  }

  .rc-view.light .rc-shell {
    border-color: #cdd5dc;
    background: linear-gradient(180deg, #eef2f4 0%, #e2e8ec 100%);
  }

  .antennas {
    position: absolute;
    top: 3px;
    left: 0;
    right: 0;
    display: flex;
    justify-content: space-between;
    padding: 0 26px;
    pointer-events: none;
  }

  .antennas span {
    width: 3px;
    height: 9px;
    border-radius: 2px;
    background: linear-gradient(180deg, #fd7719, rgba(253, 119, 25, 0.15));
  }

  .rc-view.light .antennas span {
    background: linear-gradient(180deg, #e35f0c, rgba(227, 95, 12, 0.15));
  }

  .gimbals {
    display: grid;
    grid-template-columns: 1fr 1fr;
    align-items: center;
    gap: 14px;
  }

  .gimbal {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 5px;
  }

  .gimbal svg {
    width: 100%;
    max-width: 116px;
    height: auto;
  }

  .pad {
    fill: #16242a;
    stroke: rgba(253, 119, 25, 0.28);
    stroke-width: 1;
  }

  .rc-view.light .pad {
    fill: #dbe2e7;
    stroke: rgba(227, 95, 12, 0.32);
  }

  .grid {
    stroke: #3a4c52;
    stroke-width: 1;
    stroke-dasharray: 3 4;
  }

  .rc-view.light .grid {
    stroke: #b7c1c9;
  }

  .stem {
    stroke: rgba(253, 119, 25, 0.55);
    stroke-width: 3;
    stroke-linecap: round;
  }

  .rc-view.light .stem {
    stroke: rgba(227, 95, 12, 0.6);
  }

  .knob {
    fill: #fd7719;
    stroke: #10171a;
    stroke-width: 2;
    transition: cx 0.05s linear, cy 0.05s linear;
  }

  .rc-view.light .knob {
    stroke: #fff;
  }

  .knob.dead {
    fill: #46555b;
  }

  .axis-label {
    color: #91a39c;
    font-size: 0.56rem;
    font-weight: 800;
    letter-spacing: 0.08em;
  }

  .rc-view.light .axis-label {
    color: #5c6873;
  }

  .switch-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: center;
    gap: 6px;
    margin-top: 10px;
  }

  .led {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 8px;
    border: 1px solid rgba(253, 119, 25, 0.2);
    border-radius: 7px;
    background: rgba(5, 8, 8, 0.5);
    color: #91a39c;
    font-size: 0.58rem;
    font-weight: 800;
    letter-spacing: 0.06em;
  }

  .rc-view.light .led {
    border-color: rgba(227, 95, 12, 0.26);
    background: rgba(255, 255, 255, 0.7);
    color: #5c6873;
  }

  .led .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #33454c;
  }

  .led.on {
    color: #edf6f1;
  }

  .led.on .dot {
    background: #35d07f;
    box-shadow: 0 0 6px rgba(53, 208, 127, 0.8);
  }

  .led.kill.on {
    color: #ffd7cd;
    border-color: rgba(255, 111, 130, 0.5);
  }

  .led.kill.on .dot {
    background: #ff6f82;
    box-shadow: 0 0 6px rgba(255, 111, 130, 0.85);
  }

  .rc-view.light .led.on {
    color: #12171b;
  }

  .mode-chip {
    padding: 4px 8px;
    border-radius: 7px;
    background: #fd7719;
    color: #10171a;
    font-size: 0.6rem;
    font-weight: 800;
    text-align: center;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .rc-view.light .mode-chip {
    background: #e35f0c;
    color: #fff;
  }

  .link {
    padding: 3px 8px;
    border-radius: 7px;
    background: rgba(53, 208, 127, 0.16);
    color: #7ee0ac;
    font-size: 0.56rem;
    font-weight: 800;
    text-align: center;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .link.warn {
    background: rgba(255, 176, 90, 0.18);
    color: #ffbf7a;
  }

  .link.off {
    background: rgba(90, 105, 112, 0.22);
    color: #8797a0;
  }

  .readout {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 5px 16px;
    padding: 9px 11px;
    border: 1px solid rgba(253, 119, 25, 0.2);
    border-radius: 8px;
    background: rgba(5, 8, 8, 0.5);
  }

  .rc-view.light .readout {
    border-color: rgba(227, 95, 12, 0.28);
    background: rgba(255, 255, 255, 0.85);
  }

  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .bar span {
    width: 32px;
    color: #91a39c;
    font-size: 0.6rem;
    font-weight: 760;
    text-transform: uppercase;
  }

  .bar .track {
    position: relative;
    flex: 1;
    height: 8px;
    border-radius: 3px;
    background: #16242a;
    overflow: hidden;
  }

  .rc-view.light .bar .track {
    background: #dbe2e7;
  }

  .bar .center-tick {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 50%;
    width: 1px;
    background: #3a4c52;
  }

  .bar .fill {
    position: absolute;
    top: 1px;
    bottom: 1px;
    border-radius: 2px;
    background: #fd7719;
  }

  .rc-view.light .bar .fill {
    background: #e35f0c;
  }

  /* Switch channels: full green when high, empty when low; red for kill. */
  .bar .fill.switch {
    background: #33454c;
    transition: width 0.08s linear;
  }
  .bar .fill.switch.on {
    background: #35d07f;
    box-shadow: 0 0 6px rgba(53, 208, 127, 0.5);
  }
  .bar .fill.switch.danger.on {
    background: #ff6f82;
    box-shadow: 0 0 6px rgba(255, 111, 130, 0.5);
  }

  .bar em {
    width: 38px;
    color: #edf6f1;
    font-size: 0.68rem;
    font-style: normal;
    font-weight: 700;
    text-align: right;
  }

  .bar em.lo {
    color: #697c75;
  }

  .rc-view.light .bar em {
    color: #12171b;
  }

  .rc-view.light .bar span {
    color: #5c6873;
  }

  .empty {
    padding: 6px 10px;
    border-radius: 8px;
    background: rgba(5, 8, 8, 0.5);
    color: #91a39c;
    font-size: 0.68rem;
    text-align: center;
  }

  .empty code {
    color: #fd7719;
  }

  .rc-view.light .empty {
    background: rgba(255, 255, 255, 0.8);
    color: #5c6873;
  }
</style>

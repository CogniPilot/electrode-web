<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import {
    fetchAutopilotProfile,
    fetchDevices,
    fetchFirmwareInstallStatus,
    planFirmwareInstall,
    saveAutopilotProfile,
    type AutopilotProfile,
    type DetectedDevice,
    type FirmwareInstallStatus,
    type FirmwareSource,
    type FlashMethod,
    type RuntimeProtocol,
    type RuntimeTransport
  } from '$lib/gcs';

  export let theme: 'light' | 'dark' = 'dark';

  const firmwareSources: FirmwareSource[] = ['localBuild', 'releaseArtifact', 'ciArtifact', 'customFile'];
  const flashMethods: FlashMethod[] = ['usbBootloader', 'dfu', 'serialBootloader', 'sdCard', 'externalTool'];
  const runtimeTransports: RuntimeTransport[] = ['zenoh', 'mavlinkSerial', 'mavlinkUdp', 'mavlinkTcp'];
  const runtimeProtocols: RuntimeProtocol[] = ['synapseZenoh', 'mavlink'];

  let profile: AutopilotProfile | null = null;
  let serial: DetectedDevice[] = [];
  let selectedDevice = '';
  let confirmed = false;
  let status = '';
  let install: FirmwareInstallStatus | null = null;
  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  function label(value: string): string {
    return value.replace(/([A-Z])/g, ' $1').replace(/^./, (char) => char.toUpperCase());
  }

  function setField<K extends keyof AutopilotProfile>(field: K, value: AutopilotProfile[K]): void {
    if (!profile) return;
    profile = { ...profile, [field]: value };
    scheduleSave();
  }

  function scheduleSave(): void {
    status = 'saving...';
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => void doSave(), 450);
  }

  async function doSave(): Promise<void> {
    if (!profile) return;
    try {
      profile = await saveAutopilotProfile(profile);
      status = 'saved';
    } catch (err) {
      status = err instanceof Error ? err.message : 'save failed';
    }
  }

  async function requestInstallPlan(): Promise<void> {
    try {
      if (saveTimer) {
        clearTimeout(saveTimer);
        saveTimer = null;
        await doSave();
      }
      install = await planFirmwareInstall(selectedDevice, confirmed);
      status = install.status === 'planned' ? 'install plan ready' : install.message;
    } catch (err) {
      status = err instanceof Error ? err.message : 'install request failed';
    }
  }

  onMount(async () => {
    try {
      const [loadedProfile, devices, lastInstall] = await Promise.all([
        fetchAutopilotProfile(),
        fetchDevices(),
        fetchFirmwareInstallStatus()
      ]);
      profile = loadedProfile;
      serial = devices.serial;
      selectedDevice = devices.serial[0]?.path ?? '';
      install = lastInstall;
      status = 'ready';
    } catch (err) {
      status = err instanceof Error ? err.message : 'failed to load autopilot profile';
    }
  });

  onDestroy(() => {
    if (saveTimer) clearTimeout(saveTimer);
  });
</script>

<section class="firmware-page" class:light={theme === 'light'}>
  <div class="page-head">
    <div>
      <h2>Firmware</h2>
      <span>{profile?.stackName ?? 'Cerebri'} firmware source, hardware target, and runtime protocol</span>
    </div>
    {#if status}<strong>{status}</strong>{/if}
  </div>

  {#if !profile}
    <div class="empty">loading profile...</div>
  {:else}
    <div class="grid">
      <label>
        <span>Stack</span>
        <input value={profile.stackName} oninput={(e) => setField('stackName', e.currentTarget.value)} />
      </label>

      <label>
        <span>Stack path</span>
        <input value={profile.stackPath} spellcheck="false" oninput={(e) => setField('stackPath', e.currentTarget.value)} />
      </label>

      <label>
        <span>Firmware source</span>
        <select value={profile.firmwareSource} onchange={(e) => setField('firmwareSource', e.currentTarget.value as FirmwareSource)}>
          {#each firmwareSources as source}
            <option value={source}>{label(source)}</option>
          {/each}
        </select>
      </label>

      <label>
        <span>Firmware artifact</span>
        <input
          value={profile.firmwareArtifact}
          spellcheck="false"
          oninput={(e) => setField('firmwareArtifact', e.currentTarget.value)}
        />
      </label>

      <label>
        <span>Board target</span>
        <input value={profile.boardTarget} spellcheck="false" oninput={(e) => setField('boardTarget', e.currentTarget.value)} />
      </label>

      <label>
        <span>Flash method</span>
        <select value={profile.flashMethod} onchange={(e) => setField('flashMethod', e.currentTarget.value as FlashMethod)}>
          {#each flashMethods as method}
            <option value={method}>{label(method)}</option>
          {/each}
        </select>
      </label>

      <label>
        <span>Runtime transport</span>
        <select value={profile.runtimeTransport} onchange={(e) => setField('runtimeTransport', e.currentTarget.value as RuntimeTransport)}>
          {#each runtimeTransports as transport}
            <option value={transport}>{label(transport)}</option>
          {/each}
        </select>
      </label>

      <label>
        <span>Runtime endpoint</span>
        <input
          value={profile.runtimeEndpoint}
          spellcheck="false"
          oninput={(e) => setField('runtimeEndpoint', e.currentTarget.value)}
        />
      </label>
    </div>

    <div class="protocols">
      <label>
        <span>Missions</span>
        <select value={profile.missionProtocol} onchange={(e) => setField('missionProtocol', e.currentTarget.value as RuntimeProtocol)}>
          {#each runtimeProtocols as protocol}
            <option value={protocol}>{label(protocol)}</option>
          {/each}
        </select>
      </label>
      <label>
        <span>Parameters</span>
        <select value={profile.parameterProtocol} onchange={(e) => setField('parameterProtocol', e.currentTarget.value as RuntimeProtocol)}>
          {#each runtimeProtocols as protocol}
            <option value={protocol}>{label(protocol)}</option>
          {/each}
        </select>
      </label>
      <label>
        <span>Calibration</span>
        <select value={profile.calibrationProtocol} onchange={(e) => setField('calibrationProtocol', e.currentTarget.value as RuntimeProtocol)}>
          {#each runtimeProtocols as protocol}
            <option value={protocol}>{label(protocol)}</option>
          {/each}
        </select>
      </label>
    </div>

    <div class="install">
      <label>
        <span>Hardware</span>
        <select bind:value={selectedDevice}>
          <option value="">Select plugged-in hardware</option>
          {#each serial as device}
            <option value={device.path}>{device.name} - {device.path}</option>
          {/each}
        </select>
      </label>

      <label class="confirm">
        <input type="checkbox" bind:checked={confirmed} />
        <span>Confirm firmware install planning for selected hardware</span>
      </label>

      <button type="button" class="primary" onclick={requestInstallPlan}>Plan Flash</button>
    </div>

    {#if install}
      <div class="plan" class:rejected={install.status === 'rejected'}>
        <div class="plan-head">
          <strong>{label(install.status)} · {install.jobId}</strong>
          <span>{install.message}</span>
        </div>
        <ol>
          {#each install.steps as step}
            <li>{step}</li>
          {/each}
        </ol>
      </div>
    {/if}
  {/if}
</section>

<style>
  .firmware-page {
    margin-top: 10px;
    padding: 14px;
    border: 1px solid rgba(253, 119, 25, 0.28);
    border-radius: 8px;
    background: rgba(5, 8, 8, 0.72);
    color: #edf6f1;
  }

  .firmware-page.light {
    background: rgba(255, 255, 255, 0.9);
    color: #12171b;
    border-color: rgba(227, 95, 12, 0.26);
  }

  .page-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 12px;
  }

  h2 {
    margin: 0;
    font-size: 1rem;
    letter-spacing: 0;
  }

  .page-head span,
  label span,
  .empty,
  .plan span {
    color: #91a39c;
    font-size: 0.66rem;
    font-weight: 700;
  }

  .page-head strong {
    color: #7ee0ac;
    font-size: 0.64rem;
    text-transform: uppercase;
  }

  .grid,
  .protocols,
  .install {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 8px;
  }

  .protocols,
  .install {
    margin-top: 8px;
  }

  label {
    display: grid;
    gap: 5px;
    min-width: 0;
  }

  input,
  select {
    min-width: 0;
    width: 100%;
    height: 34px;
    border: 1px solid rgba(145, 163, 156, 0.28);
    border-radius: 7px;
    background: rgba(0, 0, 0, 0.26);
    color: inherit;
    padding: 0 9px;
    font: inherit;
    font-size: 0.72rem;
  }

  .light input,
  .light select {
    background: #fff;
  }

  .confirm {
    grid-template-columns: auto 1fr;
    align-items: center;
    gap: 8px;
  }

  .confirm input {
    width: 16px;
    height: 16px;
    padding: 0;
  }

  .primary {
    min-height: 34px;
    align-self: end;
    border: 0;
    border-radius: 7px;
    background: #fd7719;
    color: #10171a;
    padding: 0 14px;
    font-weight: 820;
    cursor: pointer;
  }

  .plan {
    margin-top: 10px;
    padding: 10px;
    border: 1px solid rgba(126, 224, 172, 0.24);
    border-radius: 8px;
    background: rgba(126, 224, 172, 0.08);
  }

  .plan.rejected {
    border-color: rgba(255, 143, 111, 0.34);
    background: rgba(255, 143, 111, 0.08);
  }

  .plan-head {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
  }

  .plan strong {
    font-size: 0.72rem;
  }

  ol {
    margin: 8px 0 0;
    padding-left: 18px;
  }

  li {
    margin: 3px 0;
    color: #cddbd5;
    font-size: 0.7rem;
  }

  .light li {
    color: #3a4742;
  }

  @media (max-width: 980px) {
    .grid,
    .protocols,
    .install {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  @media (max-width: 620px) {
    .grid,
    .protocols,
    .install {
      grid-template-columns: 1fr;
    }
  }
</style>

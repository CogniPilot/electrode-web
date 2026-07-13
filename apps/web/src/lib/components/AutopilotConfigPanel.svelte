<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { fetchAutopilotProfile, saveAutopilotProfile, type AutopilotProfile, type MocapSource } from '$lib/gcs';

  export let theme: 'light' | 'dark' = 'dark';
  export let embedded = false;

  let profile: AutopilotProfile | null = null;
  let status = '';
  let saveTimer: ReturnType<typeof setTimeout> | null = null;
  let dirty = false;

  $: inboundTopicsText = profile?.inboundTopics.join('\n') ?? '';

  function setField<K extends keyof AutopilotProfile>(field: K, value: AutopilotProfile[K]): void {
    if (!profile) return;
    profile = { ...profile, [field]: value };
    dirty = true;
    scheduleSave();
  }

  function setInboundTopics(value: string): void {
    setField(
      'inboundTopics',
      value
        .split('\n')
        .map((topic) => topic.trim())
        .filter(Boolean)
    );
  }

  function scheduleSave(): void {
    status = 'saving...';
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => void doSave(), 450);
  }

  export async function flush(): Promise<void> {
    if (saveTimer) {
      clearTimeout(saveTimer);
      saveTimer = null;
    }
    await doSave();
  }

  async function doSave(): Promise<void> {
    if (!profile) return;
    if (!dirty) return;
    try {
      profile = await saveAutopilotProfile(profile);
      dirty = false;
      status = 'saved';
    } catch (err) {
      status = err instanceof Error ? err.message : 'save failed';
    }
  }

  onMount(async () => {
    try {
      profile = await fetchAutopilotProfile();
      status = 'profile loaded';
    } catch (err) {
      status = err instanceof Error ? err.message : 'failed to load autopilot profile';
    }
  });

  onDestroy(() => {
    if (saveTimer) clearTimeout(saveTimer);
  });
</script>

<section class="autopilot-config" class:light={theme === 'light'} class:embedded>
  <div class="head">
    <div>
      <h3>Autopilot Runtime</h3>
      <span>{status}</span>
    </div>
  </div>

  {#if !profile}
    <div class="status block">loading autopilot profile...</div>
  {:else}
    <div class="grid">
      <label class="wide">
        <span>Native firmware binary</span>
        <input
          value={profile.nativeBinary}
          spellcheck="false"
          oninput={(e) => setField('nativeBinary', e.currentTarget.value)}
        />
      </label>

      <label>
        <span>Stack path</span>
        <input value={profile.stackPath} spellcheck="false" oninput={(e) => setField('stackPath', e.currentTarget.value)} />
      </label>

      <label>
        <span>Trusted vehicle endpoint · loopback only</span>
        <input value={profile.runtimeEndpoint} spellcheck="false" disabled />
      </label>

      <label>
        <span>Mocap source</span>
        <select
          value={profile.mocapSource}
          onchange={(e) => setField('mocapSource', e.currentTarget.value as MocapSource)}
        >
          <option value="real">Real mocap</option>
          <option value="sim">Simulation</option>
        </select>
      </label>

      <label class="wide">
        <span>Autopilot input topics monitored on local Zenoh</span>
        <textarea spellcheck="false" value={inboundTopicsText} oninput={(e) => setInboundTopics(e.currentTarget.value)}></textarea>
      </label>
    </div>
  {/if}
</section>

<style>
  .autopilot-config {
    display: grid;
    gap: 10px;
    padding: 12px;
    border: 1px solid rgba(145, 163, 156, 0.24);
    border-radius: 8px;
    background: rgba(255, 255, 255, 0.04);
    color: #edf6f1;
  }

  .autopilot-config.light {
    color: #12171b;
    background: rgba(255, 255, 255, 0.74);
  }

  .autopilot-config.embedded {
    padding: 0;
    border: 0;
    background: transparent;
  }

  .head {
    display: flex;
    justify-content: space-between;
    gap: 12px;
  }

  h3 {
    margin: 0;
    font-size: 0.86rem;
    letter-spacing: 0;
  }

  .head span,
  label span,
  .status {
    color: #91a39c;
    font-size: 0.68rem;
    font-weight: 700;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 10px;
  }

  label {
    display: grid;
    gap: 5px;
    min-width: 0;
  }

  .wide {
    grid-column: 1 / -1;
  }

  input,
  select,
  textarea {
    width: 100%;
    min-width: 0;
    border: 1px solid rgba(145, 163, 156, 0.28);
    border-radius: 7px;
    background: rgba(0, 0, 0, 0.26);
    color: inherit;
    padding: 0 9px;
    font: inherit;
    font-size: 0.72rem;
  }

  input,
  select {
    height: 34px;
  }

  textarea {
    min-height: 86px;
    padding: 9px;
    resize: vertical;
  }

  .light input,
  .light select,
  .light textarea {
    background: rgba(255, 255, 255, 0.8);
    border-color: rgba(25, 63, 92, 0.2);
  }

  @media (max-width: 760px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>

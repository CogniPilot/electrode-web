<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import {
    fetchSimulationModel,
    fetchSimulationProfile,
    saveSimulationModel,
    saveSimulationProfile,
    type ModelicaFile,
    type SimulationProfile,
    type SimulationVehicleKind
  } from '$lib/gcs';
  import {
    simError as browserSimErrorStore,
    simPhase as browserSimPhaseStore,
    startBrowserSim,
    stopBrowserSim
  } from '$lib/sim/simController';

  export let theme: 'light' | 'dark' = 'dark';
  /** Zenoh endpoint the viewer uses; the in-browser sim joins the same network. */
  export let zenohEndpoint = 'ws/127.0.0.1:7447';

  // The sim itself is app-wide (auto-started on Ground Station load); the
  // panel observes and controls the shared instance.
  $: browserSimPhase = $browserSimPhaseStore;
  $: browserSimError = $browserSimErrorStore;
  $: browserSimRunning = browserSimPhase !== 'idle' && browserSimPhase !== 'stopped';

  function toggleBrowserSim(): void {
    if (browserSimRunning) {
      stopBrowserSim();
      return;
    }
    if (!modelText.trim()) {
      browserSimErrorStore.set('model not loaded yet');
      return;
    }
    // Manual start uses the panel's (possibly edited) model buffer so a model
    // tweak can be exercised immediately without saving first.
    void startBrowserSim({
      endpoint: zenohEndpoint,
      modelSource: modelText,
      modelName: modelFile?.path.split('/').pop()?.replace(/\.mo(\.in)?$/, '')
    });
  }

  const vehicles: SimulationVehicleKind[] = ['fixedWing', 'quadrotor'];

  let profile: SimulationProfile | null = null;
  let modelFile: ModelicaFile | null = null;
  let modelText = '';
  let status = '';
  let editorStatus = '';
  let modelDirty = false;
  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  $: lineNumbers = modelText.split('\n').map((_, index) => index + 1).join('\n');
  $: lspLabel = modelFile?.lspCommand || profile?.modelicaLspCommand || 'modelica-language-server';

  function label(value: string): string {
    return value.replace(/([A-Z])/g, ' $1').replace(/_/g, ' ').replace(/^./, (char) => char.toUpperCase());
  }

  function toNumber(value: string, fallback: number): number {
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : fallback;
  }

  function modelPathLabel(profile: SimulationProfile, modelFile: ModelicaFile | null): string {
    const configured = `${profile.projectPath.replace(/\/$/, '')}/${profile.modelPath.replace(/^\//, '')}`;
    return configured || modelFile?.path || 'FixedWingTrueSILFull.mo';
  }

  function setField<K extends keyof SimulationProfile>(field: K, value: SimulationProfile[K]): void {
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
      profile = await saveSimulationProfile(profile);
      status = 'saved';
    } catch (err) {
      status = err instanceof Error ? err.message : 'save failed';
    }
  }

  async function loadModelFile(): Promise<void> {
    try {
      editorStatus = 'loading model...';
      modelFile = await fetchSimulationModel();
      modelText = modelFile.text;
      modelDirty = false;
      editorStatus = 'model loaded';
    } catch (err) {
      editorStatus = err instanceof Error ? err.message : 'failed to load model';
    }
  }

  async function saveModelFile(): Promise<void> {
    if (!modelFile) return;
    try {
      editorStatus = 'saving model...';
      modelFile = await saveSimulationModel(modelFile.path, modelText);
      modelText = modelFile.text;
      modelDirty = false;
      editorStatus = 'model saved';
    } catch (err) {
      editorStatus = err instanceof Error ? err.message : 'model save failed';
    }
  }

  onMount(async () => {
    try {
      const [loadedProfile, loadedModel] = await Promise.all([
        fetchSimulationProfile(),
        fetchSimulationModel()
      ]);
      profile = loadedProfile;
      modelFile = loadedModel;
      modelText = loadedModel.text;
      status = 'profile loaded';
      editorStatus = 'model loaded';
    } catch (err) {
      status = err instanceof Error ? err.message : 'failed to load simulation profile';
    }
  });

  onDestroy(() => {
    if (saveTimer) clearTimeout(saveTimer);
    // The in-browser sim is app-wide (it is the aircraft) — leaving the panel
    // must not stop it.
  });
</script>

<section class="sim-page" class:light={theme === 'light'}>
  <div class="page-head">
    <div>
      <h2>SIM</h2>
      <span>Rumoca TRUE SIL workbench for the fixed-wing full-aero project</span>
    </div>
    <strong class:on={browserSimRunning}>{browserSimRunning ? browserSimPhase : 'stopped'}</strong>
  </div>

  {#if !profile}
    <div class="empty">loading profile...</div>
  {:else}
    <div class="sim-layout">
      <aside class="controls">
        <div class="panel">
          <div class="panel-head">
            <strong>Simulation</strong>
            <em class:on={browserSimRunning}>{browserSimRunning ? browserSimPhase : 'stopped'}</em>
          </div>
          <p class="hint">
            The flight model runs in this tab (rumoca WASM) and plays the aircraft: PWM in from the
            autopilot, mocap pose out — over Zenoh ({zenohEndpoint}). No native process. Manual vs
            autopilot is not chosen here: exactly like the real plane, the transmitter mode switch
            decides on the autopilot itself (watch it flip in Autopilot I/O).
          </p>
          <div class="actions">
            <button type="button" class="primary" disabled={browserSimRunning} onclick={toggleBrowserSim}>
              Start
            </button>
            <button type="button" class="quiet" disabled={!browserSimRunning} onclick={toggleBrowserSim}>
              Stop
            </button>
          </div>
          {#if browserSimError}<span class="browser-sim-error">{browserSimError}</span>{/if}
        </div>

        <div class="panel">
          <div class="panel-head">
            <strong>Model</strong>
            <span>{status}</span>
          </div>

          <label>
            <span>Vehicle</span>
            <select value={profile.vehicleKind} onchange={(e) => setField('vehicleKind', e.currentTarget.value as SimulationVehicleKind)}>
              {#each vehicles as vehicle}
                <option value={vehicle}>{label(vehicle)}</option>
              {/each}
            </select>
          </label>

          <label>
            <span>Zenoh connect</span>
            <input value={profile.zenohConnect} spellcheck="false" oninput={(e) => setField('zenohConnect', e.currentTarget.value)} />
          </label>
        </div>

        <div class="panel">
          <div class="panel-head">
            <strong>Topics</strong>
            <span>same bus as hardware</span>
          </div>

          <label>
            <span>Command input</span>
            <input value={profile.commandInputTopic} spellcheck="false" oninput={(e) => setField('commandInputTopic', e.currentTarget.value)} />
          </label>
          <label>
            <span>Actuator output</span>
            <input value={profile.actuatorOutputTopic} spellcheck="false" oninput={(e) => setField('actuatorOutputTopic', e.currentTarget.value)} />
          </label>
          <label>
            <span>Sensor output</span>
            <input value={profile.sensorOutputTopic} spellcheck="false" oninput={(e) => setField('sensorOutputTopic', e.currentTarget.value)} />
          </label>
          <label>
            <span>Telemetry output</span>
            <input value={profile.telemetryOutputTopic} spellcheck="false" oninput={(e) => setField('telemetryOutputTopic', e.currentTarget.value)} />
          </label>
        </div>

      </aside>

      <main class="editor-shell">
        <div class="editor-toolbar">
          <div>
            <strong>FixedWingTrueSILFull.mo</strong>
            <span>{modelPathLabel(profile, modelFile)}</span>
          </div>
          <div class="editor-actions">
            <button type="button" class="quiet" onclick={loadModelFile}>Reload</button>
            <button type="button" class="primary" disabled={!modelDirty} onclick={saveModelFile}>Save</button>
          </div>
        </div>

        <div class="model-row single">
          <label class="confirm">
            <input type="checkbox" checked={profile.modelEditable} onchange={(e) => setField('modelEditable', e.currentTarget.checked)} />
            <span>Editable</span>
          </label>
        </div>

        <div class="lsp-strip">
          <strong>Modelica LSP</strong>
          <span>{lspLabel}</span>
          <em>{modelDirty ? 'modified' : editorStatus}</em>
        </div>

        <div class="editor">
          <pre aria-hidden="true">{lineNumbers}</pre>
          <textarea
            aria-label="Modelica source editor"
            spellcheck="false"
            readonly={!modelFile?.editable}
            bind:value={modelText}
            oninput={() => {
              modelDirty = true;
              editorStatus = 'modified';
            }}
          ></textarea>
        </div>

      </main>
    </div>

  {/if}
</section>

<style>
  .sim-page {
    margin-top: 10px;
    padding: 14px;
    border: 1px solid rgba(75, 168, 255, 0.26);
    border-radius: 8px;
    background: rgba(5, 8, 8, 0.72);
    color: #edf6f1;
  }

  .sim-page.light {
    background: rgba(255, 255, 255, 0.9);
    color: #12171b;
    border-color: rgba(31, 112, 185, 0.24);
  }

  .page-head,
  .panel-head,
  .editor-toolbar,
  .lsp-strip {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
  }

  .page-head {
    margin-bottom: 12px;
  }

  h2 {
    margin: 0;
    font-size: 1rem;
    letter-spacing: 0;
  }

  .page-head span,
  .panel-head span,
  .editor-toolbar span,
  label span,
  .empty,
  .lsp-strip span,
  .lsp-strip em {
    color: #91a39c;
    font-size: 0.66rem;
    font-weight: 700;
  }

  .page-head strong,
  .lsp-strip strong {
    color: #91a39c;
    font-size: 0.64rem;
    text-transform: uppercase;
  }

  .page-head strong.on {
    color: #7ee0ac;
  }

  .sim-layout {
    display: grid;
    grid-template-columns: minmax(290px, 380px) minmax(0, 1fr);
    gap: 12px;
    align-items: start;
  }

  .controls,
  .panel,
  .editor-shell {
    display: grid;
    gap: 8px;
    min-width: 0;
  }

  .panel,
  .editor-shell {
    padding: 10px;
    border: 1px solid rgba(145, 163, 156, 0.24);
    border-radius: 8px;
    background: rgba(255, 255, 255, 0.04);
  }

  .model-row,
  .actions {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 8px;
  }

  .panel-head em {
    font-style: normal;
    opacity: 0.6;
    text-transform: uppercase;
    font-size: 0.7rem;
    letter-spacing: 0.04em;
  }
  .panel-head em.on {
    color: #fd7719;
    opacity: 1;
  }
  .panel .hint {
    margin: 0;
    font-size: 0.72rem;
    opacity: 0.6;
    line-height: 1.4;
  }
  .browser-sim-error {
    font-size: 0.72rem;
    color: #e5484d;
  }

  label {
    display: grid;
    gap: 5px;
    min-width: 0;
  }

  input,
  select,
  textarea {
    min-width: 0;
    width: 100%;
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
    min-height: 104px;
    padding: 9px;
    resize: vertical;
  }

  .light input,
  .light select,
  .light textarea {
    background: #fff;
  }

  .confirm {
    grid-template-columns: auto 1fr;
    align-items: center;
    gap: 8px;
    align-self: end;
    min-height: 34px;
  }

  .confirm input {
    width: 16px;
    height: 16px;
    padding: 0;
  }

  .actions,
  .editor-actions {
    grid-template-columns: repeat(3, minmax(76px, auto));
    justify-content: end;
  }

  .editor-actions {
    display: grid;
    gap: 8px;
  }

  .actions button,
  .editor-actions button {
    min-height: 34px;
    border: 0;
    border-radius: 7px;
    padding: 0 14px;
    font-weight: 820;
    cursor: pointer;
  }

  .primary {
    background: #4ba8ff;
    color: #06121f;
  }

  .quiet {
    border: 1px solid rgba(145, 163, 156, 0.24);
    background: rgba(255, 255, 255, 0.06);
    color: inherit;
  }

  button:disabled {
    cursor: not-allowed;
    opacity: 0.42;
  }

  .lsp-strip {
    align-items: center;
    padding: 7px 9px;
    border-radius: 7px;
    background: rgba(75, 168, 255, 0.08);
  }

  .lsp-strip em {
    font-style: normal;
    color: #7ee0ac;
  }

  .editor {
    display: grid;
    grid-template-columns: 52px minmax(0, 1fr);
    min-height: 620px;
    border: 1px solid rgba(145, 163, 156, 0.24);
    border-radius: 8px;
    overflow: hidden;
    background: #071011;
  }

  .light .editor {
    background: #f7faf8;
  }

  .editor pre,
  .editor textarea {
    margin: 0;
    border: 0;
    border-radius: 0;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 0.74rem;
    line-height: 1.45;
  }

  .editor pre {
    padding: 10px 8px;
    overflow: hidden;
    text-align: right;
    user-select: none;
    color: #668078;
    background: rgba(255, 255, 255, 0.04);
  }

  .editor textarea {
    min-height: 620px;
    resize: vertical;
    white-space: pre;
    overflow: auto;
    tab-size: 2;
  }

  @media (max-width: 1060px) {
    .sim-layout {
      grid-template-columns: 1fr;
    }
  }

  @media (max-width: 620px) {
    .model-row,
    .actions,
    .editor-actions {
      grid-template-columns: 1fr;
    }

    .editor-toolbar {
      display: grid;
    }

    .editor {
      grid-template-columns: 44px minmax(0, 1fr);
      min-height: 480px;
    }

    .editor textarea {
      min-height: 480px;
    }
  }
</style>

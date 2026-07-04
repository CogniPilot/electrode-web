<script lang="ts">
  import { onMount } from 'svelte';
  import type { Attitude, ControlInputs } from '@electrode/sdk';
  import * as three from 'three';
  import type { Group, OrthographicCamera, Scene, WebGLRenderer } from 'three';
  import { loadVehicleRig, VEHICLE_LABELS, type VehicleKind, type VehicleRig } from '$lib/vehicle/vehicleRig';

  export let attitude: Attitude | null = null;
  export let controls: ControlInputs | null = null;
  export let motors: number[] | null = null;
  export let theme: 'light' | 'dark' = 'dark';
  export let vehicleType: VehicleKind = 'fixedwing';

  const FIT = 3.0;
  const FRUSTUM_HALF = 2.3;

  let container: HTMLDivElement;
  let canvas: HTMLCanvasElement;
  let renderer: WebGLRenderer | null = null;
  let scene: Scene | null = null;
  let topCamera: OrthographicCamera | null = null;
  let rearCamera: OrthographicCamera | null = null;
  let bodyPivot: Group | null = null;
  let rig: VehicleRig | null = null;
  let resizeObserver: ResizeObserver | null = null;
  let animationFrame = 0;
  let ready = false;
  let loadError = '';
  let loadToken = 0;
  let mounted = false;

  $: rollDeg = attitude?.rollDeg ?? 0;
  $: pitchDeg = attitude?.pitchDeg ?? 0;
  $: yawDeg = attitude?.yawDeg ?? 0;
  $: aileron = controls?.aileron ?? 0;
  $: elevator = controls?.elevator ?? 0;
  $: rudder = controls?.rudder ?? 0;
  $: throttle = controls?.throttle ?? 0;

  $: if (mounted && scene) {
    void loadVehicle(vehicleType);
  }
  $: applyBackground(theme);

  const vehicleKinds: VehicleKind[] = ['quadrotor', 'fixedwing'];

  function bgColor(name: 'light' | 'dark'): number {
    return name === 'light' ? 0xe7ebee : 0x070d0f;
  }

  onMount(() => {
    let disposed = false;
    requestAnimationFrame(() => {
      if (disposed) {
        return;
      }
      initScene();
      resizeObserver = new ResizeObserver(resize);
      resizeObserver.observe(container);
      resize();
      animate();
      mounted = true;
      void loadVehicle(vehicleType);
    });
    return () => {
      disposed = true;
      disposeScene();
    };
  });

  function initScene(): void {
    scene = new three.Scene();
    scene.background = new three.Color(bgColor(theme));

    // Top view: straight down, nose (-Z) toward the top of the frame.
    topCamera = new three.OrthographicCamera(-FRUSTUM_HALF, FRUSTUM_HALF, FRUSTUM_HALF, -FRUSTUM_HALF, 0.1, 100);
    topCamera.position.set(0, 12, 0);
    topCamera.up.set(0, 0, -1);
    topCamera.lookAt(0, 0, 0);

    // Rear view: from behind the tail (+Z) looking forward toward the nose.
    rearCamera = new three.OrthographicCamera(-FRUSTUM_HALF, FRUSTUM_HALF, FRUSTUM_HALF, -FRUSTUM_HALF, 0.1, 100);
    rearCamera.position.set(0, 0.2, 9);
    rearCamera.up.set(0, 1, 0);
    rearCamera.lookAt(0, 0, 0);

    renderer = new three.WebGLRenderer({ canvas, antialias: true, alpha: false, preserveDrawingBuffer: true });
    renderer.setClearColor(bgColor(theme), 1);
    renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
    renderer.outputColorSpace = three.SRGBColorSpace;

    scene.add(new three.AmbientLight(0xcfeee6, 0.95));
    const key = new three.DirectionalLight(0xffffff, 1.7);
    key.position.set(4, 10, 6);
    scene.add(key);
    const rear = new three.DirectionalLight(0xffd7a0, 0.8);
    rear.position.set(-3, 4, -8);
    scene.add(rear);

    bodyPivot = new three.Group();
    bodyPivot.rotation.order = 'YXZ';
    scene.add(bodyPivot);
  }

  async function loadVehicle(kind: VehicleKind): Promise<void> {
    if (!scene || !bodyPivot) {
      return;
    }
    if (rig && rig.kind === kind) {
      return;
    }
    const token = ++loadToken;
    ready = false;
    loadError = '';

    let nextRig: VehicleRig;
    try {
      nextRig = await loadVehicleRig(kind, FIT);
    } catch (error) {
      if (token === loadToken) {
        loadError = `Failed to load ${VEHICLE_LABELS[kind]} model`;
        console.error('DeflectionView: model load failed', error);
      }
      return;
    }

    if (token !== loadToken || !bodyPivot) {
      nextRig.dispose();
      return;
    }

    while (bodyPivot.children.length > 0) {
      bodyPivot.remove(bodyPivot.children[0]);
    }
    rig?.dispose();
    rig = nextRig;
    bodyPivot.add(rig.root);
    ready = true;
  }

  function applyBackground(name: 'light' | 'dark'): void {
    if (!scene || !renderer) {
      return;
    }
    if (scene.background instanceof three.Color) {
      scene.background.set(bgColor(name));
    }
    renderer.setClearColor(bgColor(name), 1);
  }

  function setOrthoAspect(camera: OrthographicCamera, aspect: number): void {
    const halfW = FRUSTUM_HALF * aspect;
    camera.left = -halfW;
    camera.right = halfW;
    camera.top = FRUSTUM_HALF;
    camera.bottom = -FRUSTUM_HALF;
    camera.updateProjectionMatrix();
  }

  function resize(): void {
    if (!container || !renderer) {
      return;
    }
    renderer.setSize(container.clientWidth, container.clientHeight, false);
  }

  function animate(): void {
    animationFrame = requestAnimationFrame(animate);
    if (!renderer || !scene || !topCamera || !rearCamera || !bodyPivot) {
      return;
    }

    bodyPivot.rotation.set(
      three.MathUtils.degToRad(pitchDeg),
      -three.MathUtils.degToRad(yawDeg),
      -three.MathUtils.degToRad(rollDeg)
    );
    if (ready && rig) {
      rig.update(controls, motors);
    }

    const width = renderer.domElement.width / renderer.getPixelRatio();
    const height = renderer.domElement.height / renderer.getPixelRatio();
    const halfW = Math.floor(width / 2);
    const rightW = width - halfW;

    renderer.setScissorTest(true);

    // Left: top-down view.
    renderer.setViewport(0, 0, halfW, height);
    renderer.setScissor(0, 0, halfW, height);
    setOrthoAspect(topCamera, halfW / Math.max(1, height));
    renderer.render(scene, topCamera);

    // Right: rear view.
    renderer.setViewport(halfW, 0, rightW, height);
    renderer.setScissor(halfW, 0, rightW, height);
    setOrthoAspect(rearCamera, rightW / Math.max(1, height));
    renderer.render(scene, rearCamera);

    renderer.setScissorTest(false);
  }

  function disposeScene(): void {
    cancelAnimationFrame(animationFrame);
    resizeObserver?.disconnect();
    rig?.dispose();
    renderer?.dispose();
    renderer = null;
    scene = null;
    topCamera = null;
    rearCamera = null;
    bodyPivot = null;
    rig = null;
    resizeObserver = null;
  }

  function clamp(value: number, min: number, max: number): number {
    return Math.min(max, Math.max(min, value));
  }
</script>

<div class="deflection-view" class:light={theme === 'light'}>
  <div class="viewport" bind:this={container}>
    <canvas bind:this={canvas} aria-label="Vehicle deflection view (top and rear)"></canvas>
    <span class="view-label label-top">TOP</span>
    <span class="view-label label-rear">REAR</span>
    <div class="view-divider" aria-hidden="true"></div>

    <div class="vehicle-select" role="group" aria-label="Vehicle model">
      {#each vehicleKinds as kind}
        <button
          type="button"
          class:active={vehicleType === kind}
          onclick={() => {
            vehicleType = kind;
          }}
        >
          {VEHICLE_LABELS[kind]}
        </button>
      {/each}
    </div>

    <div class="attitude-readout">
      <div><span>Roll</span><strong>{rollDeg.toFixed(0)}°</strong></div>
      <div><span>Pitch</span><strong>{pitchDeg.toFixed(0)}°</strong></div>
      <div><span>Yaw</span><strong>{yawDeg.toFixed(0)}°</strong></div>
    </div>

    {#if loadError}
      <div class="view-status error">{loadError}</div>
    {:else if !ready}
      <div class="view-status">Loading {VEHICLE_LABELS[vehicleType]}…</div>
    {/if}
  </div>

  <div class="control-readout">
    {#each [['Ail', aileron, true], ['Elev', elevator, true], ['Rud', rudder, true], ['Thr', throttle, false]] as [label, value, bipolar]}
      <div class="control-bar">
        <span>{label}</span>
        <div class="track" class:bipolar={bipolar as boolean}>
          {#if bipolar}
            <div class="center-tick"></div>
            <div
              class="fill"
              style={`left:${50 + ((value as number) < 0 ? clamp(value as number, -1, 1) * 50 : 0)}%; width:${Math.abs(clamp(value as number, -1, 1)) * 50}%;`}
            ></div>
          {:else}
            <div class="fill" style={`left:0%; width:${clamp(value as number, 0, 1) * 100}%;`}></div>
          {/if}
        </div>
        <em>{(value as number).toFixed(2)}</em>
      </div>
    {/each}
  </div>
</div>

<style>
  .deflection-view {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .viewport {
    position: relative;
    overflow: hidden;
    width: 100%;
    height: 300px;
    border: 1px solid #2a383f;
    border-radius: 8px;
    background: #070d0f;
  }

  .deflection-view.light .viewport {
    border-color: #cdd5dc;
    background: #e7ebee;
  }

  canvas {
    display: block;
    width: 100%;
    height: 100%;
  }

  .view-divider {
    position: absolute;
    top: 8%;
    bottom: 8%;
    left: 50%;
    width: 1px;
    background: rgba(253, 119, 25, 0.28);
    pointer-events: none;
  }

  .view-label {
    position: absolute;
    bottom: 10px;
    padding: 2px 8px;
    border-radius: 6px;
    background: rgba(5, 8, 8, 0.66);
    color: #9fb0a8;
    font-size: 0.6rem;
    font-weight: 800;
    letter-spacing: 0.08em;
    pointer-events: none;
  }

  .label-top {
    left: 12px;
  }

  .label-rear {
    right: 12px;
  }

  .deflection-view.light .view-label {
    background: rgba(255, 255, 255, 0.8);
    color: #5c6873;
  }

  .vehicle-select {
    position: absolute;
    top: 10px;
    left: 10px;
    display: flex;
    gap: 4px;
    padding: 4px;
    border: 1px solid rgba(253, 119, 25, 0.25);
    border-radius: 8px;
    background: rgba(5, 8, 8, 0.72);
    backdrop-filter: blur(5px);
  }

  .vehicle-select button {
    padding: 4px 11px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: #91a39c;
    font-size: 0.7rem;
    font-weight: 760;
    cursor: pointer;
  }

  .vehicle-select button.active {
    background: #fd7719;
    color: #10171a;
  }

  .deflection-view.light .vehicle-select {
    border-color: rgba(227, 95, 12, 0.3);
    background: rgba(255, 255, 255, 0.82);
  }

  .deflection-view.light .vehicle-select button {
    color: #5c6873;
  }

  .deflection-view.light .vehicle-select button.active {
    background: #e35f0c;
    color: #fff;
  }

  .attitude-readout {
    position: absolute;
    top: 10px;
    right: 10px;
    display: flex;
    gap: 5px;
    pointer-events: none;
  }

  .attitude-readout > div {
    display: grid;
    gap: 1px;
    padding: 4px 8px;
    border: 1px solid rgba(253, 119, 25, 0.2);
    border-radius: 7px;
    background: rgba(5, 8, 8, 0.72);
    text-align: center;
  }

  .attitude-readout span {
    color: #91a39c;
    font-size: 0.54rem;
    font-weight: 760;
    text-transform: uppercase;
  }

  .attitude-readout strong {
    color: #edf6f1;
    font-size: 0.82rem;
    font-weight: 760;
  }

  .view-status {
    position: absolute;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    padding: 6px 12px;
    border-radius: 8px;
    background: rgba(5, 8, 8, 0.78);
    color: #d6e4dd;
    font-size: 0.74rem;
    font-weight: 700;
  }

  .view-status.error {
    color: #ff8f6f;
    border: 1px solid rgba(255, 143, 111, 0.4);
  }

  .control-readout {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 5px 16px;
    padding: 9px 11px;
    border: 1px solid rgba(253, 119, 25, 0.2);
    border-radius: 8px;
    background: rgba(5, 8, 8, 0.5);
  }

  .deflection-view.light .attitude-readout > div,
  .deflection-view.light .control-readout {
    border-color: rgba(227, 95, 12, 0.28);
    background: rgba(255, 255, 255, 0.85);
  }

  .control-bar {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .control-bar span {
    width: 32px;
    color: #91a39c;
    font-size: 0.6rem;
    font-weight: 760;
    text-transform: uppercase;
  }

  .control-bar .track {
    position: relative;
    flex: 1;
    height: 8px;
    border-radius: 3px;
    background: #16242a;
    overflow: hidden;
  }

  .control-bar .center-tick {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 50%;
    width: 1px;
    background: #3a4c52;
  }

  .control-bar .fill {
    position: absolute;
    top: 1px;
    bottom: 1px;
    border-radius: 2px;
    background: #fd7719;
  }

  .control-bar em {
    width: 38px;
    color: #edf6f1;
    font-size: 0.68rem;
    font-style: normal;
    font-weight: 700;
    text-align: right;
  }

  .deflection-view.light .control-bar span {
    color: #5c6873;
  }

  .deflection-view.light .control-bar em {
    color: #12171b;
  }

  .deflection-view.light .control-bar .track {
    background: #dbe2e7;
  }
</style>

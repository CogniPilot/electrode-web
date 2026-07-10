<script lang="ts">
  import { onMount } from 'svelte';
  import type { Attitude, ControlInputs, MissionPlanState, Pose } from '@electrode/sdk';
  import * as three from 'three';
  import { OrbitControls } from 'three/examples/jsm/controls/OrbitControls.js';
  import type { BufferGeometry, Group, Line, Material, Object3D, PerspectiveCamera, Scene, Vector3, WebGLRenderer } from 'three';
  import { loadVehicleRig, type VehicleKind, type VehicleRig } from '$lib/vehicle/vehicleRig';

  export let pose: Pose | null = null;
  export let attitude: Attitude | null = null;
  export let controls: ControlInputs | null = null;
  export let motors: number[] | null = null;
  export let mission: MissionPlanState | null = null;
  export let localizationQuality = 0;
  export let theme: 'light' | 'dark' = 'dark';
  export let vehicleType: VehicleKind = 'fixedwing';

  // Local mocap metres are compressed into scene units for the room view. Keep
  // the vehicle on the same scale: fixed wing is 2 ft wingspan in local metres.
  const LOCAL_METERS_TO_SCENE = 0.09;
  const VEHICLE_FIT = 0.6096 * LOCAL_METERS_TO_SCENE;
  const MIN_ALTITUDE_SCENE_Y = 0.006;
  const CAMERA_TARGET = new three.Vector3(0, 0.24, 0);
  const CAMERA_START = new three.Vector3(1.15, 0.78, 1.25);
  // ENU/body yaw convention: yaw 0 faces +X (east), yaw +90 faces +Y (north).
  // The rig is authored/displayed with its nose along scene -Z, so rotate it
  // -90 deg at zero yaw to make the visual nose line up with +X.
  const YAW_ZERO_EAST_OFFSET_RAD = -Math.PI / 2;
  const MAX_TRAIL = 3600;

  // Follow mode: the orbit target eases onto the vehicle every frame and the
  // camera translates with it, preserving the user's orbit angle and zoom.
  const FOLLOW_LERP = 0.16;
  const followDelta = new three.Vector3();

  let followMode = false;
  let rig: VehicleRig | null = null;
  let rigLoadToken = 0;
  let trail: Line | null = null;
  let trailPositions: Float32Array | null = null;
  let trailCount = 0;
  let mounted = false;

  let container: HTMLDivElement;
  let canvas: HTMLCanvasElement;
  let renderer: WebGLRenderer | null = null;
  let scene: Scene | null = null;
  let camera: PerspectiveCamera | null = null;
  let orbit: OrbitControls | null = null;
  let vehicleGroup: Group | null = null;
  let frameGroup: Group | null = null;
  let missionGroup: Group | null = null;
  let missionSignature = '';
  let resizeObserver: ResizeObserver | null = null;
  let animationFrame = 0;

  type ScenePalette = {
    bg: number;
    clear: number;
    floor: number;
    floorOpacity: number;
    gridCenter: number;
    gridLine: number;
    fineA: number;
    fineB: number;
    ink: number;
    forward: number;
    xAxis: number;
    up: number;
    labelX: string;
    labelY: string;
    labelZ: string;
    labelShadow: string;
  };

  function paletteFor(name: 'light' | 'dark'): ScenePalette {
    if (name === 'light') {
      return {
        bg: 0xeef1f3,
        clear: 0xeef1f3,
        floor: 0xf2f5f7,
        floorOpacity: 0.5,
        gridCenter: 0xe35f0c,
        gridLine: 0xbcc6cc,
        fineA: 0xccd4d9,
        fineB: 0xdee3e6,
        ink: 0x141a1f,
        forward: 0x141a1f,
        xAxis: 0xe35f0c,
        up: 0xc4831c,
        labelX: '#e35f0c',
        labelY: '#141a1f',
        labelZ: '#b0761a',
        labelShadow: '#ffffff'
      };
    }
    return {
      bg: 0x0a1113,
      clear: 0x091012,
      floor: 0x0d1718,
      floorOpacity: 0.62,
      gridCenter: 0xfd7719,
      gridLine: 0x203a3a,
      fineA: 0x1d5b55,
      fineB: 0x142827,
      ink: 0xe9fff9,
      forward: 0xf4fbf7,
      xAxis: 0xfd7719,
      up: 0xffc35a,
      labelX: '#fd7719',
      labelY: '#f4fbf7',
      labelZ: '#ffc35a',
      labelShadow: '#000000'
    };
  }

  let pal: ScenePalette = paletteFor('dark');

  $: localX = pose?.xM ?? 0;
  $: localY = pose?.yM ?? 0;
  $: localAlt = pose?.altM ?? 0;
  $: updateVehicle(pose, attitude);
  $: updateMission(mission);
  $: applyTheme(theme);
  $: if (mounted && scene) {
    void loadVehicle(vehicleType);
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
    pal = paletteFor(theme);
    scene = new three.Scene();
    scene.background = new three.Color(pal.bg);
    scene.fog = new three.Fog(pal.bg, 26, 54);

    camera = new three.PerspectiveCamera(42, 1, 0.03, 28);
    camera.position.copy(CAMERA_START);
    camera.lookAt(CAMERA_TARGET);

    renderer = new three.WebGLRenderer({ canvas, antialias: true, alpha: false, preserveDrawingBuffer: true });
    renderer.setClearColor(pal.clear, 1);
    renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));

    orbit = new OrbitControls(camera, renderer.domElement);
    orbit.enableDamping = true;
    orbit.dampingFactor = 0.11;
    orbit.enableRotate = true;
    orbit.enablePan = true;
    orbit.enableZoom = true;
    orbit.screenSpacePanning = false;
    orbit.minPolarAngle = 0.18;
    orbit.maxPolarAngle = Math.PI * 0.48;
    orbit.minDistance = 0.18;
    orbit.maxDistance = 8;
    orbit.rotateSpeed = 0.58;
    orbit.zoomSpeed = 0.78;
    orbit.panSpeed = 0.82;
    orbit.target.copy(CAMERA_TARGET);
    orbit.mouseButtons = {
      LEFT: three.MOUSE.PAN,
      MIDDLE: three.MOUSE.DOLLY,
      RIGHT: three.MOUSE.ROTATE
    };
    orbit.touches = {
      ONE: three.TOUCH.PAN,
      TWO: three.TOUCH.DOLLY_PAN
    };
    orbit.addEventListener('change', clampCameraControls);
    orbit.update();

    const ambient = new three.AmbientLight(0xbffaf0, 0.78);
    scene.add(ambient);

    const keyLight = new three.DirectionalLight(0xdffbf6, 1.35);
    keyLight.position.set(7, 12, 8);
    scene.add(keyLight);

    const fillLight = new three.PointLight(0xfd7719, 3.8, 24);
    fillLight.position.set(-7, 4, -6);
    scene.add(fillLight);

    frameGroup = buildFrame();
    scene.add(frameGroup);

    vehicleGroup = createVehicleMarker();
    scene.add(vehicleGroup);

    trailPositions = new Float32Array(MAX_TRAIL * 3);
    trailCount = 0;
    const trailGeometry = new three.BufferGeometry();
    trailGeometry.setAttribute('position', new three.BufferAttribute(trailPositions, 3));
    trailGeometry.setDrawRange(0, 0);
    trail = new three.Line(
      trailGeometry,
      new three.LineBasicMaterial({ color: pal.xAxis, transparent: true, opacity: 0.7 })
    );
    scene.add(trail);

    updateVehicle(pose, attitude);
    updateMission(mission);
  }

  async function loadVehicle(kind: VehicleKind): Promise<void> {
    if (!scene || !vehicleGroup) {
      return;
    }
    if (rig && rig.kind === kind) {
      return;
    }
    const token = ++rigLoadToken;

    let nextRig: VehicleRig;
    try {
      nextRig = await loadVehicleRig(kind, VEHICLE_FIT);
    } catch (error) {
      console.error('IndoorScene: vehicle model load failed', error);
      return;
    }

    if (token !== rigLoadToken || !vehicleGroup) {
      nextRig.dispose();
      return;
    }

    // Replace the placeholder arrow marker (or the previous model) in place.
    while (vehicleGroup.children.length > 0) {
      const child = vehicleGroup.children[0];
      vehicleGroup.remove(child);
      disposeObject(child);
    }
    rig?.dispose();
    rig = nextRig;
    vehicleGroup.add(rig.root);
    // Reset the trail so it doesn't jump across a vehicle swap.
    trailCount = 0;
    if (trail) {
      trail.geometry.setDrawRange(0, 0);
    }
  }

  function applyTheme(name: 'light' | 'dark'): void {
    if (!scene || !renderer) {
      return;
    }

    pal = paletteFor(name);
    if (scene.background instanceof three.Color) {
      scene.background.set(pal.bg);
    }
    if (scene.fog) {
      (scene.fog as three.Fog).color.set(pal.bg);
    }
    renderer.setClearColor(pal.clear, 1);

    if (frameGroup) {
      scene.remove(frameGroup);
      disposeObject(frameGroup);
    }
    frameGroup = buildFrame();
    scene.add(frameGroup);

    // The vehicle model keeps its own GLB materials across themes; only recolor
    // the flight trail to the theme accent.
    if (trail) {
      (trail.material as three.LineBasicMaterial).color.set(pal.xAxis);
    }

    updateVehicle(pose, attitude);
    // The mission signature includes the theme, so this rebuilds the markers
    // with the new palette.
    updateMission(mission);
  }

  function buildFrame(): Group {
    const group = new three.Group();

    const floor = new three.Mesh(
      new three.PlaneGeometry(22, 22),
      new three.MeshBasicMaterial({
        color: pal.floor,
        transparent: true,
        opacity: pal.floorOpacity,
        side: three.DoubleSide
      })
    );
    floor.rotation.x = -Math.PI / 2;
    floor.position.y = -0.015;
    group.add(floor);

    const grid = new three.GridHelper(22, 22, pal.gridCenter, pal.gridLine);
    grid.position.y = 0;
    group.add(grid);

    const fineGrid = new three.GridHelper(22, 44, pal.fineA, pal.fineB);
    fineGrid.position.y = 0.006;
    group.add(fineGrid);

    const origin = new three.Mesh(
      new three.SphereGeometry(0.012, 18, 12),
      new three.MeshBasicMaterial({ color: pal.ink })
    );
    origin.position.y = 0.015;
    group.add(origin);

    const axisOrigin = new three.Vector3(0, 0.015, 0);
    group.add(new three.ArrowHelper(new three.Vector3(1, 0, 0), axisOrigin, 0.45, pal.xAxis, 0.07, 0.035));
    group.add(new three.ArrowHelper(new three.Vector3(0, 0, -1), axisOrigin, 0.45, pal.forward, 0.07, 0.035));
    group.add(new three.ArrowHelper(new three.Vector3(0, 1, 0), axisOrigin, 0.22, pal.up, 0.06, 0.03));

    addAxisLabel(group, 'X', pal.labelX, new three.Vector3(0.52, 0.06, 0));
    addAxisLabel(group, 'Y', pal.labelY, new three.Vector3(0, 0.06, -0.52));
    addAxisLabel(group, 'Z', pal.labelZ, new three.Vector3(0.05, 0.3, 0));

    return group;
  }

  function addAxisLabel(group: Group, text: string, color: string, position: Vector3): void {
    const labelCanvas = document.createElement('canvas');
    labelCanvas.width = 128;
    labelCanvas.height = 72;
    const context = labelCanvas.getContext('2d');
    if (!context) {
      return;
    }

    context.clearRect(0, 0, labelCanvas.width, labelCanvas.height);
    context.font = '700 44px Inter, system-ui, sans-serif';
    context.textAlign = 'center';
    context.textBaseline = 'middle';
    context.shadowColor = pal.labelShadow;
    context.shadowBlur = 10;
    context.fillStyle = color;
    context.fillText(text, labelCanvas.width / 2, labelCanvas.height / 2);

    const texture = new three.CanvasTexture(labelCanvas);
    const material = new three.SpriteMaterial({
      map: texture,
      transparent: true,
      depthTest: false,
      depthWrite: false
    });
    const sprite = new three.Sprite(material);
    sprite.position.copy(position);
    sprite.scale.set(0.16, 0.09, 1);
    group.add(sprite);
  }

  function createVehicleMarker(): Group {
    const group = new three.Group();
    group.rotation.order = 'YXZ';
    const bodyOrigin = new three.Vector3(0, 0, 0);
    const origin = new three.Mesh(
      new three.SphereGeometry(0.02, 18, 12),
      new three.MeshBasicMaterial({ color: pal.ink })
    );
    group.add(origin);
    group.add(new three.ArrowHelper(new three.Vector3(0, 0, -1), bodyOrigin, 0.1, pal.ink, 0.025, 0.012));
    group.add(new three.ArrowHelper(new three.Vector3(1, 0, 0), bodyOrigin, 0.08, pal.forward, 0.02, 0.01));
    group.add(new three.ArrowHelper(new three.Vector3(0, 1, 0), bodyOrigin, 0.065, pal.up, 0.018, 0.009));

    return group;
  }

  // Mission waypoints are near-static (a periodic broadcast of a fixed plan),
  // so the marker group is rebuilt only when the plan, active item, or theme
  // actually changes.
  function missionSig(plan: MissionPlanState | null): string {
    if (!plan) {
      return theme;
    }
    const points = plan.waypoints
      .map((wp) => (wp ? `${wp.east},${wp.north},${wp.up}` : 'x'))
      .join(';');
    return `${theme}|${plan.missionId}|${plan.currentSeq}|${points}`;
  }

  function updateMission(plan: MissionPlanState | null): void {
    if (!scene) {
      return;
    }
    const signature = missionSig(plan);
    if (signature === missionSignature) {
      return;
    }
    missionSignature = signature;

    if (missionGroup) {
      scene.remove(missionGroup);
      disposeObject(missionGroup);
      missionGroup = null;
    }
    if (!plan || plan.waypoints.every((wp) => wp === null)) {
      return;
    }
    missionGroup = buildMissionGroup(plan);
    scene.add(missionGroup);
  }

  function buildMissionGroup(plan: MissionPlanState): Group {
    const group = new three.Group();
    const known = plan.waypoints.filter((wp): wp is NonNullable<typeof wp> => wp !== null);

    // Planned path through the waypoints in sequence order.
    if (known.length >= 2) {
      const points = known.map((wp) => enuToScene(wp.east, wp.north, wp.up));
      const pathGeometry = new three.BufferGeometry().setFromPoints(points);
      group.add(
        new three.Line(
          pathGeometry,
          new three.LineBasicMaterial({ color: pal.up, transparent: true, opacity: 0.55 })
        )
      );
    }

    for (const wp of known) {
      const active = wp.seq === plan.currentSeq;
      const position = enuToScene(wp.east, wp.north, wp.up);

      const marker = new three.Mesh(
        new three.SphereGeometry(active ? 0.03 : 0.018, 18, 12),
        new three.MeshBasicMaterial({
          color: active ? pal.xAxis : pal.ink,
          transparent: !active,
          opacity: active ? 1 : 0.78
        })
      );
      marker.position.copy(position);
      group.add(marker);

      if (active) {
        // Halo ring so the active waypoint reads at a glance.
        const halo = new three.Mesh(
          new three.RingGeometry(0.045, 0.058, 32),
          new three.MeshBasicMaterial({
            color: pal.xAxis,
            transparent: true,
            opacity: 0.85,
            side: three.DoubleSide
          })
        );
        halo.rotation.x = -Math.PI / 2;
        halo.position.copy(position);
        group.add(halo);
      }

      // Drop line to the floor anchors the altitude visually.
      const dropGeometry = new three.BufferGeometry().setFromPoints([
        position,
        new three.Vector3(position.x, 0.002, position.z)
      ]);
      group.add(
        new three.Line(
          dropGeometry,
          new three.LineBasicMaterial({
            color: active ? pal.xAxis : pal.gridLine,
            transparent: true,
            opacity: active ? 0.55 : 0.4
          })
        )
      );

      addAxisLabel(
        group,
        String(wp.seq + 1),
        active ? pal.labelX : pal.labelY,
        position.clone().add(new three.Vector3(0, 0.07, 0))
      );
    }

    return group;
  }

  function enuToScene(east: number, north: number, up: number): Vector3 {
    return new three.Vector3(
      clamp(east * LOCAL_METERS_TO_SCENE, -9.8, 9.8),
      localAltitudeSceneY(up),
      clamp(-north * LOCAL_METERS_TO_SCENE, -9.8, 9.8)
    );
  }

  function disposeObject(root: Object3D): void {
    root.traverse((object: Object3D) => {
      const renderable = object as Object3D & {
        geometry?: BufferGeometry;
        material?: Material | Material[];
      };
      renderable.geometry?.dispose();
      if (renderable.material) {
        const materials = Array.isArray(renderable.material) ? renderable.material : [renderable.material];
        for (const material of materials) {
          const materialWithMap = material as Material & { map?: { dispose: () => void } };
          materialWithMap.map?.dispose();
          material.dispose();
        }
      }
    });
  }

  function updateVehicle(nextPose: Pose | null, nextAttitude: Attitude | null): void {
    if (!vehicleGroup || !nextPose) {
      return;
    }

    const vehiclePosition = localPositionToScene(nextPose);
    vehicleGroup.position.copy(vehiclePosition);
    vehicleGroup.quaternion.copy(attitudeToSceneQuaternion(nextAttitude));
    recordTrail(vehiclePosition);
  }

  function attitudeToSceneQuaternion(nextAttitude: Attitude | null): three.Quaternion {
    if (hasSynapseQuaternion(nextAttitude)) {
      const bodyToEnu = new three.Quaternion(
        nextAttitude.qx,
        nextAttitude.qy,
        nextAttitude.qz,
        nextAttitude.qw
      ).normalize();
      const xAxis = synapseBodyVectorToScene(modelLocalVectorToSynapseBody(new three.Vector3(1, 0, 0)), bodyToEnu);
      const yAxis = synapseBodyVectorToScene(modelLocalVectorToSynapseBody(new three.Vector3(0, 1, 0)), bodyToEnu);
      const zAxis = synapseBodyVectorToScene(modelLocalVectorToSynapseBody(new three.Vector3(0, 0, 1)), bodyToEnu);
      return new three.Quaternion().setFromRotationMatrix(new three.Matrix4().makeBasis(xAxis, yAxis, zAxis));
    }

    return new three.Quaternion().setFromEuler(
      new three.Euler(
        three.MathUtils.degToRad(nextAttitude?.pitchDeg ?? 0),
        three.MathUtils.degToRad(nextAttitude?.yawDeg ?? 0) + YAW_ZERO_EAST_OFFSET_RAD,
        -three.MathUtils.degToRad(nextAttitude?.rollDeg ?? 0),
        'YXZ'
      )
    );
  }

  function hasSynapseQuaternion(
    nextAttitude: Attitude | null
  ): nextAttitude is Attitude & { qx: number; qy: number; qz: number; qw: number } {
    return (
      nextAttitude !== null &&
      Number.isFinite(nextAttitude.qx) &&
      Number.isFinite(nextAttitude.qy) &&
      Number.isFinite(nextAttitude.qz) &&
      Number.isFinite(nextAttitude.qw)
    );
  }

  function modelLocalVectorToSynapseBody(vector: three.Vector3): three.Vector3 {
    return new three.Vector3(-vector.z, -vector.x, vector.y);
  }

  function synapseBodyVectorToScene(vector: three.Vector3, bodyToEnu: three.Quaternion): three.Vector3 {
    const enu = vector.applyQuaternion(bodyToEnu);
    return new three.Vector3(enu.x, enu.z, -enu.y).normalize();
  }

  function recordTrail(position: Vector3): void {
    if (!trail || !trailPositions) {
      return;
    }
    if (trailCount >= MAX_TRAIL) {
      trailPositions.copyWithin(0, 3);
      trailCount = MAX_TRAIL - 1;
    }
    const index = trailCount;
    trailPositions[index * 3] = position.x;
    trailPositions[index * 3 + 1] = position.y;
    trailPositions[index * 3 + 2] = position.z;
    trailCount++;
    const attribute = trail.geometry.getAttribute('position') as three.BufferAttribute;
    attribute.needsUpdate = true;
    trail.geometry.setDrawRange(0, Math.min(trailCount, MAX_TRAIL));
  }

  function localPositionToScene(nextPose: Pose): Vector3 {
    return new three.Vector3(
      clamp(nextPose.xM * LOCAL_METERS_TO_SCENE, -9.8, 9.8),
      localAltitudeSceneY(nextPose.altM),
      clamp(-nextPose.yM * LOCAL_METERS_TO_SCENE, -9.8, 9.8)
    );
  }

  function localAltitudeSceneY(altM: number): number {
    return clamp(altM * LOCAL_METERS_TO_SCENE, MIN_ALTITUDE_SCENE_Y, 3.2);
  }

  function resize(): void {
    if (!container || !renderer || !camera) {
      return;
    }

    const width = container.clientWidth;
    const height = container.clientHeight;
    renderer.setSize(width, height, false);
    camera.aspect = width / Math.max(1, height);
    camera.updateProjectionMatrix();
  }

  function animate(): void {
    animationFrame = requestAnimationFrame(animate);
    if (!renderer || !scene || !camera) {
      return;
    }
    rig?.update(controls, motors);
    if (followMode && orbit && vehicleGroup) {
      followDelta.copy(vehicleGroup.position).sub(orbit.target).multiplyScalar(FOLLOW_LERP);
      orbit.target.add(followDelta);
      camera.position.add(followDelta);
    }
    orbit?.update();
    renderer.render(scene, camera);
  }

  function toggleFollow(): void {
    followMode = !followMode;
    if (orbit) {
      // Panning would fight the follow target every frame; hand the target
      // back to the user only when follow is off.
      orbit.enablePan = !followMode;
    }
  }

  function disposeScene(): void {
    cancelAnimationFrame(animationFrame);
    resizeObserver?.disconnect();
    orbit?.removeEventListener('change', clampCameraControls);
    orbit?.dispose();
    rig?.dispose();
    rig = null;
    renderer?.dispose();
    scene?.traverse((object: Object3D) => {
      const maybeRenderable = object as Object3D & {
        geometry?: BufferGeometry;
        material?: Material | Material[];
      };
      maybeRenderable.geometry?.dispose();
      if (maybeRenderable.material) {
        const materials = Array.isArray(maybeRenderable.material) ? maybeRenderable.material : [maybeRenderable.material];
        for (const material of materials) {
          const materialWithMap = material as Material & { map?: { dispose: () => void } };
          materialWithMap.map?.dispose();
          material.dispose();
        }
      }
    });
    renderer = null;
    scene = null;
    camera = null;
    orbit = null;
    vehicleGroup = null;
    frameGroup = null;
    missionGroup = null;
    missionSignature = '';
    trail = null;
    trailPositions = null;
    resizeObserver = null;
  }

  function clampCameraControls(): void {
    if (!orbit) {
      return;
    }

    const min = new three.Vector3(-13, 0.05, -13);
    const max = new three.Vector3(13, 4.6, 13);
    orbit.target.clamp(min, max);
  }

  function clamp(value: number, min: number, max: number): number {
    return Math.min(max, Math.max(min, value));
  }

  function containSceneWheel(event: WheelEvent): void {
    event.preventDefault();
  }

  function containSceneContextMenu(event: MouseEvent): void {
    event.preventDefault();
  }
</script>

<div
  class="indoor-scene"
  class:light={theme === 'light'}
  bind:this={container}
  role="application"
  aria-label="Indoor local navigation map"
  onwheel={containSceneWheel}
  oncontextmenu={containSceneContextMenu}
>
  <canvas bind:this={canvas} aria-label="Indoor 3D local navigation view"></canvas>
  <button
    type="button"
    class="follow-toggle"
    class:active={followMode}
    aria-pressed={followMode}
    onclick={toggleFollow}
  >
    {followMode ? 'Following' : 'Follow'}
  </button>
  <div class="indoor-readout">
    <div>
      <span>Local X</span>
      <strong>{localX.toFixed(1)} m</strong>
    </div>
    <div>
      <span>Local Y</span>
      <strong>{localY.toFixed(1)} m</strong>
    </div>
    <div>
      <span>Alt</span>
      <strong>{localAlt.toFixed(1)} m</strong>
    </div>
    <div>
      <span>Quality</span>
      <strong>{(localizationQuality * 100).toFixed(0)}%</strong>
    </div>
  </div>
</div>

<style>
  .indoor-scene {
    position: relative;
    overflow: hidden;
    width: 100%;
    height: 420px;
    border: 1px solid #2a383f;
    border-radius: 8px;
    background: #091012;
    overscroll-behavior: contain;
    touch-action: none;
  }

  .indoor-scene.light {
    border-color: #cdd5dc;
    background: #eef1f3;
  }

  canvas {
    display: block;
    width: 100%;
    height: 100%;
    cursor: grab;
    touch-action: none;
  }

  canvas:active {
    cursor: grabbing;
  }

  .follow-toggle {
    position: absolute;
    top: 12px;
    right: 12px;
    padding: 6px 12px;
    border: 1px solid rgba(253, 119, 25, 0.35);
    border-radius: 8px;
    background: rgba(5, 8, 8, 0.74);
    backdrop-filter: blur(5px);
    color: #edf6f1;
    font-size: 0.68rem;
    font-weight: 760;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    cursor: pointer;
  }

  .follow-toggle:hover {
    border-color: rgba(253, 119, 25, 0.7);
  }

  .follow-toggle.active {
    border-color: #fd7719;
    background: rgba(253, 119, 25, 0.22);
    color: #ffd9b8;
  }

  .indoor-scene.light .follow-toggle {
    border-color: rgba(227, 95, 12, 0.35);
    background: rgba(255, 255, 255, 0.82);
    color: #12171b;
  }

  .indoor-scene.light .follow-toggle:hover {
    border-color: rgba(227, 95, 12, 0.75);
  }

  .indoor-scene.light .follow-toggle.active {
    border-color: #e35f0c;
    background: rgba(227, 95, 12, 0.16);
    color: #a04208;
  }

  .indoor-readout {
    position: absolute;
    right: 12px;
    bottom: 12px;
    left: 12px;
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 6px;
    pointer-events: none;
  }

  .indoor-readout > div {
    display: grid;
    gap: 3px;
    min-width: 0;
    padding: 7px 8px;
    border: 1px solid rgba(253, 119, 25, 0.2);
    border-radius: 8px;
    background: rgba(5, 8, 8, 0.74);
    backdrop-filter: blur(5px);
  }

  .indoor-readout span {
    overflow: hidden;
    color: #91a39c;
    font-size: 0.62rem;
    font-weight: 760;
    text-overflow: ellipsis;
    text-transform: uppercase;
    white-space: nowrap;
  }

  .indoor-readout strong {
    overflow: hidden;
    color: #edf6f1;
    font-size: 0.83rem;
    font-weight: 760;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .indoor-scene.light .indoor-readout > div {
    border-color: rgba(227, 95, 12, 0.28);
    background: rgba(255, 255, 255, 0.82);
  }

  .indoor-scene.light .indoor-readout span {
    color: #5c6873;
  }

  .indoor-scene.light .indoor-readout strong {
    color: #12171b;
  }

  @media (max-width: 820px) {
    .indoor-scene {
      height: 300px;
    }

    .indoor-readout {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
</style>

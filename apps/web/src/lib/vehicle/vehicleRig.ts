// Shared loader + rig for the quadrotor / fixed-wing sim models. Encapsulates
// the GLB loading, centering/scaling, hinge-axis rigging of control surfaces,
// and per-frame surface deflection / rotor spin so the same vehicle can be
// dropped into any Three.js scene (the flying local-nav map and the fixed-angle
// deflection panel). Node names and hinge axes match the rigged rumoca sim
// models (airplane.glb / drone.glb).
import * as three from 'three';
import { GLTFLoader } from 'three/examples/jsm/loaders/GLTFLoader.js';
import type { ControlInputs } from '@electrode/sdk';

export type VehicleKind = 'quadrotor' | 'fixedwing';

export const VEHICLE_LABELS: Record<VehicleKind, string> = {
  quadrotor: 'Quadrotor',
  fixedwing: 'Fixed Wing'
};

type SurfaceDriver = 'aileron' | 'elevator' | 'rudder' | 'prop';

type SurfaceRig = {
  pivot: string;
  mesh: string;
  axis: [number, number, number];
  sign: number;
  driver: SurfaceDriver;
};

type VehicleConfig = {
  url: string;
  fitSize: number;
  surfaces: SurfaceRig[];
  rotors: { names: string[]; spinSigns: number[] } | null;
};

const VEHICLES: Record<VehicleKind, VehicleConfig> = {
  fixedwing: {
    url: '/assets/models/airplane.glb',
    fitSize: 4.2,
    surfaces: [
      { pivot: 'ElevatorPivot', mesh: 'Elevator', axis: [0, 0, 1], sign: 1, driver: 'elevator' },
      { pivot: 'LeftAileronPivot', mesh: 'LeftAileron', axis: [0, 0, 1], sign: 1, driver: 'aileron' },
      { pivot: 'RightAileronPivot', mesh: 'RightAileron', axis: [0, 0, 1], sign: -1, driver: 'aileron' },
      { pivot: 'RudderPivot', mesh: 'Rudder', axis: [0, 1, 0], sign: 1, driver: 'rudder' },
      { pivot: 'PropPivot', mesh: 'Prop', axis: [1, 0, 0], sign: 1, driver: 'prop' }
    ],
    rotors: null
  },
  quadrotor: {
    url: '/assets/models/drone.glb',
    fitSize: 3.6,
    surfaces: [],
    rotors: {
      names: ['Object_34', 'Object_38', 'Object_36', 'Object_32'],
      spinSigns: [-1, -1, -1, -1]
    }
  }
};

// Peak surface deflection (rad) at full stick, exaggerated for visibility to
// match the rumoca viewer's ~1.6x gain on ~0.35 rad.
const MAX_DEFLECT = 0.55;

// Per-frame propeller/rotor spin step (rad). Kept small so the blades read as
// spinning forward instead of aliasing into a stutter/reverse (wagon-wheel) at
// 60fps: a 2-blade prop has 180deg symmetry, so steps must stay well under 90.
const ROTOR_IDLE_STEP = 0.12;
const ROTOR_GAIN_STEP = 0.3;

type RiggedSurface = {
  node: three.Object3D;
  base: three.Quaternion;
  axis: three.Vector3;
  sign: number;
  driver: SurfaceDriver;
};

type RiggedRotor = { node: three.Object3D; spinSign: number };

export type VehicleRig = {
  kind: VehicleKind;
  /** aligned + centered model group; add this to your own attitude pivot */
  root: three.Group;
  /** apply live control-surface deflection + rotor spin */
  update(controls: ControlInputs | null, motors: number[] | null): void;
  dispose(): void;
};

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/**
 * Load a vehicle model and return a rig whose `root` can be parented under any
 * attitude/position pivot. `targetSize` overrides the model's default fit size
 * (largest bounding-box dimension in scene units).
 */
export async function loadVehicleRig(kind: VehicleKind, targetSize?: number): Promise<VehicleRig> {
  const config = VEHICLES[kind];
  const gltf = await new GLTFLoader().loadAsync(config.url);
  const model = gltf.scene;

  configureModelMeshes(model);
  fitModelToSize(model, targetSize ?? config.fitSize);

  const root = createAlignedRoot(model);
  const byName = collectNodesByName(model);
  const surfaces = rigSurfaces(config.surfaces, byName);
  const rotors = rigRotors(config.rotors, byName);
  const update = createRigUpdater(surfaces, rotors);

  return {
    kind,
    root,
    update,
    dispose: () => disposeRoot(root)
  };
}

function configureModelMeshes(model: three.Object3D): void {
  model.traverse((object) => {
    const mesh = object as three.Mesh;
    if (mesh.isMesh) {
      mesh.castShadow = false;
      mesh.receiveShadow = false;
    }
  });
}

function fitModelToSize(model: three.Object3D, targetSize: number): void {
  const box = new three.Box3().setFromObject(model);
  const size = new three.Vector3();
  const center = new three.Vector3();
  box.getSize(size);
  box.getCenter(center);
  const maxDim = Math.max(size.x, size.y, size.z) || 1;
  const scale = targetSize / maxDim;
  // Scale first, then offset by the scaled center so the model is centered on
  // the root origin (the offset lives in the parent frame, so it must be scaled
  // to match the now-scaled geometry — otherwise the model sits off-center).
  model.scale.setScalar(scale);
  model.position.copy(center).multiplyScalar(-scale);
}

function createAlignedRoot(model: three.Object3D): three.Group {
  const root = new three.Group();
  root.rotation.y = Math.PI;
  root.add(model);
  // Bring world matrices up to date before measuring hinge/rotor centers below.
  root.updateMatrixWorld(true);
  return root;
}

function collectNodesByName(model: three.Object3D): Record<string, three.Object3D> {
  const byName: Record<string, three.Object3D> = {};
  model.traverse((object) => {
    byName[object.name] = object;
  });
  return byName;
}

function rigSurfaces(rigs: SurfaceRig[], byName: Record<string, three.Object3D>): RiggedSurface[] {
  const surfaces: RiggedSurface[] = [];
  for (const rig of rigs) {
    const pivot = byName[rig.pivot];
    const mesh = byName[rig.mesh];
    if (!pivot || !mesh) {
      console.warn('vehicleRig: missing surface node', rig.pivot, rig.mesh);
      continue;
    }
    (pivot as three.Object3D & { attach: (o: three.Object3D) => void }).attach(mesh);
    surfaces.push({
      node: pivot,
      base: pivot.quaternion.clone(),
      axis: new three.Vector3(rig.axis[0], rig.axis[1], rig.axis[2]),
      sign: rig.sign,
      driver: rig.driver
    });
  }
  return surfaces;
}

function rigRotors(config: VehicleConfig['rotors'], byName: Record<string, three.Object3D>): RiggedRotor[] {
  const rotors: RiggedRotor[] = [];
  if (config) {
    config.names.forEach((name, index) => {
      const rotor = byName[name];
      const parent = rotor?.parent;
      if (!rotor || !parent) {
        console.warn('vehicleRig: missing rotor node', name);
        return;
      }
      const bounds = new three.Box3().setFromObject(rotor);
      const rotorCenter = new three.Vector3();
      bounds.getCenter(rotorCenter);
      const pivot = new three.Group();
      pivot.name = `${name}_spin`;
      pivot.position.copy(parent.worldToLocal(rotorCenter.clone()));
      parent.add(pivot);
      (pivot as three.Object3D & { attach: (o: three.Object3D) => void }).attach(rotor);
      rotors.push({ node: pivot, spinSign: config.spinSigns[index] ?? -1 });
    });
  }
  return rotors;
}

function createRigUpdater(surfaces: RiggedSurface[], rotors: RiggedRotor[]): VehicleRig['update'] {
  let propSpin = 0;
  const q = new three.Quaternion();

  return (controls: ControlInputs | null, motors: number[] | null): void => {
    const aileron = controls?.aileron ?? 0;
    const elevator = controls?.elevator ?? 0;
    const rudder = controls?.rudder ?? 0;
    const throttle = controls?.throttle ?? 0;
    const driverValue: Record<SurfaceDriver, number> = { aileron, elevator, rudder, prop: 0 };

    for (const surface of surfaces) {
      if (surface.driver === 'prop') {
        propSpin += ROTOR_IDLE_STEP + clamp(throttle, 0, 1) * ROTOR_GAIN_STEP;
        q.setFromAxisAngle(surface.axis, surface.sign * propSpin);
      } else {
        const angle = clamp(driverValue[surface.driver], -1, 1) * MAX_DEFLECT;
        q.setFromAxisAngle(surface.axis, surface.sign * angle);
      }
      surface.node.quaternion.copy(surface.base).multiply(q);
    }

    rotors.forEach((rotor, index) => {
      const command = clamp(motors?.[index] ?? throttle, 0, 1);
      rotor.node.rotation.y += rotor.spinSign * (ROTOR_IDLE_STEP + command * ROTOR_GAIN_STEP);
    });
  };
}

function disposeRoot(root: three.Object3D): void {
  root.traverse((object) => {
    const renderable = object as three.Mesh;
    renderable.geometry?.dispose();
    const material = renderable.material;
    if (material) {
      const list = Array.isArray(material) ? material : [material];
      for (const entry of list) {
        const withMap = entry as three.Material & { map?: { dispose: () => void } };
        withMap.map?.dispose();
        entry.dispose();
      }
    }
  });
}

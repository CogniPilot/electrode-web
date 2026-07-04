import type { MappingProfile, JoystickSnapshot } from '$lib/gcs';
import type { ManualControlState } from '@electrode/sdk';

/**
 * Mirror of the manual-control-bridge's `encode_manual_control`, computed in the
 * browser so the Manual Link box can show the *hardware* path — the physical
 * controller run through the current mapping — without going through Zenoh.
 */

/** Latched arm/kill state for buttons configured as toggles. */
export interface LatchState {
  arm: boolean;
  kill: boolean;
}

export function emptyLatch(): LatchState {
  return { arm: false, kill: false };
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function signedAxis(snap: JoystickSnapshot, axis: number, invert: boolean): number {
  const value = snap.axes[axis] ?? 0;
  return invert ? -value : value;
}

/**
 * Advance the toggle latches on button rising edges. Call once per snapshot,
 * threading `prevButtons` through, before {@link mapJoystickToManual}.
 */
export function advanceLatch(
  snap: JoystickSnapshot,
  mapping: MappingProfile,
  latch: LatchState,
  prevButtons: number[]
): LatchState {
  let { arm, kill } = latch;
  const rising = (index: number | null): boolean =>
    index !== null && snap.buttons[index] === 1 && prevButtons[index] !== 1;
  if (mapping.armToggle && rising(mapping.armButton)) arm = !arm;
  if (mapping.killToggle && rising(mapping.killButton)) kill = !kill;
  return { arm, kill };
}

export function mapJoystickToManual(
  snap: JoystickSnapshot,
  mapping: MappingProfile,
  latch: LatchState,
  nowMs: number
): ManualControlState {
  const roll = clamp(signedAxis(snap, mapping.rollAxis, mapping.invertRoll), -1, 1);
  const pitch = clamp(signedAxis(snap, mapping.pitchAxis, mapping.invertPitch), -1, 1);
  const yaw = clamp(signedAxis(snap, mapping.yawAxis, mapping.invertYaw), -1, 1);
  const throttle = clamp(
    (signedAxis(snap, mapping.throttleAxis, mapping.invertThrottle) + 1) * 0.5,
    0,
    1
  );
  const flightMode = signedAxis(snap, mapping.modeAxis, false) > 0 ? 1 : 0;
  const active = signedAxis(snap, mapping.activeAxis, mapping.invertActive) > 0;

  const armMomentary = mapping.armButton !== null && snap.buttons[mapping.armButton] === 1;
  const killMomentary = mapping.killButton !== null && snap.buttons[mapping.killButton] === 1;
  const armSwitch = mapping.armToggle ? latch.arm : armMomentary;
  const killSwitch = mapping.killToggle ? latch.kill : killMomentary;

  return {
    roll,
    pitch,
    yaw,
    throttle,
    flightMode,
    armSwitch,
    killSwitch,
    active,
    valid: true,
    updatedAtMs: nowMs
  };
}

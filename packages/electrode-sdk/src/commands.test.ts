import { describe, expect, it } from 'vitest';

import { buildCommandIntent, validateCommandPreconditions } from './commands';
import { createInitialVehicleState } from './state-store';

describe('command helpers', () => {
  it('builds deterministic command intents with resolved topics and expiry', () => {
    const intent = buildCommandIntent({
      vehicleId: 'cubs2',
      command: 'setMode',
      args: { mode: 'auto' },
      sequence: 7,
      nowMs: 1000
    });

    expect(intent).toMatchObject({
      kind: 'command',
      commandId: 'cubs2-setMode-7-1000',
      command: 'setMode',
      vehicleId: 'cubs2',
      topic: 'vehicle/cubs2/cmd/mode',
      args: { mode: 'auto' },
      createdAtMs: 1000,
      expiresAtMs: 2000,
      sequence: 7
    });
  });

  it('reports safety precondition failures before publishing commands', () => {
    const state = createInitialVehicleState('cubs2');

    expect(validateCommandPreconditions(state, 'arm')).toEqual(['vehicle is not connected', 'localization is stale']);

    state.connected = true;
    state.localization.fresh = true;
    state.mode.failsafe = true;

    expect(validateCommandPreconditions(state, 'arm')).toEqual(['vehicle is in failsafe']);
    expect(validateCommandPreconditions(state, 'disarm')).toEqual([]);
  });
});

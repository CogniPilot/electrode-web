// Regenerate the TypeScript FlatBuffers readers for the Synapse topics that the
// browser decodes when connected directly over Zenoh (see
// packages/electrode-sdk/src/synapse-decode.ts).
//
// Source of truth: the canonical schemas shipped by the published
// `@cognipilot/synapse-fbs` npm package. That package intentionally ships only
// the `.fbs`/`.bfbs` assets (see its `index.js` exports `fbsDir`/`schemaFiles`),
// so consumers generate their own bindings with the pinned `flatc` (25.12.19)
// that matches the `flatbuffers` npm runtime.
//
// Usage:
//   node scripts/generate-synapse-fbs.mjs
//   SYNAPSE_FLATC=/path/to/flatc node scripts/generate-synapse-fbs.mjs
import { existsSync, mkdirSync, rmSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { fbsDir } from '@cognipilot/synapse-fbs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const outDir = resolve(root, 'packages/electrode-sdk/src/generated');

// Known pinned flatc (25.12.19) bootstrapped by ../synapse_fbs, which matches
// the installed `flatbuffers` runtime. Used as the default when `SYNAPSE_FLATC`
// is not set so `npm run check` works without extra environment setup.
const BOOTSTRAP_FLATC =
  '/home/micah/autopilot/synapse_fbs/target/xtask/flatc-bootstrap/target/debug/build/flatbuffers-build-1bac332fdb7e5d36/out/bin/flatc';

function findFlatc() {
  // Prefer an explicit override, then the known version-pinned bootstrap flatc,
  // then whatever is on PATH.
  const override = process.env.SYNAPSE_FLATC;
  if (override) {
    return override;
  }
  if (existsSync(BOOTSTRAP_FLATC)) {
    return BOOTSTRAP_FLATC;
  }
  return 'flatc';
}

const flatc = findFlatc();
const version = spawnSync(flatc, ['--version'], { encoding: 'utf8' });
if (version.error?.code === 'ENOENT') {
  console.error(
    `flatc not found at "${flatc}". Set SYNAPSE_FLATC to the pinned 25.12.19 flatc, ` +
      'build it in ../synapse_fbs (`cargo xtask ...`), or install FlatBuffers 25.12.19.'
  );
  process.exit(1);
}
if (!String(version.stdout).includes('25.12.19')) {
  console.warn(
    `Warning: expected flatc 25.12.19 (matches flatbuffers runtime), got: ${String(version.stdout).trim()}`
  );
}

rmSync(resolve(outDir, 'synapse'), { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

const result = spawnSync(
  flatc,
  ['--ts', '--gen-all', '-o', outDir, '-I', fbsDir, resolve(fbsDir, 'all.fbs')],
  { stdio: 'inherit' }
);

if (result.status !== 0) {
  process.exit(result.status ?? 1);
}
console.log(`Generated Synapse TS bindings into ${outDir}/synapse using ${flatc}`);

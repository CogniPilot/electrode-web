# electrode-web

`electrode-web` is CogniPilot's browser-based ground station and viewer for
Synapse/Zenoh systems. The same SvelteKit build runs as a static GitHub Pages
viewer and as the UI served by the local Rust ground-station daemon.

- Static Viewer: <https://cognipilot.github.io/electrode-web/>
- Local Ground Station: <http://127.0.0.1:8790/> after `npm run ground-station`

## What It Does

- Connects directly from the browser to Zenoh over WebSocket with
  `@cognipilot/zenoh-wasm`.
- Discovers and subscribes to Synapse topics, decodes known FlatBuffer payloads,
  and adapts them into a live vehicle state.
- Provides status, map/scene, RC/manual-control, simulation, topic discovery,
  plotting, command, event, MCAP log recording, and replay views.
- Serves as a static viewer when no local backend is present.
- Unlocks host hardware workflows when served by `electrode-ground-station`,
  including joystick discovery, RC mapping, PPM bridge control, simulation
  control, and autopilot profile management.
- Includes native Rust tools for ground-station serving, manual-control input,
  PPM output, fake simulation, and bridge-compatible testing.

## Repository Layout

```text
apps/web                       SvelteKit viewer and ground-station UI
packages/electrode-sdk         TypeScript SDK, state store, transport, replay, logs
packages/electrode-flatbuffers Pregenerated schema assets shared by the SDK
crates/electrode-ground-station Local daemon that serves the app and gcs/* APIs
crates/electrode-manual-control-bridge USB joystick to Synapse ManualControl
crates/electrode-ppm-bridge    Synapse manual/autopilot output to serial PPM
crates/electrode-fake-sim      Zenoh/Synapse fake vehicle publisher
docs                           Architecture, safety, lifecycle, and topic notes
```

## Development Environment

Use either your host tools or the Nix development shell.

```bash
nix develop
npm ci
npm run ci
```

The CI workflow uses the same flake with Determinate Nix and FlakeHub Cache, so
local `nix develop` and GitHub Actions resolve the same toolchain baseline.

Without Nix, install Node.js 24, Rust with `rustfmt` and `clippy`, and the Linux
`libudev` development package if building serial/joystick crates. The
FlatBuffers compiler is not required for normal development or CI; Synapse
schemas are consumed from the published `@cognipilot/synapse-fbs` and
`synapse_fbs` packages, with pregenerated TypeScript/schema assets committed in
this repo.

## Web Viewer

Run the static web app in development:

```bash
npm ci
npm run dev
```

Open the Vite URL printed by the command. In viewer mode the app is display-only
unless it can reach a Zenoh WebSocket endpoint. Static hosting does not expose
local `gcs/*` hardware APIs.

Build the static site:

```bash
npm run build
npm run verify:pages
```

The GitHub Actions workflow publishes `apps/web/build` to GitHub Pages on pushes
to `main`.

Recordings download as `.mcap` files using FlatBuffer schema records. Replay
loads MCAP recordings.

## Local Ground Station

Build the web app and start the local daemon:

```bash
npm run build
npm run ground-station
```

Open <http://127.0.0.1:8790/>. The daemon serves the same static app and answers
same-origin `gcs/*` requests for host capabilities. Add `?viewer` to force
display-only mode even when the daemon is present.

The daemon defaults to a local Zenoh hub that exposes:

```text
udp/127.0.0.1:7447    native tools and vehicle-side processes
ws/127.0.0.1:7447     browser Zenoh WASM transport
```

## Zenoh Connection

Browsers can only use Zenoh over WebSocket. Use `ws/127.0.0.1:7447` in the app,
or point it at another router/listener that exposes `ws/`.

For a standalone router:

```bash
zenohd -l udp/0.0.0.0:7447 -l ws/0.0.0.0:7447
```

Native tools in this repo default to UDP on port 7447. Override the locator with
the relevant environment variable or CLI flag when needed.

## Simulation And Hardware

Run the fake Synapse publisher when you want a live vehicle stream without
hardware:

```bash
npm run sim:fake
```

Run the manual-control bridge for a Linux joystick/gamepad:

```bash
npm run manual:bridge -- --device /dev/input/js0
```

Inspect the manual-control stream:

```bash
npm run manual:dump -- --topic synapse/v1/topic/manual_control_command
```

Run the PPM bridge for hardware-in-the-loop receiver output:

```bash
npm run ppm:bridge -- \
  --manual-topic synapse/v1/topic/manual_control_command \
  --control-output-topic synapse/v1/topic/pwm_signal_outputs \
  --serial-device /dev/ttyACM0
```

## Quality Gates

JavaScript and Rust checks are wired into CI:

```bash
npm run lint
npm run lint:rust-files
npm run test
npm run check
npm run build
npm run verify:pages

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

`npm run ci` runs the JavaScript lint, unit tests, type/Svelte checks, static
build, and Pages artifact verification.

Rust crates opt into workspace lints with `clippy.toml` thresholds for nesting,
function length, argument count, and type complexity. The Rust file length check
enforces the same 2000-line tracked-source limit used by the CI lint step.

## Developer Book

The developer notes in `docs/` build into a versioned mdBook site through the
Rust `xtask` driver:

```bash
cargo install mdbook --version "$(awk -F= '/^MDBOOK_VERSION=/{print $2}' tools.lock)" --locked
cargo run --locked --manifest-path xtask/Cargo.toml -- docs --version main --out-dir target/xtask/docs
```

Open `target/xtask/docs/main/index.html` for a local copy. GitHub Actions also
builds the book into `apps/web/build/dev-book` during the normal Pages build, so
pushes to `main` deploy the app and the developer book together at
`https://cognipilot.github.io/electrode-web/dev-book/`.

## Notes

- `@cognipilot/zenoh-wasm` is pinned from npm, not vendored from this repo.
- The current static build uses relative asset paths so one artifact works both
  at the GitHub Pages subpath and at the local daemon root.
- Hardware access remains daemon/native-tool responsibility; browser code only
  uses same-origin `gcs/*` APIs or Zenoh WebSocket transport.

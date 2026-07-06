# electrode-web Developer Guide

`electrode-web` is CogniPilot's browser-based ground station and viewer for
Synapse/Zenoh systems. This book gathers the design notes that explain how the
web app, TypeScript SDK, Rust ground-station daemon, native bridge tools, logs,
and replay path fit together.

The book is generated the same way as the `synapse_fbs` docs: a pinned mdBook
version in `tools.lock`, a Rust `xtask` command, versioned output directories,
and a generated version selector for published snapshots.

## Local Build

```sh
cargo install mdbook --version "$(awk -F= '/^MDBOOK_VERSION=/{print $2}' tools.lock)" --locked
cargo run --locked --manifest-path xtask/Cargo.toml -- docs --version main --out-dir target/xtask/docs
```

Open `target/xtask/docs/main/index.html` after the build completes.

## Source Map

- `apps/web` contains the SvelteKit app used by static hosting and the local
  ground-station daemon.
- `packages/electrode-sdk` contains the TypeScript state, decode, replay,
  plotting, and simulator-facing SDK.
- `crates/electrode-ground-station` serves the app locally and owns host-side
  `gcs/*` APIs.
- Native Rust bridge crates connect joystick, PPM, fake simulation, and logging
  workflows to Zenoh/Synapse.

Start with [Using The UI](ui-guide.md) if you are trying to operate the app or
explain the screen to someone else.

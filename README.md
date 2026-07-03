# electrode-web

`electrode-web` is the web-first ground station workspace for the `electrode` project. It follows the Cerebri GCS roadmap with a browser UI, a Rust/WASM protocol core, a TypeScript SDK, FlatBuffer schemas, and a native Rust bridge that exposes a browser-safe WebSocket endpoint.

## Current MVP

- SvelteKit web app with connection, status, map, commands, replay, events, plots, and topic inspector panels.
- TypeScript SDK with topic registry, latest-wins state store, command preconditions, Synapse `.sylg` log recording/replay, Zenoh WASM publishing, and transport types.
- Web Worker that runs the Zenoh, bridge, and replay pipelines off the UI thread.
- Rust `electrode-web-core` crate for shared message validation and future WASM exports.
- Browser `@cognipilot/zenoh-wasm` integration for direct command-intent publishing to a Zenoh WebSocket endpoint.
- Rust `electrode-ground-bridge` crate that subscribes to live Synapse FlatBuffers on Zenoh, discovers published keys, decodes known topics (FlightSnapshot/ManualControl/MotorOutput), and forwards selected topics to the browser over WebSocket.
- Rust `electrode-manual-control-bridge` crate for Linux joystick/gamepad input to Synapse `ManualControl` over Zenoh.
- Rust `electrode-ppm-bridge` crate for Synapse manual/autopilot outputs to serial PPM encoder output.
- FlatBuffer schema files, BFBS schema assets, and generator script hooks.

## Quick Start

```bash
npm install
cargo build
npm run build
npm run dev
```

Run the bridge in another terminal when using bridge mode:

```bash
npm run bridge
```

The bridge subscribes to a live Zenoh network and reports every discovered key to
the **Discovery** panel, where you toggle which topics to stream. Decodable
Synapse topics are auto-selected on first sight. Configure it with:

```bash
ELECTRODE_ZENOH_CONNECT=udp/127.0.0.1:7447 \  # Zenoh router/peer locator
ELECTRODE_ZENOH_KEYEXPR="synapse/**" \        # discovery key expression
ELECTRODE_ZENOH_AUTOSELECT=1 \                # auto-stream decodable topics
  npm run bridge
```

To exercise the full path without vehicle hardware or a running sim, start the
bundled fake sim. It models the real network as two publishers — a **mocap**
source (pose on `synapse/mocap/frame`) and an **autopilot** source
(`synapse/flight_snapshot` + `synapse/motor_output`) — flying a coordinated-turn
loiter, then run the bridge against `udp/127.0.0.1:7447`:

```bash
npm run sim:fake                       # both publishers (default)
npm run sim:fake -- --role mocap       # or run them as two processes
npm run sim:fake -- --role autopilot
```

The sim defaults to Zenoh `peer` mode acting as the hub. It listens on **two**
locators on port 7447: `udp/127.0.0.1:7447` for native components and
`ws/127.0.0.1:7447` for the browser (a browser can only speak WebSocket). Every
native Zenoh client in this repo defaults to the `udp/…:7447` locator and the
web app connects over `ws/…:7447`, so they all reach the same hub with no extra
config.

Run the native manual-control bridge when using a Linux joystick/gamepad:

```bash
npm run manual:bridge -- --device /dev/input/js0
```

Inspect the published manual-control stream:

```bash
npm run manual:dump -- --topic synapse/manual_control
```

Run the native PPM bridge when using hardware-in-the-loop receiver output:

```bash
npm run ppm:bridge -- \
  --manual-topic synapse/v1/topic/manual_control_command \
  --control-output-topic synapse/v1/topic/pwm_signal_outputs \
  --serial-device /dev/ttyACM0
```

The web app connects directly over Zenoh from the browser (compiled WASM client)
and can only use WebSocket. Any Zenoh hub it connects to must therefore offer a
`ws/` listener — the bundled fake sim (and the ground-bridge in `peer` mode) do
this automatically alongside `udp`. To use a standalone router instead, run one
with a WebSocket listener:

```bash
zenohd -l udp/0.0.0.0:7447 -l ws/0.0.0.0:7447
```

Then in the app select `zenoh` mode and use `ws/127.0.0.1:7447`. To run without
vehicle hardware, start the bundled fake sim (`npm run sim:fake`) as the hub, or
use replay mode.

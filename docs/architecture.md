# electrode Architecture

`electrode` uses one UI codebase and one protocol boundary across browser, replay, and native bridge modes.

```text
Vehicle / simulator
  -> Zenoh + FlatBuffers
  -> electrode-ground-bridge or zenoh-wasm
  -> WebSocket bridge or Zenoh WebSocket transport
  -> electrode web app + worker + SDK
```

The browser owns operator interaction, visualization, constrained command intent, replay, and diagnostics. It never owns hardware access, raw actuator authority, or safety-critical watchdogs.

The browser can publish command intent directly through `@cognipilot/zenoh-wasm` when connected to a Zenoh router WebSocket endpoint. The current published package exposes async session open and put APIs but no subscriber API, so live telemetry is received through the native `electrode-ground-bridge`: it subscribes to `synapse/**`, tracks a discovery catalog of every observed key (rate, size, decodability), decodes the Synapse tables it understands into GCS frames, and forwards operator-selected topics to the browser over WebSocket. Simulator and replay remain available as offline sources.

The native bridge owns Zenoh connectivity, allowlists, command sequence checks, stale command rejection, local logging, telemetry forwarding, and future hardware integration.

The Rust/WASM core owns message validation, schema-version checks, unit conversion, stale-message checks, and future FlatBuffer encode/decode shared with native tools.

## Physical Deployment

Electrode is the ground-station client and should not absorb every edge bridge on the network. Hardware-adjacent bridges stay with the machine that owns that hardware, then publish or route typed Zenoh topics for clients that request them.

```text
Mocap Windows computer
  -> Qualisys QTM
  -> synapse-qualisys-bridge
  -> Zenoh router/listener: udp,tcp/<mocap-pc-ip>:7447
  -> synapse/mocap/**

Ground-station computer
  -> electrode web app
  -> @cognipilot/zenoh-wasm or electrode-ground-bridge
  -> subscribes only to required Zenoh key expressions

Flight-control computer
  -> Zephyr native_sim, QEMU Cerebri, NUC-hosted autopilot, or USB/FTDI-connected Cerebri hardware
  -> Synapse topics for vehicle state, commands, manual control, and actuator/control outputs
```

The Qualisys bridge belongs on the mocap Windows computer because it owns the QTM connection, QTM component selection, Windows installer/update flow, and embedded Zenoh router/listener. Electrode should discover or configure its endpoint and consume `synapse/mocap/**` topics, not vendor the bridge runtime into the GCS app.

This keeps traffic shaping at the correct boundary: the mocap bridge publishes selected motion-capture streams into Zenoh, and Zenoh routes only the key expressions requested by downstream clients instead of forcing every ground-station client to receive a raw QTM UDP stream.

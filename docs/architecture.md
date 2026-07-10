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

The command authority observes `synapse/**` on its trusted vehicle session and publishes throttled, payload-free topic announcements to the browser session. This lets the topic browser discover every Synapse key on the LAN. Normal vehicle telemetry and the `cub1` compact pose remain subscribed by default; other payloads cross into the browser only after the operator selects their announced key.

## Mocap Wire Contract

Every mocap producer — the Qualisys bridge on real hardware, and the Ground Station's sim bridge when a plant (in-browser Rumoca WASM or the native sim executable) is running — publishes the same two data streams, so downstream consumers use one wire contract for simulation and live capture:

| Topic | Payload |
| --- | --- |
| `synapse/mocap/frame` | `synapse.topic.MocapFrame` FlatBuffer (rigid bodies, markers, timing) |
| `synapse/mocap/rigid_body/<name>/pose` | Compact 28 bytes: little-endian f32 `[px, py, pz, qx, qy, qz, qw]` |

Conventions (synapse_fbs 0.5.0): `MocapFrame` rigid bodies carry ENU positions, a body-to-ENU rotation matrix, residuals in millimetres, and validity flags. The compact pose derives a normalized quaternion from that matrix and carries its scalar component **last**. Producers must deliver an FLU-aligned body frame — in QTM that means defining the rigid body with X forward, Y left, Z up; consumers apply no per-body corrections.

Sim plants publish `MocapFrame` FlatBuffers on the private `electrode/sim/rumoca/mocap_frame` topic only. When simulation is selected, the trusted vehicle-side Ground Station bridge verifies each frame and publishes the public frame plus `synapse/mocap/rigid_body/cub1/pose`. When real mocap is selected, sim publication is suppressed and the external capture bridge owns that public pose. The autopilot link forwards the chosen public pose to cubs2, whose `csyn_decode_mocap_frame` accepts both the compact pose and the full FlatBuffer.

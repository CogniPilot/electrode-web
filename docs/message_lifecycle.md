# Message Lifecycle

Live telemetry and replay data use the same envelope:

```text
topic
header
payload
```

The header includes sequence number, source time, receive time, expiration time, vehicle id, schema version, message type, priority, and stream id.

Receiver policy:

- Reject unknown schema versions.
- Drop expired messages.
- Drop older messages from the same stream.
- Mark topics stale when their timeout is exceeded.
- Render latest state, not every high-rate sample.

Replay feeds recorded envelopes into the same worker and state store used by live bridge data.

## Logs

Ground-station recordings use MCAP files with FlatBuffer schema records. Live
Synapse topic schemas come from the published `@cognipilot/synapse-fbs` and
`synapse_fbs` packages, which ship pregenerated `.bfbs` reflection schemas; the
FlatBuffers compiler is not part of normal development or CI.

The browser exports `.mcap` files with:

- MCAP profile `synapse`
- `flatbuffer` schema encoding and message encoding
- BFBS schema data for `electrode.gcs.GcsFrame`
- one channel per captured Zenoh topic
- `electrode.recording` metadata for source, description, and creation time

Each browser-recorded message payload is an `electrode.gcs.GcsFrame` FlatBuffer
with file identifier `EGCS`. `GcsFrame` wraps the typed state/event payloads in a
FlatBuffer union, so replay does not depend on JSON. The replay loader can also
read native Synapse MCAP channels when their payloads match the SDK's current
Synapse decoder.

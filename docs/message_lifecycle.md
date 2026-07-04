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

Ground-station recordings use committed schema assets from
`@electrode/flatbuffers`. Live Synapse topic schemas come from the published
`@cognipilot/synapse-fbs` and `synapse_fbs` packages.

The browser exports `.sylg` files as a stream of size-prefixed `synapse.log.LogRecord`
FlatBuffers with file identifier `SYLG`. Each file starts with:

- `LogFileHeader`
- `SchemaRecord` for the Synapse log schema, including BFBS bytes
- `SchemaRecord` for `electrode.gcs.GcsFrame`, including BFBS bytes
- `TopicRecord` entries for every topic captured in the session

Each data sample is then a `LogFrame` whose payload is an `electrode.gcs.GcsFrame`
FlatBuffer with file identifier `EGCS`. `GcsFrame` wraps the typed state/event
payloads in a FlatBuffer union, so replay does not depend on JSON.

# Electrode command authority

This crate separates four Zenoh trust domains with multicast discovery
disabled:

- the trusted GCS website on `ws/127.0.0.1:7447`;
- checked LAN command requests on `ws/0.0.0.0:7448`;
- a connect-only LAN telemetry client for the Qualisys router; and
- the autopilot router on `udp/127.0.0.1:7447`.

Local website commands use the typed target mapping but bypass LAN value
policy. LAN command requests use `CommandPolicy`. LAN telemetry is allowlisted
and validated before it is copied into the trusted browser or vehicle domains.

The checked LAN protocol accepts team-scoped velocity (`EVC1`) and budget-query
(`EVB1`) envelopes from the standalone website. Each normalized team name has
five persistent velocity attempts. CSV is authoritative, JSON is an inspection
mirror, and status is returned on `gcs/v1/status/velocity`. This quota is not an
authentication mechanism; team names are self-asserted.

Accepted typed intents:

- `gcs/v1/cmd/velocity` -> `pos_sp`
- `gcs/v1/cmd/manual` -> `manual`
- `gcs/v1/cmd/radio` -> `rc`
- `gcs/v1/cmd/gain` -> `cmd/param_set` query
- `gcs/v1/cmd/parameters` -> `cmd/param_get` query
- `gcs/v1/cmd/trajectory` -> `cmd/trajectory_set` query

`gcs/v1/cmd/raw/<leaf>` preserves the Packet Traffic prototype feature. The
leaf must be one safe segment and each payload is bounded to 4 KiB. Explicitly
selected typed leaves are allowed; the bytes are forwarded exactly and are not
claimed to be a schema-verified FlatBuffer. Repeat and interval are controlled
by the website.

Vehicle telemetry is relayed one way to the trusted local browser. LAN
`MocapFrame`, rigid-body names, and external-odometry streams are validated and
relayed from the dedicated telemetry client. Private Rumoca `MocapFrame` data
is accepted only from the trusted local browser domain.

## Firmware updates

The browser's staged namespace is
`gcs/v1/cmd/firmware/<update-id>/{start,chunk/<index>,commit}`. The vehicle-side
query keys are `cmd/firmware_{info,status,prepare,chunk,commit,abort}`.
The request and reply payloads use the generated firmware bindings exported by
`synapse_fbs` 0.5.1. This crate assembles and hashes the browser upload,
compares it with a trusted baseline,
allows differences only inside configured gain windows, and only then performs
the vehicle query transfer with chunk retries. Progress is published on
`gcs/v1/status/firmware/<update-id>`.

Set `ELECTRODE_GCS_FIRMWARE_BASELINE` to the trusted binary and
`ELECTRODE_GCS_GAIN_WINDOWS_PATH` to the gain-window JSON. The CUBS2 prototype
artifact paths are detected when present. First-upload baseline bootstrapping
is disabled unless `ELECTRODE_GCS_FIRMWARE_AUTOBOOTSTRAP=true` is explicitly
set.

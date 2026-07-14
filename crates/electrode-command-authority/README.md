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
(`EVB1`) envelopes from the standalone website. An `EVC1` body must be a
`ParamSetRequest` for exactly the floating-point parameter
`velocity.setpoint`, within 1-4 m/s. The envelope is removed and the canonical
request is sent as a `cmd/param_set` query. Each normalized team name has five
persistent velocity attempts. CSV is authoritative, JSON is an inspection
mirror, and query success or failure is returned with budget metadata on
`gcs/v1/status/velocity`. This quota is not an authentication mechanism; team
names are self-asserted.

Accepted typed intents:

- `gcs/v1/cmd/velocity` -> `cmd/param_set` query for `velocity.setpoint`
- `gcs/v1/cmd/radio` -> `rc`
- `gcs/v1/cmd/gain` -> allowlisted `cmd/param_set` query (velocity aliases are excluded)
- `gcs/v1/cmd/parameters` -> `cmd/param_get` query
- `gcs/v1/cmd/trajectory` -> `cmd/trajectory_set` query

Vehicle telemetry is relayed one way to the trusted local browser. LAN
`MocapFrame`, rigid-body names, and external-odometry streams are validated and
relayed from the dedicated telemetry client. Private Rumoca `MocapFrame` data
is accepted only from the trusted local browser domain.

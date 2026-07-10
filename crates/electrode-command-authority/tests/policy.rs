use electrode_command_authority::{
    CommandAuthorityConfig, CommandPolicy, Delivery, PolicyConfig, CANONICAL_FIRMWARE_QUERY_KEYS,
};
use flatbuffers::FlatBufferBuilder;
use synapse_fbs::cmd::{
    ParamKind, ParamSetRequest, ParamSetRequestArgs, ParamValue, ParamValueArgs,
};

fn policy() -> CommandPolicy {
    CommandPolicy::new(PolicyConfig::default())
}

#[test]
fn default_runtime_uses_distinct_browser_and_vehicle_transports() {
    let config = CommandAuthorityConfig::default();
    assert!(config.browser_listen.starts_with("ws/"));
    assert!(config.vehicle_listen.starts_with("udp/"));
    assert_ne!(config.browser_listen, config.vehicle_listen);
}

fn velocity(x: f32, y: f32, z: f32) -> Vec<u8> {
    let mut payload = vec![0_u8; 56];
    payload[20..24].copy_from_slice(&x.to_le_bytes());
    payload[24..28].copy_from_slice(&y.to_le_bytes());
    payload[28..32].copy_from_slice(&z.to_le_bytes());
    payload[52..54].copy_from_slice(&3527_u16.to_le_bytes());
    payload
}

fn manual(flags: u8) -> Vec<u8> {
    let mut payload = vec![0_u8; 40];
    payload[12..14].copy_from_slice(&15_u16.to_le_bytes());
    payload[18..20].copy_from_slice(&700_i16.to_le_bytes());
    payload[35] = flags;
    payload
}

fn radio(channel: u16) -> Vec<u8> {
    let mut payload = vec![0_u8; 48];
    payload[8] = 5;
    payload[9] = 100;
    for index in 0..5 {
        let offset = 10 + index * 2;
        payload[offset..offset + 2].copy_from_slice(&channel.to_le_bytes());
    }
    payload
}

fn parameter(name: &str, value: f64) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let name = builder.create_string(name);
    let parameter = ParamValue::create(
        &mut builder,
        &ParamValueArgs {
            name: Some(name),
            kind: ParamKind::Float,
            float_value: value,
            ..Default::default()
        },
    );
    let request = ParamSetRequest::create(
        &mut builder,
        &ParamSetRequestArgs {
            value: Some(parameter),
        },
    );
    builder.finish(request, None);
    builder.finished_data().to_vec()
}

#[test]
fn maps_only_valid_x_velocity_and_enforces_budget() {
    let policy = policy();
    let command = policy
        .authorize("gcs/v1/cmd/velocity", &velocity(2.5, 0.0, 0.0))
        .unwrap();
    assert_eq!(command.delivery, Delivery::Publish);
    assert_eq!(command.target, "synapse/v1/topic/local_position_command");
    assert_eq!(command.velocity_remaining, Some(4));
    assert!(policy
        .authorize("gcs/v1/cmd/velocity", &velocity(2.5, 0.1, 0.0))
        .is_err());
    for _ in 0..4 {
        policy
            .authorize("gcs/v1/cmd/velocity", &velocity(1.0, 0.0, 0.0))
            .unwrap();
    }
    assert!(policy
        .authorize("gcs/v1/cmd/velocity", &velocity(1.0, 0.0, 0.0))
        .is_err());
    assert_eq!(policy.velocity_remaining("default"), 0);
}

#[test]
fn typed_manual_and_radio_have_exact_mappings_and_ranges() {
    let policy = policy();
    assert_eq!(
        policy
            .authorize("gcs/v1/cmd/manual", &manual(12))
            .unwrap()
            .target,
        "synapse/v1/topic/manual_control_command"
    );
    assert!(policy.authorize("gcs/v1/cmd/manual", &manual(0)).is_err());
    assert_eq!(
        policy
            .authorize("gcs/v1/cmd/radio", &radio(1500))
            .unwrap()
            .target,
        "synapse/v1/topic/radio_control"
    );
    assert!(policy.authorize("gcs/v1/cmd/radio", &radio(899)).is_err());
    assert!(policy.authorize("gcs/v1/cmd/radio", &radio(2101)).is_err());
}

#[test]
fn gain_is_schema_verified_and_allowlisted() {
    let policy = policy();
    let command = policy
        .authorize("gcs/v1/cmd/gain", &parameter("attitude.headingPid.kp", 1.2))
        .unwrap();
    assert_eq!(command.delivery, Delivery::Query);
    assert_eq!(command.target, "synapse/v1/cmd/param_set");
    assert!(policy
        .authorize(
            "gcs/v1/cmd/gain",
            &parameter("attitude.headingPid.trim", 1.0),
        )
        .is_err());
    assert!(policy
        .authorize(
            "gcs/v1/cmd/config",
            &parameter("attitude.headingPid.kp", 1.2),
        )
        .is_err());
}

#[test]
fn raw_path_is_one_bounded_leaf_and_preserves_selected_topics() {
    let policy = policy();
    assert_eq!(
        policy
            .authorize("gcs/v1/cmd/raw/text_status", b"operator bytes")
            .unwrap()
            .target,
        "synapse/v1/topic/text_status"
    );
    let payload = (0_u8..40).collect::<Vec<_>>();
    let command = policy
        .authorize("gcs/v1/cmd/raw/manual_control_command", &payload)
        .unwrap();
    assert_eq!(command.target, "synapse/v1/topic/manual_control_command");
    assert_eq!(command.payload, payload);
    assert!(policy
        .authorize("gcs/v1/cmd/raw/nested/channel", &[0; 16])
        .is_err());
    assert!(policy
        .authorize("gcs/v1/cmd/raw/text_status", &vec![0; 4097])
        .is_err());
}

#[test]
fn firmware_upload_uses_staged_authority_delivery() {
    let policy = policy();
    assert_eq!(
        PolicyConfig::default().firmware_key_prefix,
        "synapse/v1/cmd/firmware"
    );
    assert!(CANONICAL_FIRMWARE_QUERY_KEYS.contains(&"synapse/v1/cmd/firmware_abort"));
    let command = policy
        .authorize("gcs/v1/cmd/firmware/update-1/start", &[0; 32])
        .unwrap();
    assert_eq!(command.delivery, Delivery::Firmware);
    assert_eq!(command.target, "update-1/start");
    assert_eq!(command.status_leaf, "firmware/update-1");
    assert_eq!(command.payload, vec![0; 32]);
    assert!(policy
        .authorize("gcs/v1/cmd/firmware/update-1/chunk/2", &[1; 64])
        .is_ok());
    assert!(policy
        .authorize(
            "gcs/v1/cmd/firmware/update-1/chunk/2",
            &vec![1; 68 * 1024 + 1]
        )
        .is_err());
    assert!(policy
        .authorize("gcs/v1/cmd/firmware/../commit", &[0; 32])
        .is_err());
    assert!(policy
        .authorize("gcs/v1/cmd/firmware_info", &[0; 32])
        .is_err());
}

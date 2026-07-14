//! Pure command decoding, validation, and key mapping.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::Deserialize;
use synapse_fbs::cmd::{ParamGetRequest, ParamKind, ParamSetRequest, TrajectorySetRequest};
use synapse_fbs::topic::{LocalPositionCommandData, ManualControlData, RadioControlData};

use crate::velocity_budget::{BudgetState, Credential, VelocityBudgetStore};

const VELOCITY_ONLY_MASK: u16 = 3527;
const LOCAL_ENU_FRAME: u8 = 0;
const MANUAL_ACTIVE_AXES_MASK: u16 = 0x03ff;
const MANUAL_FLAG_ACTIVE: u8 = 4;
const MANUAL_FLAG_VALID: u8 = 8;
const FIRMWARE_PREPARE_MAX_BYTES: usize = 4 * 1024;
const FIRMWARE_CHUNK_MAX_BYTES: usize = 68 * 1024;
const FIRMWARE_COMMIT_MAX_BYTES: usize = 2 * 1024;
const VELOCITY_MAGIC: &[u8; 4] = b"EVC1";
const VELOCITY_BUDGET_MAGIC: &[u8; 4] = b"EVB1";
const RAW_VELOCITY_MAGIC: &[u8; 4] = b"EVR1";
const VELOCITY_PAYLOAD_BYTES: usize = 56;
const TEAM_NAME_MAX_BYTES: usize = 64;
const PARAMETER_LIMITS_SCHEMA: &str = "electrode.public-lan-parameter-limits.v1";
const DEFAULT_PARAMETER_LIMITS: &str = include_str!("../config/public-lan-parameter-limits.json");
const PUBLIC_PARAMETER_NAMES: [&str; 7] = [
    "velocity.setpoint",
    "route.crossTrackSteeringDistance",
    "route.waypointSwitchingDistance",
    "attitude.rollLimit",
    "attitude.headingPid.kp",
    "attitude.headingPid.ki",
    "attitude.headingPid.kd",
];
/// Canonical vehicle query keys used by the staged firmware-update transfer.
pub const CANONICAL_FIRMWARE_QUERY_KEYS: [&str; 6] = [
    "cmd/firmware_info",
    "cmd/firmware_status",
    "cmd/firmware_prepare",
    "cmd/firmware_chunk",
    "cmd/firmware_commit",
    "cmd/firmware_abort",
];

/// Whether the authorized payload is a Zenoh publication or request/reply query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Publish,
    Query,
    Firmware,
    Budget,
}

/// The only data the runtime needs after policy authorization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedCommand {
    pub delivery: Delivery,
    pub target: String,
    pub payload: Vec<u8>,
    pub status_leaf: String,
    pub velocity_device: Option<String>,
    pub velocity_remaining: Option<u32>,
    pub velocity_credential_id: Option<String>,
    pub velocity_limit: Option<u32>,
    pub velocity_used: Option<u32>,
    pub velocity_budget_version: Option<String>,
    /// Synapse value-contract encoding stamped on the outbound Zenoh value.
    /// None for raw targets outside the catalog.
    pub encoding: Option<String>,
}

/// Policy settings independent of Zenoh transport configuration.
#[derive(Clone, Debug)]
pub struct PolicyConfig {
    pub intent_prefix: String,
    pub vehicle_topic_prefix: String,
    pub parameter_key: String,
    pub firmware_key_prefix: String,
    pub velocity_min_mps: f32,
    pub velocity_max_mps: f32,
    pub velocity_budget: u32,
    pub velocity_budget_json: PathBuf,
    pub velocity_budget_csv: PathBuf,
    pub raw_max_bytes: usize,
    pub parameter_limits_path: Option<PathBuf>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            intent_prefix: "gcs/v1/cmd".to_string(),
            // synapse_fbs 0.6.0 compact keys: the vehicle prefix is the
            // deployment namespace prepended to topic/command keys. csyn
            // publishes bare catalog keys, so the default is empty.
            vehicle_topic_prefix: String::new(),
            parameter_key: "cmd/param_set".to_string(),
            firmware_key_prefix: "cmd/firmware".to_string(),
            velocity_min_mps: 1.0,
            velocity_max_mps: 4.0,
            velocity_budget: 5,
            velocity_budget_json: PathBuf::from("data/velocity-budget-db.json"),
            velocity_budget_csv: PathBuf::from("data/velocity-budget.csv"),
            raw_max_bytes: 4 * 1024,
            parameter_limits_path: None,
        }
    }
}

/// A rejected browser intent. Rejected bytes are never exposed to the vehicle session.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("intent key is outside the command namespace")]
    WrongNamespace,
    #[error("unknown command intent {0:?}")]
    UnknownIntent(String),
    #[error("invalid command payload: {0}")]
    InvalidPayload(String),
    #[error("command rejected by policy: {0}")]
    Rejected(String),
}

/// Stateful policy engine. Only accepted velocity commands consume a budget entry.
pub struct CommandPolicy {
    config: PolicyConfig,
    velocity_budget: Mutex<VelocityBudgetStore>,
    parameter_limits: HashMap<String, (f64, f64)>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ParameterLimitsFile {
    schema: String,
    parameters: Vec<ParameterLimit>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ParameterLimit {
    name: String,
    minimum: f64,
    maximum: f64,
}

impl CommandPolicy {
    #[must_use]
    pub fn new(config: PolicyConfig) -> Self {
        let velocity_budget = VelocityBudgetStore::new(
            config.velocity_budget_json.clone(),
            config.velocity_budget_csv.clone(),
            config.velocity_budget,
        );
        let parameter_limits = load_parameter_limits(config.parameter_limits_path.as_deref());
        Self {
            config,
            velocity_budget: Mutex::new(velocity_budget),
            parameter_limits,
        }
    }

    pub fn authorize(&self, key: &str, payload: &[u8]) -> Result<AuthorizedCommand, PolicyError> {
        let prefix = format!("{}/", self.config.intent_prefix.trim_end_matches('/'));
        let suffix = key
            .strip_prefix(&prefix)
            .ok_or(PolicyError::WrongNamespace)?;
        match suffix {
            "velocity" => self.authorize_velocity(payload),
            "velocity_budget" => self.authorize_velocity_budget(payload),
            "manual" => self.authorize_manual(payload),
            "radio" => self.authorize_radio(payload),
            "gain" => self.authorize_gain(payload),
            "parameters" => self.authorize_parameters(payload),
            "trajectory" => self.authorize_trajectory(payload),
            raw if raw.starts_with("raw/") => self.authorize_raw(&raw[4..], payload),
            firmware if firmware.starts_with("firmware/") => {
                self.authorize_firmware(&firmware[9..], payload)
            }
            other => Err(PolicyError::UnknownIntent(other.to_string())),
        }
    }

    /// Map commands from the localhost-only GCS website without applying the
    /// LAN policy limits. The autopilot remains responsible for decoding the
    /// payload; this boundary only constrains the command namespace and target.
    pub fn authorize_trusted(
        &self,
        key: &str,
        payload: &[u8],
    ) -> Result<AuthorizedCommand, PolicyError> {
        let prefix = format!("{}/", self.config.intent_prefix.trim_end_matches('/'));
        let suffix = key
            .strip_prefix(&prefix)
            .ok_or(PolicyError::WrongNamespace)?;
        let query = |target: &str, command_name: &str, status_leaf: &str| AuthorizedCommand {
            delivery: Delivery::Query,
            target: target.to_string(),
            payload: payload.to_vec(),
            status_leaf: status_leaf.to_string(),
            velocity_device: None,
            velocity_remaining: None,
            velocity_credential_id: None,
            velocity_limit: None,
            velocity_used: None,
            velocity_budget_version: None,
            encoding: command_request_encoding(command_name),
        };
        match suffix {
            "gain" => Ok(query(&self.config.parameter_key, "param_set", "gain")),
            "parameters" => Ok(query(
                &self.vehicle_command_key("param_get"),
                "param_get",
                "parameters",
            )),
            "trajectory" => Ok(query(
                &self.vehicle_command_key("trajectory_set"),
                "trajectory_set",
                "trajectory",
            )),
            "velocity" => Ok(self.publish_topic("pos_sp", payload, "velocity", None)),
            "manual" => Ok(self.publish_topic("manual", payload, "manual", None)),
            "radio" => Ok(self.publish_topic("rc", payload, "radio", None)),
            raw if raw.starts_with("raw/") => self.authorize_raw(&raw[4..], payload),
            firmware if firmware.starts_with("firmware/") => {
                self.authorize_firmware(&firmware[9..], payload)
            }
            other => Err(PolicyError::UnknownIntent(other.to_string())),
        }
    }

    pub(crate) fn refund_velocity(
        &self,
        device: &str,
        credential_id: &str,
    ) -> Result<BudgetState, PolicyError> {
        self.velocity_budget
            .lock()
            .expect("velocity budget lock poisoned")
            .refund(&Credential {
                device_id: device.to_string(),
                credential_id: credential_id.to_string(),
            })
            .map_err(PolicyError::Rejected)
    }

    pub(crate) fn velocity_state_for_payload(
        &self,
        payload: &[u8],
    ) -> Result<BudgetState, PolicyError> {
        let team = team_name_from_any_velocity_envelope(payload)?;
        let store = self
            .velocity_budget
            .lock()
            .expect("velocity budget lock poisoned");
        let credential = store.resolve(&team).map_err(PolicyError::Rejected)?;
        store.state(&credential).map_err(PolicyError::Rejected)
    }

    pub(crate) fn credential_id_for_payload(payload: &[u8]) -> Option<String> {
        team_name_from_any_velocity_envelope(payload)
            .ok()
            .map(|team| crate::velocity_budget::credential_id(&team))
    }

    fn authorize_velocity(&self, payload: &[u8]) -> Result<AuthorizedCommand, PolicyError> {
        let (team, vehicle_payload) =
            credential_envelope(payload, VELOCITY_MAGIC, Some(VELOCITY_PAYLOAD_BYTES))?;
        let command = follow_struct::<LocalPositionCommandData>(vehicle_payload, 56)?;
        let velocity = command.velocity_enu_m_s();
        let (x, y, z) = (velocity.x(), velocity.y(), velocity.z());
        if !x.is_finite() || !y.is_finite() || !z.is_finite() {
            return rejected("velocity components must be finite");
        }
        if y != 0.0 || z != 0.0 {
            return rejected("velocity must be X-only; Y and Z must be zero");
        }
        if command.type_mask() != VELOCITY_ONLY_MASK
            || command.coordinate_frame().0 != LOCAL_ENU_FRAME
        {
            return rejected("velocity must use the Local ENU velocity-only shape");
        }
        if x < self.config.velocity_min_mps || x > self.config.velocity_max_mps {
            return rejected(format!(
                "velocity {x:.3} m/s is outside [{:.3}, {:.3}]",
                self.config.velocity_min_mps, self.config.velocity_max_mps
            ));
        }
        let state = self.consume_velocity_budget(&team, Some(x))?;
        let mut command = self.publish_topic(
            "pos_sp",
            vehicle_payload,
            "velocity",
            Some(state.device_id.clone()),
        );
        apply_budget_state(&mut command, &state);
        Ok(command)
    }

    fn authorize_velocity_budget(&self, payload: &[u8]) -> Result<AuthorizedCommand, PolicyError> {
        let (team, _) = credential_envelope(payload, VELOCITY_BUDGET_MAGIC, Some(0))?;
        let store = self
            .velocity_budget
            .lock()
            .expect("velocity budget lock poisoned");
        let credential = store.resolve(&team).map_err(PolicyError::Rejected)?;
        let state = store.state(&credential).map_err(PolicyError::Rejected)?;
        let mut command = self.publish_topic("", &[], "velocity", Some(state.device_id.clone()));
        command.delivery = Delivery::Budget;
        command.target.clear();
        apply_budget_state(&mut command, &state);
        Ok(command)
    }

    fn authorize_manual(&self, payload: &[u8]) -> Result<AuthorizedCommand, PolicyError> {
        let command = follow_struct::<ManualControlData>(payload, 40)?;
        let axes = [
            command.pitch_milli(),
            command.roll_milli(),
            command.throttle_milli(),
            command.yaw_milli(),
            command.aux0_milli(),
            command.aux1_milli(),
            command.aux2_milli(),
            command.aux3_milli(),
            command.aux4_milli(),
            command.aux5_milli(),
        ];
        if axes.iter().any(|value| !(-1000..=1000).contains(value)) {
            return rejected("manual axes must be within [-1000, 1000]");
        }
        if command.active_axes() & !MANUAL_ACTIVE_AXES_MASK != 0 {
            return rejected("manual active_axes contains unknown bits");
        }
        let required = MANUAL_FLAG_ACTIVE | MANUAL_FLAG_VALID;
        if command.flags() & required != required {
            return rejected("manual command must be active and valid");
        }
        Ok(self.publish_topic("manual", payload, "manual", None))
    }

    fn authorize_radio(&self, payload: &[u8]) -> Result<AuthorizedCommand, PolicyError> {
        let command = follow_struct::<RadioControlData>(payload, 48)?;
        let count = usize::from(command.channel_count());
        if !(5..=18).contains(&count) {
            return rejected("radio channel_count must be within [5, 18]");
        }
        if !(1..=100).contains(&command.link_quality_pct()) {
            return rejected("radio link quality must be within [1, 100]");
        }
        for index in 0..count {
            let offset = 10 + index * 2;
            let value = u16::from_le_bytes([payload[offset], payload[offset + 1]]);
            if !(900..=2100).contains(&value) {
                return rejected(format!("radio channel {index} is outside [900, 2100] us"));
            }
        }
        Ok(self.publish_topic("rc", payload, "radio", None))
    }

    fn authorize_gain(&self, payload: &[u8]) -> Result<AuthorizedCommand, PolicyError> {
        let request = flatbuffers::root::<ParamSetRequest<'_>>(payload)
            .map_err(|error| invalid(error.to_string()))?;
        let value = request
            .value()
            .ok_or_else(|| invalid("ParamSetRequest has no value"))?;
        let name = value.name().unwrap_or("").trim();
        if name.is_empty() || name.len() > 128 {
            return rejected("parameter name must contain 1 to 128 characters");
        }
        validate_gain(
            &self.parameter_limits,
            name,
            value.kind(),
            value.float_value(),
        )?;
        Ok(AuthorizedCommand {
            delivery: Delivery::Query,
            target: self.config.parameter_key.clone(),
            payload: payload.to_vec(),
            status_leaf: "gain".to_string(),
            velocity_device: None,
            velocity_remaining: None,
            velocity_credential_id: None,
            velocity_limit: None,
            velocity_used: None,
            velocity_budget_version: None,
            encoding: command_request_encoding("param_set"),
        })
    }

    fn authorize_parameters(&self, payload: &[u8]) -> Result<AuthorizedCommand, PolicyError> {
        let request = flatbuffers::root::<ParamGetRequest<'_>>(payload)
            .map_err(|error| invalid(error.to_string()))?;
        let name = request.name().unwrap_or("").trim();
        if name.is_empty() || name.len() > 128 {
            return rejected("parameter name must contain 1 to 128 characters");
        }
        Ok(AuthorizedCommand {
            delivery: Delivery::Query,
            target: self.vehicle_command_key("param_get"),
            payload: payload.to_vec(),
            status_leaf: "parameters".to_string(),
            velocity_device: None,
            velocity_remaining: None,
            velocity_credential_id: None,
            velocity_limit: None,
            velocity_used: None,
            velocity_budget_version: None,
            encoding: command_request_encoding("param_get"),
        })
    }

    fn authorize_trajectory(&self, payload: &[u8]) -> Result<AuthorizedCommand, PolicyError> {
        let request = flatbuffers::root::<TrajectorySetRequest<'_>>(payload)
            .map_err(|error| invalid(error.to_string()))?;
        let segments = request
            .segments()
            .ok_or_else(|| invalid("TrajectorySetRequest has no segments"))?;
        if !(1..=6).contains(&segments.len()) || request.total() != segments.len() as u32 {
            return rejected("trajectory must contain one to six complete segments");
        }
        for index in 0..segments.len() {
            let segment = segments.get(index);
            if segment.segment_seq() != index as u32 || segment.frame().0 != LOCAL_ENU_FRAME {
                return rejected("trajectory segments must be ordered Local ENU segments");
            }
            let start = segment.p0_enu_m();
            let end = segment.p1_enu_m();
            if [start.x(), start.y(), start.z(), end.x(), end.y(), end.z()]
                .iter()
                .any(|value| !value.is_finite())
            {
                return rejected("trajectory coordinates must be finite");
            }
        }
        Ok(AuthorizedCommand {
            delivery: Delivery::Query,
            target: self.vehicle_command_key("trajectory_set"),
            payload: payload.to_vec(),
            status_leaf: "trajectory".to_string(),
            velocity_device: None,
            velocity_remaining: None,
            velocity_credential_id: None,
            velocity_limit: None,
            velocity_used: None,
            velocity_budget_version: None,
            encoding: command_request_encoding("trajectory_set"),
        })
    }

    fn authorize_raw(&self, leaf: &str, payload: &[u8]) -> Result<AuthorizedCommand, PolicyError> {
        if leaf.is_empty()
            || leaf.len() > 64
            || !leaf
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return rejected("raw target must be one safe topic leaf");
        }
        if leaf == "pos_sp" || leaf == "local_position_command" {
            let (team, vehicle_payload) = credential_envelope(payload, RAW_VELOCITY_MAGIC, None)?;
            if vehicle_payload.is_empty() || vehicle_payload.len() > self.config.raw_max_bytes {
                return rejected("credentialed raw velocity payload has an invalid size");
            }
            let state = self.consume_velocity_budget(&team, None)?;
            let mut command = self.publish_topic(
                leaf,
                vehicle_payload,
                "velocity",
                Some(state.device_id.clone()),
            );
            apply_budget_state(&mut command, &state);
            return Ok(command);
        }
        if payload.is_empty() || payload.len() > self.config.raw_max_bytes {
            return rejected(format!(
                "raw payload must contain 1 to {} bytes",
                self.config.raw_max_bytes
            ));
        }
        Ok(self.publish_topic(leaf, payload, &format!("raw/{leaf}"), None))
    }

    fn authorize_firmware(
        &self,
        suffix: &str,
        payload: &[u8],
    ) -> Result<AuthorizedCommand, PolicyError> {
        let parts = suffix.split('/').collect::<Vec<_>>();
        let recognized = matches!(parts.as_slice(), [id, "start"] if safe_id(id))
            || matches!(parts.as_slice(), [id, "commit"] if safe_id(id))
            || matches!(parts.as_slice(), [id, "chunk", index] if safe_id(id) && index.parse::<u32>().is_ok());
        if !recognized {
            return Err(PolicyError::UnknownIntent(format!("firmware/{suffix}")));
        }
        let max_payload = match parts.as_slice() {
            [_, "start"] => FIRMWARE_PREPARE_MAX_BYTES,
            [_, "commit"] => FIRMWARE_COMMIT_MAX_BYTES,
            [_, "chunk", _] => FIRMWARE_CHUNK_MAX_BYTES,
            _ => unreachable!("recognized firmware shape"),
        };
        if payload.is_empty() || payload.len() > max_payload {
            return rejected(format!(
                "firmware payload must contain 1 to {max_payload} bytes"
            ));
        }
        let update_id = parts[0];
        Ok(AuthorizedCommand {
            delivery: Delivery::Firmware,
            target: suffix.to_string(),
            payload: payload.to_vec(),
            status_leaf: format!("firmware/{update_id}"),
            velocity_device: None,
            velocity_remaining: None,
            velocity_credential_id: None,
            velocity_limit: None,
            velocity_used: None,
            velocity_budget_version: None,
            encoding: None,
        })
    }

    fn consume_velocity_budget(
        &self,
        team: &str,
        velocity: Option<f32>,
    ) -> Result<BudgetState, PolicyError> {
        let store = self
            .velocity_budget
            .lock()
            .expect("velocity budget lock poisoned");
        let credential = store.resolve(team).map_err(PolicyError::Rejected)?;
        store
            .consume(&credential, velocity)
            .map_err(PolicyError::Rejected)
    }

    /// Compact-key publication (`[<namespace>/]<catalog key>`).
    fn publish_topic(
        &self,
        key: &str,
        payload: &[u8],
        status_leaf: &str,
        velocity_device: Option<String>,
    ) -> AuthorizedCommand {
        let namespace = self.config.vehicle_topic_prefix.trim_matches('/');
        AuthorizedCommand {
            delivery: Delivery::Publish,
            target: if namespace.is_empty() {
                key.to_string()
            } else {
                format!("{namespace}/{key}")
            },
            payload: payload.to_vec(),
            status_leaf: status_leaf.to_string(),
            velocity_device,
            velocity_remaining: None,
            velocity_credential_id: None,
            velocity_limit: None,
            velocity_used: None,
            velocity_budget_version: None,
            encoding: synapse_fbs::topic_catalog::topic_by_key(key)
                .map(synapse_fbs::value_contract::encoding_for_topic),
        }
    }

    /// Command queryable key (`[<namespace>/]cmd/<command>`).
    fn vehicle_command_key(&self, name: &str) -> String {
        let namespace = self.config.vehicle_topic_prefix.trim_matches('/');
        let cmd = synapse_fbs::topic_catalog::CMD_KEY_PREFIX;
        if namespace.is_empty() {
            format!("{cmd}/{name}")
        } else {
            format!("{namespace}/{cmd}/{name}")
        }
    }
}

/// Canonical value-contract encoding for a command's request payload.
pub(crate) fn command_request_encoding(name: &str) -> Option<String> {
    let command = synapse_fbs::topic_catalog::command_by_name(name)?;
    let media_type = if command.request_encoding == "struct" {
        synapse_fbs::value_contract::STRUCT_MEDIA_TYPE
    } else {
        synapse_fbs::value_contract::FLATBUFFER_MEDIA_TYPE
    };
    Some(format!(
        "{media_type};type={};schema=sha256-128:{}",
        command.request_type, command.request_schema_hash
    ))
}

fn apply_budget_state(command: &mut AuthorizedCommand, state: &BudgetState) {
    command.velocity_device = Some(state.device_id.clone());
    command.velocity_credential_id = Some(state.credential_id.clone());
    command.velocity_limit = Some(state.limit);
    command.velocity_used = Some(state.used);
    command.velocity_remaining = Some(state.remaining);
    command.velocity_budget_version = Some(state.budget_version.clone());
}

fn credential_envelope<'a>(
    payload: &'a [u8],
    magic: &[u8; 4],
    expected_body: Option<usize>,
) -> Result<(String, &'a [u8]), PolicyError> {
    if payload.get(..4) != Some(magic.as_slice()) {
        return Err(invalid("velocity credential envelope has invalid magic"));
    }
    let name_len = *payload
        .get(4)
        .ok_or_else(|| invalid("velocity credential envelope is missing a team name"))?
        as usize;
    if name_len == 0 || name_len > TEAM_NAME_MAX_BYTES {
        return Err(invalid("velocity team name length is out of range"));
    }
    let body_start = 5 + name_len;
    let name = std::str::from_utf8(
        payload
            .get(5..body_start)
            .ok_or_else(|| invalid("velocity team name is truncated"))?,
    )
    .map_err(|_| invalid("velocity team name is not valid UTF-8"))?;
    if !crate::velocity_budget::safe_device_id(name) || name.len() > TEAM_NAME_MAX_BYTES {
        return rejected(
            "team name must be 1-64 characters of letters, numbers, dot, underscore, colon, or hyphen",
        );
    }
    let body = &payload[body_start..];
    if expected_body.is_some_and(|expected| body.len() != expected) {
        return Err(invalid("velocity payload has an invalid size"));
    }
    Ok((name.to_ascii_lowercase(), body))
}

fn team_name_from_any_velocity_envelope(payload: &[u8]) -> Result<String, PolicyError> {
    let magic = payload
        .get(..4)
        .ok_or_else(|| invalid("velocity credential envelope is too short"))?;
    if magic == VELOCITY_MAGIC {
        credential_envelope(payload, VELOCITY_MAGIC, Some(VELOCITY_PAYLOAD_BYTES)).map(|v| v.0)
    } else if magic == VELOCITY_BUDGET_MAGIC {
        credential_envelope(payload, VELOCITY_BUDGET_MAGIC, Some(0)).map(|v| v.0)
    } else if magic == RAW_VELOCITY_MAGIC {
        credential_envelope(payload, RAW_VELOCITY_MAGIC, None).map(|v| v.0)
    } else {
        Err(invalid("velocity credential envelope has invalid magic"))
    }
}

fn load_parameter_limits(path: Option<&std::path::Path>) -> HashMap<String, (f64, f64)> {
    let json = match path {
        Some(path) => match fs::read_to_string(path) {
            Ok(json) => json,
            Err(error) => {
                tracing::error!(
                    path = %path.display(),
                    %error,
                    "LAN parameter limits unavailable; denying all LAN parameter writes"
                );
                return HashMap::new();
            }
        },
        None => DEFAULT_PARAMETER_LIMITS.to_string(),
    };
    match parse_parameter_limits(&json) {
        Ok(limits) => limits,
        Err(error) => {
            tracing::error!(%error, "LAN parameter limits invalid; denying all LAN parameter writes");
            HashMap::new()
        }
    }
}

fn parse_parameter_limits(json: &str) -> Result<HashMap<String, (f64, f64)>, String> {
    let file: ParameterLimitsFile =
        serde_json::from_str(json).map_err(|error| format!("parse limits: {error}"))?;
    if file.schema != PARAMETER_LIMITS_SCHEMA {
        return Err(format!("unsupported schema {:?}", file.schema));
    }
    if file.parameters.len() != PUBLIC_PARAMETER_NAMES.len() {
        return Err("limits must define exactly seven public parameters".into());
    }
    let expected = PUBLIC_PARAMETER_NAMES.into_iter().collect::<HashSet<_>>();
    let mut limits = HashMap::with_capacity(file.parameters.len());
    for parameter in file.parameters {
        if !expected.contains(parameter.name.as_str()) {
            return Err(format!("unexpected public parameter {:?}", parameter.name));
        }
        if !parameter.minimum.is_finite()
            || !parameter.maximum.is_finite()
            || parameter.minimum > parameter.maximum
        {
            return Err(format!("invalid limits for {:?}", parameter.name));
        }
        let name = parameter.name;
        if limits
            .insert(name.clone(), (parameter.minimum, parameter.maximum))
            .is_some()
        {
            return Err(format!("duplicate public parameter {name:?}"));
        }
    }
    if limits.len() != expected.len() {
        return Err("limits do not define every public parameter".into());
    }
    Ok(limits)
}

fn validate_gain(
    limits: &HashMap<String, (f64, f64)>,
    name: &str,
    kind: ParamKind,
    value: f64,
) -> Result<(), PolicyError> {
    if kind != ParamKind::Float {
        return rejected("public parameters must use ParamKind::Float");
    }
    let &(minimum, maximum) = limits.get(name).ok_or_else(|| {
        PolicyError::Rejected(format!("public parameter {name:?} is not allowlisted"))
    })?;
    if !value.is_finite() {
        return rejected(format!("parameter {name} must be finite"));
    }
    if value < minimum || value > maximum {
        return rejected(format!(
            "parameter {name} must be within [{minimum}, {maximum}]"
        ));
    }
    Ok(())
}

fn follow_struct<'a, T>(payload: &'a [u8], expected: usize) -> Result<T::Inner, PolicyError>
where
    T: flatbuffers::Follow<'a>,
{
    if payload.len() != expected {
        return Err(invalid(format!(
            "payload is {} bytes, expected {expected}",
            payload.len()
        )));
    }
    // SAFETY: Synapse generated fixed-layout structs use unaligned accessors,
    // and the exact-size check above guarantees the complete backing storage.
    Ok(unsafe { T::follow(payload, 0) })
}

fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn rejected<T>(reason: impl Into<String>) -> Result<T, PolicyError> {
    Err(PolicyError::Rejected(reason.into()))
}

fn invalid(reason: impl Into<String>) -> PolicyError {
    PolicyError::InvalidPayload(reason.into())
}

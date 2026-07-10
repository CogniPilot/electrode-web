//! Pure command decoding, validation, and key mapping.

use std::collections::HashMap;
use std::sync::Mutex;

use synapse_fbs::cmd::{ParamKind, ParamSetRequest};
use synapse_fbs::topic::{LocalPositionCommandData, ManualControlData, RadioControlData};

const VELOCITY_ONLY_MASK: u16 = 3527;
const LOCAL_ENU_FRAME: u8 = 0;
const MANUAL_ACTIVE_AXES_MASK: u16 = 0x03ff;
const MANUAL_FLAG_ACTIVE: u8 = 4;
const MANUAL_FLAG_VALID: u8 = 8;
const FIRMWARE_PREPARE_MAX_BYTES: usize = 4 * 1024;
const FIRMWARE_CHUNK_MAX_BYTES: usize = 68 * 1024;
const FIRMWARE_COMMIT_MAX_BYTES: usize = 2 * 1024;
/// Canonical vehicle query keys used by the staged firmware-update transfer.
pub const CANONICAL_FIRMWARE_QUERY_KEYS: [&str; 6] = [
    "synapse/v1/cmd/firmware_info",
    "synapse/v1/cmd/firmware_status",
    "synapse/v1/cmd/firmware_prepare",
    "synapse/v1/cmd/firmware_chunk",
    "synapse/v1/cmd/firmware_commit",
    "synapse/v1/cmd/firmware_abort",
];

/// Whether the authorized payload is a Zenoh publication or request/reply query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Publish,
    Query,
    Firmware,
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
}

/// Policy settings independent of Zenoh transport configuration.
#[derive(Clone, Debug)]
pub struct PolicyConfig {
    pub intent_prefix: String,
    pub vehicle_topic_prefix: String,
    pub parameter_key: String,
    pub firmware_key_prefix: String,
    pub device: String,
    pub velocity_min_mps: f32,
    pub velocity_max_mps: f32,
    pub velocity_budget: u32,
    pub raw_max_bytes: usize,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            intent_prefix: "gcs/v1/cmd".to_string(),
            vehicle_topic_prefix: "synapse/v1/topic".to_string(),
            parameter_key: "synapse/v1/cmd/param_set".to_string(),
            firmware_key_prefix: "synapse/v1/cmd/firmware".to_string(),
            device: "default".to_string(),
            velocity_min_mps: 1.0,
            velocity_max_mps: 4.0,
            velocity_budget: 5,
            raw_max_bytes: 4 * 1024,
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
    velocity_used: Mutex<HashMap<String, u32>>,
}

impl CommandPolicy {
    #[must_use]
    pub fn new(config: PolicyConfig) -> Self {
        Self {
            config,
            velocity_used: Mutex::new(HashMap::new()),
        }
    }

    pub fn authorize(&self, key: &str, payload: &[u8]) -> Result<AuthorizedCommand, PolicyError> {
        let prefix = format!("{}/", self.config.intent_prefix.trim_end_matches('/'));
        let suffix = key
            .strip_prefix(&prefix)
            .ok_or(PolicyError::WrongNamespace)?;
        match suffix {
            "velocity" => self.authorize_velocity(payload),
            "manual" => self.authorize_manual(payload),
            "radio" => self.authorize_radio(payload),
            "gain" => self.authorize_gain(payload),
            raw if raw.starts_with("raw/") => self.authorize_raw(&raw[4..], payload),
            firmware if firmware.starts_with("firmware/") => {
                self.authorize_firmware(&firmware[9..], payload)
            }
            other => Err(PolicyError::UnknownIntent(other.to_string())),
        }
    }

    pub fn refund_velocity(&self, device: &str) {
        let mut used = self
            .velocity_used
            .lock()
            .expect("velocity budget lock poisoned");
        if let Some(count) = used.get_mut(device) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                used.remove(device);
            }
        }
    }

    fn authorize_velocity(&self, payload: &[u8]) -> Result<AuthorizedCommand, PolicyError> {
        let command = follow_struct::<LocalPositionCommandData>(payload, 56)?;
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
        let remaining = self.consume_velocity_budget()?;
        let mut command = self.publish_topic(
            "local_position_command",
            payload,
            "velocity",
            Some(self.config.device.clone()),
        );
        command.velocity_remaining = Some(remaining);
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
        Ok(self.publish_topic("manual_control_command", payload, "manual", None))
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
        Ok(self.publish_topic("radio_control", payload, "radio", None))
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
        validate_gain(name, value.kind(), value.float_value())?;
        Ok(AuthorizedCommand {
            delivery: Delivery::Query,
            target: self.config.parameter_key.clone(),
            payload: payload.to_vec(),
            status_leaf: "gain".to_string(),
            velocity_device: None,
            velocity_remaining: None,
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
        })
    }

    fn consume_velocity_budget(&self) -> Result<u32, PolicyError> {
        let mut used = self
            .velocity_used
            .lock()
            .expect("velocity budget lock poisoned");
        let count = used.entry(self.config.device.clone()).or_insert(0);
        if *count >= self.config.velocity_budget {
            return rejected("velocity command budget exhausted");
        }
        *count += 1;
        Ok(self.config.velocity_budget.saturating_sub(*count))
    }

    #[must_use]
    pub fn velocity_remaining(&self, device: &str) -> u32 {
        let used = self
            .velocity_used
            .lock()
            .expect("velocity budget lock poisoned");
        self.config
            .velocity_budget
            .saturating_sub(used.get(device).copied().unwrap_or(0))
    }

    fn publish_topic(
        &self,
        leaf: &str,
        payload: &[u8],
        status_leaf: &str,
        velocity_device: Option<String>,
    ) -> AuthorizedCommand {
        AuthorizedCommand {
            delivery: Delivery::Publish,
            target: format!(
                "{}/{}",
                self.config.vehicle_topic_prefix.trim_end_matches('/'),
                leaf
            ),
            payload: payload.to_vec(),
            status_leaf: status_leaf.to_string(),
            velocity_device,
            velocity_remaining: None,
        }
    }
}

fn validate_gain(name: &str, kind: ParamKind, value: f64) -> Result<(), PolicyError> {
    if kind != ParamKind::Float {
        return rejected("controller gains must use ParamKind::Float");
    }
    let (min, max) = match name {
        "attitude.headingPid.kp" => (0.0, 5.0),
        "attitude.headingPid.ki" => (0.0, 1.0),
        "attitude.headingPid.kd" => (0.0, 2.0),
        _ => return rejected(format!("gain parameter {name:?} is not allowlisted")),
    };
    if !value.is_finite() || value < min || value > max {
        return rejected(format!("gain {name} must be within [{min}, {max}]"));
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

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    Normal,
    High,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageHeader {
    pub sequence: u64,
    pub source_time_ns: u64,
    pub receive_time_ns: u64,
    pub expire_time_ns: u64,
    pub vehicle_id: String,
    pub schema_version: u16,
    pub message_type: String,
    pub priority: Priority,
    pub stream_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryEnvelope<T = serde_json::Value> {
    pub kind: String,
    pub topic: String,
    pub header: MessageHeader,
    pub payload: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("schema version {actual} is not supported; expected {expected}")]
    UnsupportedSchema { expected: u16, actual: u16 },
    #[error("message expired at {expire_time_ns}; now is {now_ns}")]
    Expired { expire_time_ns: u64, now_ns: u64 },
    #[error("vehicle id is empty")]
    MissingVehicleId,
    #[error("stream id is empty")]
    MissingStreamId,
}

pub fn validate_header(header: &MessageHeader, now_ns: u64) -> Result<(), ValidationError> {
    if header.schema_version != SCHEMA_VERSION {
        return Err(ValidationError::UnsupportedSchema {
            expected: SCHEMA_VERSION,
            actual: header.schema_version,
        });
    }

    if header.expire_time_ns > 0 && header.expire_time_ns < now_ns {
        return Err(ValidationError::Expired {
            expire_time_ns: header.expire_time_ns,
            now_ns,
        });
    }

    if header.vehicle_id.trim().is_empty() {
        return Err(ValidationError::MissingVehicleId);
    }

    if header.stream_id.trim().is_empty() {
        return Err(ValidationError::MissingStreamId);
    }

    Ok(())
}

pub fn validate_json_frame(frame: &str, now_ns: u64) -> Result<(), String> {
    let envelope: TelemetryEnvelope = serde_json::from_str(frame).map_err(|err| err.to_string())?;
    validate_header(&envelope.header, now_ns).map_err(|err| err.to_string())
}

/// Cross-check that the kernel actually has listeners bound for the given Zenoh
/// listen locators (e.g. `udp/0.0.0.0:7447`, `ws/127.0.0.1:7447`).
///
/// `zenoh::open` returns success and logs "Zenoh can be reached at: …" even when
/// a listener's bind silently fails (observed on rapid restarts / port reuse),
/// so a hub can end up advertising an endpoint nothing is actually listening on.
/// This inspects `/proc/net/{tcp,tcp6,udp,udp6}` and returns the subset of
/// locators with **no** matching bound socket (empty slice = all good).
///
/// Linux-only; on other targets it returns an empty vec (verification skipped).
pub fn unbound_listeners(locators: &[String]) -> Vec<String> {
    locators
        .iter()
        .filter(|locator| !locator_is_bound(locator))
        .cloned()
        .collect()
}

#[cfg(target_os = "linux")]
fn locator_is_bound(locator: &str) -> bool {
    // Locator form: "<proto>/<host>:<port>[?params]", e.g. "ws/0.0.0.0:7447".
    let (proto, rest) = match locator.split_once('/') {
        Some(parts) => parts,
        None => return true, // unrecognized shape: don't block startup
    };
    let addr = rest.split('?').next().unwrap_or(rest);
    let port = match addr.rsplit(':').next().and_then(|p| p.trim().parse::<u16>().ok()) {
        Some(port) => port,
        None => return true,
    };
    let hex_port = format!("{port:04X}");
    match proto {
        "udp" => proc_has_port(&["/proc/net/udp", "/proc/net/udp6"], &hex_port, None),
        // ws/tls/quic all ride TCP and appear as a LISTEN (state 0A) socket.
        "tcp" | "ws" | "tls" | "quic" => {
            proc_has_port(&["/proc/net/tcp", "/proc/net/tcp6"], &hex_port, Some("0A"))
        }
        _ => true, // unknown transport: assume fine rather than block
    }
}

#[cfg(target_os = "linux")]
fn proc_has_port(files: &[&str], hex_port: &str, want_state: Option<&str>) -> bool {
    for file in files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in content.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // cols[1] = local_address "HEXIP:HEXPORT", cols[3] = connection state.
            let (Some(local), Some(state)) = (cols.get(1), cols.get(3)) else {
                continue;
            };
            let Some((_, port)) = local.rsplit_once(':') else {
                continue;
            };
            if port.eq_ignore_ascii_case(hex_port)
                && want_state.map_or(true, |s| state.eq_ignore_ascii_case(s))
            {
                return true;
            }
        }
    }
    false
}

#[cfg(not(target_os = "linux"))]
fn locator_is_bound(_locator: &str) -> bool {
    true
}

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = validateJsonFrame)]
pub fn validate_json_frame_wasm(frame: &str, now_ns: f64) -> String {
    match validate_json_frame(frame, now_ns as u64) {
        Ok(()) => String::new(),
        Err(err) => err,
    }
}

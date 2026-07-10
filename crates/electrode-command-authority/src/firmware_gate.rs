//! Stateful browser-upload assembly and trusted-baseline gate.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use zenoh::{Session, Wait};

use crate::firmware::{self, GainWindow, Origin, TransferConfig};

const DEFAULT_BASELINE_PATH: &str = "artifacts/cubs2-firmware/baseline-cubs2-mr-vmu-tropic.bin";
const DEFAULT_GAIN_WINDOWS_PATH: &str = "artifacts/cubs2-firmware/gain-windows-cubs2.json";
const DEFAULT_GAIN_WINDOWS: &[GainWindow] = &[GainWindow {
    origin: Origin::Start,
    offset: 18_480,
    length: 8,
}];
const MAX_PENDING_UPLOADS: usize = 4;
const MAX_PREPARE_PAYLOAD: usize = 4 * 1024;
const MAX_COMMIT_PAYLOAD: usize = 2 * 1024;
const MAX_CHUNK_OVERHEAD: usize = 4 * 1024;

pub(crate) struct FirmwareGate {
    baseline: Mutex<Option<Vec<u8>>>,
    windows: Vec<GainWindow>,
    auto_bootstrap: bool,
    uploads: Mutex<HashMap<String, FirmwareUpload>>,
    max_image_size: usize,
    transfer: TransferConfig,
}

struct FirmwareUpload {
    expected_size: usize,
    chunk_size: usize,
    version: String,
    image_sha256: String,
    chunks: Vec<Option<Vec<u8>>>,
    received_bytes: usize,
}

impl FirmwareGate {
    pub(crate) fn from_env(key_prefix: String, query_timeout: Duration) -> Self {
        let baseline_path = env::var("ELECTRODE_GCS_FIRMWARE_BASELINE")
            .ok()
            .filter(|path| !path.trim().is_empty())
            .or_else(|| {
                Path::new(DEFAULT_BASELINE_PATH)
                    .exists()
                    .then(|| DEFAULT_BASELINE_PATH.to_string())
            });
        let baseline = baseline_path.and_then(|path| match fs::read(&path) {
            Ok(bytes) => {
                tracing::info!(%path, size = bytes.len(), "firmware baseline loaded");
                Some(bytes)
            }
            Err(error) => {
                tracing::warn!(%path, %error, "firmware baseline unreadable");
                None
            }
        });
        let windows = env::var("ELECTRODE_GCS_GAIN_WINDOWS_JSON")
            .ok()
            .or_else(|| {
                env::var("ELECTRODE_GCS_GAIN_WINDOWS_PATH")
                    .ok()
                    .and_then(|path| fs::read_to_string(path).ok())
            })
            .or_else(|| fs::read_to_string(DEFAULT_GAIN_WINDOWS_PATH).ok())
            .map(|json| firmware::parse_gain_windows(&json))
            .filter(|windows| !windows.is_empty())
            .unwrap_or_else(|| DEFAULT_GAIN_WINDOWS.to_vec());
        let auto_bootstrap = baseline.is_none() && env_bool("ELECTRODE_GCS_FIRMWARE_AUTOBOOTSTRAP");

        Self {
            baseline: Mutex::new(baseline),
            windows,
            auto_bootstrap,
            uploads: Mutex::new(HashMap::new()),
            max_image_size: env_usize(
                "ELECTRODE_GCS_FIRMWARE_MAX_IMAGE_SIZE",
                8 * 1024 * 1024,
                1,
                64 * 1024 * 1024,
            ),
            transfer: TransferConfig {
                key_prefix,
                target: env::var("ELECTRODE_GCS_FIRMWARE_TARGET")
                    .unwrap_or_else(|_| "cubs2".to_string()),
                board_id: env::var("ELECTRODE_GCS_FIRMWARE_BOARD_ID").unwrap_or_default(),
                chunk_size: env_usize("ELECTRODE_GCS_FIRMWARE_CHUNK_SIZE", 512, 128, 4096),
                retries: env_u32("ELECTRODE_GCS_FIRMWARE_RETRIES", 3).clamp(1, 10),
                timeout: Duration::from_millis(
                    env_u64(
                        "ELECTRODE_GCS_FIRMWARE_TIMEOUT_MS",
                        query_timeout.as_millis() as u64,
                    )
                    .clamp(100, 60_000),
                ),
            },
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn handle_intent(
        &self,
        vehicle: &Session,
        browser: &Session,
        intent_prefix: &str,
        suffix: &str,
        payload: &[u8],
    ) {
        let mut parts = suffix.split('/');
        let update_id = parts.next().unwrap_or_default();
        if !valid_update_id(update_id) {
            tracing::warn!(%update_id, "invalid firmware update id");
            return;
        }
        let Some(operation) = parts.next() else {
            publish_status(
                browser,
                intent_prefix,
                update_id,
                "rejected",
                "upload",
                0,
                "Firmware uploads require prepare, chunk, and commit intents.",
                None,
            );
            return;
        };

        match operation {
            "start" if parts.next().is_none() => {
                if payload.len() > MAX_PREPARE_PAYLOAD {
                    self.reject_upload(
                        browser,
                        intent_prefix,
                        update_id,
                        "Firmware prepare payload is too large.",
                    );
                    return;
                }
                let start = match firmware::decode_firmware_prepare_upload(payload) {
                    Ok(start) => start,
                    Err(error) => {
                        tracing::warn!(%update_id, %error, "invalid FirmwarePrepareRequest");
                        self.reject_upload(
                            browser,
                            intent_prefix,
                            update_id,
                            "Firmware prepare payload is not a valid Synapse FlatBuffer.",
                        );
                        return;
                    }
                };
                let image_size = usize::try_from(start.image_size).unwrap_or(usize::MAX);
                let chunk_size = start.requested_chunk_size as usize;
                let chunk_count = start.chunk_count as usize;
                let expected_chunks = image_size.div_ceil(chunk_size.max(1));
                if start.update_id != update_id
                    || image_size == 0
                    || image_size > self.max_image_size
                    || !(1024..=65_536).contains(&chunk_size)
                    || chunk_count == 0
                    || chunk_count != expected_chunks
                    || chunk_count > self.max_image_size.div_ceil(1024)
                    || !valid_sha256_hex(&start.image_sha256)
                {
                    self.reject_upload(
                        browser,
                        intent_prefix,
                        update_id,
                        "Firmware upload size or chunk metadata is outside GCS limits.",
                    );
                    return;
                }
                let version = sanitized_version(&start.version);
                let mut uploads = self.uploads.lock().expect("firmware uploads lock poisoned");
                if uploads.len() >= MAX_PENDING_UPLOADS && !uploads.contains_key(update_id) {
                    drop(uploads);
                    self.reject_upload(
                        browser,
                        intent_prefix,
                        update_id,
                        "Too many firmware uploads are pending.",
                    );
                    return;
                }
                uploads.insert(
                    update_id.to_string(),
                    FirmwareUpload {
                        expected_size: image_size,
                        chunk_size,
                        version,
                        image_sha256: start.image_sha256.to_ascii_lowercase(),
                        chunks: vec![None; chunk_count],
                        received_bytes: 0,
                    },
                );
                drop(uploads);
                tracing::info!(%update_id, image_size, chunk_size, chunk_count, "firmware upload started");
                publish_status(
                    browser,
                    intent_prefix,
                    update_id,
                    "in_progress",
                    "upload",
                    0,
                    "GCS is receiving the browser firmware image.",
                    Some((0, image_size, None)),
                );
            }
            "chunk" => {
                let Some(index) = parts.next().and_then(|raw| raw.parse::<usize>().ok()) else {
                    self.reject_upload(
                        browser,
                        intent_prefix,
                        update_id,
                        "Firmware chunk index is invalid.",
                    );
                    return;
                };
                if parts.next().is_some() || payload.len() > 65_536 + MAX_CHUNK_OVERHEAD {
                    self.reject_upload(
                        browser,
                        intent_prefix,
                        update_id,
                        "Firmware chunk key or payload is invalid.",
                    );
                    return;
                }
                let chunk = match firmware::decode_firmware_chunk_upload(payload) {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        tracing::warn!(%update_id, index, %error, "invalid FirmwareChunkRequest");
                        self.reject_upload(
                            browser,
                            intent_prefix,
                            update_id,
                            "Firmware chunk payload is not a valid Synapse FlatBuffer.",
                        );
                        return;
                    }
                };
                let mut uploads = self.uploads.lock().expect("firmware uploads lock poisoned");
                let Some(upload) = uploads.get_mut(update_id) else {
                    drop(uploads);
                    self.reject_upload(
                        browser,
                        intent_prefix,
                        update_id,
                        "Firmware upload was not started.",
                    );
                    return;
                };
                if index >= upload.chunks.len() {
                    drop(uploads);
                    self.reject_upload(
                        browser,
                        intent_prefix,
                        update_id,
                        "Firmware chunk index is out of range.",
                    );
                    return;
                }
                let offset = index * upload.chunk_size;
                let expected_len = upload.chunk_size.min(upload.expected_size - offset);
                let expected_final = index + 1 == upload.chunks.len();
                let valid = chunk.update_id == update_id
                    && chunk.chunk_index as usize == index
                    && chunk.offset == offset as u64
                    && chunk.data.len() == expected_len
                    && chunk.final_chunk == expected_final
                    && valid_sha256_hex(&chunk.chunk_sha256)
                    && firmware::full_sha256(&chunk.data).eq_ignore_ascii_case(&chunk.chunk_sha256);
                if !valid {
                    drop(uploads);
                    self.reject_upload(
                        browser,
                        intent_prefix,
                        update_id,
                        "Firmware chunk metadata or SHA-256 does not match the upload.",
                    );
                    return;
                }
                if let Some(previous) = upload.chunks[index].as_ref() {
                    if previous != &chunk.data {
                        drop(uploads);
                        self.reject_upload(
                            browser,
                            intent_prefix,
                            update_id,
                            "A repeated firmware chunk changed its contents.",
                        );
                        return;
                    }
                } else {
                    upload.received_bytes += chunk.data.len();
                    upload.chunks[index] = Some(chunk.data);
                }
                let received = upload.received_bytes;
                let expected = upload.expected_size;
                drop(uploads);
                publish_status(
                    browser,
                    intent_prefix,
                    update_id,
                    "in_progress",
                    "upload",
                    ((received * 5) / expected.max(1)).min(5) as u8,
                    &format!("Received {received}/{expected} browser bytes."),
                    Some((received, expected, Some(index))),
                );
            }
            "commit" if parts.next().is_none() => {
                if payload.len() > MAX_COMMIT_PAYLOAD {
                    self.reject_upload(
                        browser,
                        intent_prefix,
                        update_id,
                        "Firmware commit payload is too large.",
                    );
                    return;
                }
                let commit = match firmware::decode_firmware_commit_upload(payload) {
                    Ok(commit) => commit,
                    Err(error) => {
                        tracing::warn!(%update_id, %error, "invalid FirmwareCommitRequest");
                        self.reject_upload(
                            browser,
                            intent_prefix,
                            update_id,
                            "Firmware commit payload is not a valid Synapse FlatBuffer.",
                        );
                        return;
                    }
                };
                let upload = self
                    .uploads
                    .lock()
                    .expect("firmware uploads lock poisoned")
                    .remove(update_id);
                let Some(upload) = upload else {
                    self.reject_upload(
                        browser,
                        intent_prefix,
                        update_id,
                        "Firmware upload was not started.",
                    );
                    return;
                };
                if commit.update_id != update_id
                    || !valid_sha256_hex(&commit.image_sha256)
                    || !commit
                        .image_sha256
                        .eq_ignore_ascii_case(&upload.image_sha256)
                {
                    self.reject_upload(
                        browser,
                        intent_prefix,
                        update_id,
                        "Firmware commit identifier or SHA-256 does not match prepare.",
                    );
                    return;
                }
                if upload.received_bytes != upload.expected_size
                    || upload.chunks.iter().any(Option::is_none)
                {
                    self.reject_upload(
                        browser,
                        intent_prefix,
                        update_id,
                        "Firmware upload is incomplete.",
                    );
                    return;
                }
                let mut image = Vec::with_capacity(upload.expected_size);
                for chunk in upload.chunks.into_iter().flatten() {
                    image.extend_from_slice(&chunk);
                }
                if !firmware::full_sha256(&image).eq_ignore_ascii_case(&upload.image_sha256) {
                    self.reject_upload(
                        browser,
                        intent_prefix,
                        update_id,
                        "Reconstructed firmware SHA-256 does not match prepare.",
                    );
                    return;
                }
                self.handle_candidate(
                    vehicle,
                    browser,
                    intent_prefix,
                    update_id,
                    &upload.version,
                    &image,
                );
            }
            _ => self.reject_upload(
                browser,
                intent_prefix,
                update_id,
                "Unknown firmware upload operation.",
            ),
        }
    }

    fn handle_candidate(
        &self,
        vehicle: &Session,
        browser: &Session,
        intent_prefix: &str,
        update_id: &str,
        version: &str,
        image: &[u8],
    ) {
        publish_status(
            browser,
            intent_prefix,
            update_id,
            "validating",
            "validate",
            0,
            "Validating candidate against the trusted baseline.",
            None,
        );
        let baseline = {
            let mut baseline = self
                .baseline
                .lock()
                .expect("firmware baseline lock poisoned");
            if baseline.is_none() && self.auto_bootstrap {
                *baseline = Some(image.to_vec());
                publish_status(
                    browser,
                    intent_prefix,
                    update_id,
                    "complete",
                    "baseline",
                    100,
                    "Firmware image stored as the local baseline; no update was transferred.",
                    None,
                );
                return;
            }
            baseline.clone()
        };
        let Some(baseline) = baseline else {
            self.reject_validation(
                browser,
                intent_prefix,
                update_id,
                "No trusted firmware baseline is configured.",
            );
            return;
        };
        let validation = firmware::validate_gain_only_change(image, &baseline, &self.windows);
        if !validation.allowed {
            let first_outside = validation
                .outside_gain_diffs
                .first()
                .map(|diff| diff.offset);
            tracing::warn!(
                %update_id,
                candidate_size = validation.candidate_size,
                baseline_size = validation.baseline_size,
                differing = validation.differing_byte_count,
                gain_differing = validation.gain_differing_byte_count,
                ?first_outside,
                reason = %validation.reason,
                "firmware candidate rejected"
            );
            self.reject_validation(browser, intent_prefix, update_id, &validation.reason);
            return;
        }
        let progress = |phase: &str, progress_pct: u8, message: &str| {
            publish_status(
                browser,
                intent_prefix,
                update_id,
                if phase == "complete" {
                    "complete"
                } else {
                    "in_progress"
                },
                phase,
                progress_pct,
                message,
                None,
            );
        };
        if let Err(error) = firmware::transfer_firmware(
            vehicle,
            &self.transfer,
            update_id,
            version,
            image,
            &self.windows,
            progress,
        ) {
            tracing::error!(%update_id, %error, "firmware update failed");
            publish_status(
                browser,
                intent_prefix,
                update_id,
                "failed",
                "error",
                0,
                &error.to_string(),
                None,
            );
        }
    }

    fn reject_upload(&self, browser: &Session, prefix: &str, id: &str, message: &str) {
        publish_status(browser, prefix, id, "rejected", "upload", 0, message, None);
    }

    fn reject_validation(&self, browser: &Session, prefix: &str, id: &str, message: &str) {
        publish_status(
            browser, prefix, id, "rejected", "validate", 0, message, None,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_status(
    browser: &Session,
    intent_prefix: &str,
    update_id: &str,
    status: &str,
    phase: &str,
    progress_pct: u8,
    message: &str,
    upload: Option<(usize, usize, Option<usize>)>,
) {
    let key = format!(
        "{}/status/firmware/{update_id}",
        intent_prefix.strip_suffix("/cmd").unwrap_or(intent_prefix)
    );
    let mut value = serde_json::json!({
        "updateId": update_id,
        "status": status,
        "phase": phase,
        "progressPct": progress_pct,
        "message": message,
    });
    if let Some((received, total, chunk_index)) = upload {
        value["receivedBytes"] = received.into();
        value["totalBytes"] = total.into();
        value["chunkIndex"] = chunk_index.into();
    }
    if let Err(error) = browser.put(&key, value.to_string()).wait() {
        tracing::warn!(%key, %error, "firmware status publish failed");
    }
}

pub(crate) fn publish_policy_rejection(
    browser: &Session,
    intent_prefix: &str,
    update_id: &str,
    message: &str,
) -> bool {
    if !valid_update_id(update_id) {
        return false;
    }
    publish_status(
        browser,
        intent_prefix,
        update_id,
        "rejected",
        "upload",
        0,
        message,
        None,
    );
    true
}

fn valid_update_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sanitized_version(value: &str) -> String {
    let value = value
        .chars()
        .take(128)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if value.is_empty() {
        "browser-upload.bin".to_string()
    } else {
        value
    }
}

fn env_bool(key: &str) -> bool {
    env::var(key)
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn env_u32(key: &str, default: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize, min: usize, max: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_ids_and_hashes_are_bounded() {
        assert!(valid_update_id("fw-123_alpha"));
        assert!(!valid_update_id("../firmware"));
        assert!(!valid_update_id(&"x".repeat(81)));
        assert!(valid_sha256_hex(&"a5".repeat(32)));
        assert!(!valid_sha256_hex(&"z".repeat(64)));
    }

    #[test]
    fn versions_are_safe_and_bounded() {
        assert_eq!(
            sanitized_version("candidate cubs2!.bin"),
            "candidate_cubs2_.bin"
        );
        assert_eq!(sanitized_version(""), "browser-upload.bin");
        assert_eq!(sanitized_version(&"a".repeat(200)).len(), 128);
    }
}

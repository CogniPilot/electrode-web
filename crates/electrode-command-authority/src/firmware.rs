//! Firmware gain-window checksum validation — the GCS's second security gate.
//!
//! A candidate firmware image is accepted only if it is byte-identical to a trusted baseline
//! *except* inside authorized "gain windows" (small regions where tuning gains
//! live). Anything else — a size change or a single byte flipped outside a
//! window — is rejected, so the upload surface can only re-tune gains, not
//! replace firmware.

use anyhow::{anyhow, bail, Context, Result};
use flatbuffers::{FlatBufferBuilder, WIPOffset};
use sha2::{Digest, Sha256};
use std::time::Duration;
use synapse_fbs::cmd::{
    FirmwareChunkReply, FirmwareChunkRequest, FirmwareChunkRequestArgs, FirmwareCommitReply,
    FirmwareCommitRequest, FirmwareCommitRequestArgs, FirmwareInfoReply, FirmwareInfoRequest,
    FirmwareInfoRequestArgs, FirmwarePrepareReply, FirmwarePrepareRequest,
    FirmwarePrepareRequestArgs, FirmwareStatusReply, FirmwareStatusRequest,
    FirmwareStatusRequestArgs,
};
use synapse_fbs::types::CommandResultCode;
use zenoh::{Session, Wait};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Start,
    End,
}

#[derive(Clone, Copy, Debug)]
pub struct GainWindow {
    pub origin: Origin,
    pub offset: usize,
    pub length: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: usize,
    pub end: usize,
}

/// A byte that differs outside every authorized gain window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutsideDiff {
    pub offset: usize,
    pub baseline: u8,
    pub candidate: u8,
}

#[derive(Clone, Debug)]
pub struct GainValidation {
    pub allowed: bool,
    pub reason: String,
    pub baseline_size: usize,
    pub candidate_size: usize,
    pub differing_byte_count: usize,
    pub gain_differing_byte_count: usize,
    pub outside_gain_diffs: Vec<OutsideDiff>,
}

#[derive(Clone, Debug)]
pub struct TransferConfig {
    pub key_prefix: String,
    pub target: String,
    pub board_id: String,
    pub chunk_size: usize,
    pub retries: u32,
    pub timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferResult {
    pub image_sha256: String,
    pub chunk_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwarePrepareUpload {
    pub update_id: String,
    pub version: String,
    pub image_size: u64,
    pub image_sha256: String,
    pub requested_chunk_size: u32,
    pub chunk_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareChunkUpload {
    pub update_id: String,
    pub chunk_index: u32,
    pub offset: u64,
    pub data: Vec<u8>,
    pub chunk_sha256: String,
    pub final_chunk: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareCommitUpload {
    pub update_id: String,
    pub image_sha256: String,
}

fn finish_table<T>(builder: &mut FlatBufferBuilder<'_>, table: WIPOffset<T>) -> Vec<u8> {
    builder.finish(table, None);
    builder.finished_data().to_vec()
}

fn build_info_request(target: &str) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let target = builder.create_string(target);
    let table = FirmwareInfoRequest::create(
        &mut builder,
        &FirmwareInfoRequestArgs {
            target: Some(target),
        },
    );
    finish_table(&mut builder, table)
}

#[allow(clippy::too_many_arguments)]
fn build_prepare_request(
    update_id: &str,
    target: &str,
    board_id: &str,
    version: &str,
    image_size: usize,
    image_sha256: &str,
    selective_sha256: &str,
    chunk_size: usize,
    chunk_count: usize,
) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::with_capacity(1024);
    let update_id = builder.create_string(update_id);
    let target = builder.create_string(target);
    let board_id = builder.create_string(board_id);
    let version = builder.create_string(version);
    let image_sha256 = builder.create_string(image_sha256);
    let selective_sha256 = builder.create_string(selective_sha256);
    let manifest = builder.create_string("{}");
    let table = FirmwarePrepareRequest::create(
        &mut builder,
        &FirmwarePrepareRequestArgs {
            update_id: Some(update_id),
            target: Some(target),
            board_id: Some(board_id),
            version: Some(version),
            image_size: image_size as u64,
            image_sha256: Some(image_sha256),
            selective_sha256: Some(selective_sha256),
            requested_chunk_size: chunk_size as u32,
            chunk_count: chunk_count as u32,
            signature: None,
            manifest: Some(manifest),
        },
    );
    finish_table(&mut builder, table)
}

fn build_commit_request(update_id: &str, image_sha256: &str) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let update_id = builder.create_string(update_id);
    let image_sha256 = builder.create_string(image_sha256);
    let table = FirmwareCommitRequest::create(
        &mut builder,
        &FirmwareCommitRequestArgs {
            update_id: Some(update_id),
            image_sha256: Some(image_sha256),
        },
    );
    finish_table(&mut builder, table)
}

fn build_status_request(update_id: &str) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let update_id = builder.create_string(update_id);
    let table = FirmwareStatusRequest::create(
        &mut builder,
        &FirmwareStatusRequestArgs {
            update_id: Some(update_id),
        },
    );
    finish_table(&mut builder, table)
}

pub fn decode_firmware_prepare_upload(bytes: &[u8]) -> Result<FirmwarePrepareUpload> {
    let request = flatbuffers::root::<FirmwarePrepareRequest<'_>>(bytes)
        .context("decode FirmwarePrepareRequest")?;
    Ok(FirmwarePrepareUpload {
        update_id: request.update_id().unwrap_or_default().to_string(),
        version: request.version().unwrap_or_default().to_string(),
        image_size: request.image_size(),
        image_sha256: request.image_sha256().unwrap_or_default().to_string(),
        requested_chunk_size: request.requested_chunk_size(),
        chunk_count: request.chunk_count(),
    })
}

pub fn decode_firmware_chunk_upload(bytes: &[u8]) -> Result<FirmwareChunkUpload> {
    let request = flatbuffers::root::<FirmwareChunkRequest<'_>>(bytes)
        .context("decode FirmwareChunkRequest")?;
    Ok(FirmwareChunkUpload {
        update_id: request.update_id().unwrap_or_default().to_string(),
        chunk_index: request.chunk_index(),
        offset: request.offset(),
        data: request
            .data()
            .map(|data| data.iter().collect())
            .unwrap_or_default(),
        chunk_sha256: request.chunk_sha256().unwrap_or_default().to_string(),
        final_chunk: request.final_chunk(),
    })
}

pub fn decode_firmware_commit_upload(bytes: &[u8]) -> Result<FirmwareCommitUpload> {
    let request = flatbuffers::root::<FirmwareCommitRequest<'_>>(bytes)
        .context("decode FirmwareCommitRequest")?;
    Ok(FirmwareCommitUpload {
        update_id: request.update_id().unwrap_or_default().to_string(),
        image_sha256: request.image_sha256().unwrap_or_default().to_string(),
    })
}

fn require_accepted(result: CommandResultCode, operation: &str) -> Result<()> {
    if result == CommandResultCode::Accepted || result == CommandResultCode::InProgress {
        return Ok(());
    }
    bail!("{operation} rejected with firmware result code {result:?}")
}

fn accept_info_reply(bytes: &[u8]) -> Result<u32> {
    let reply =
        flatbuffers::root::<FirmwareInfoReply<'_>>(bytes).context("decode FirmwareInfoReply")?;
    require_accepted(reply.result(), "info")?;
    Ok(reply.max_chunk_size())
}

fn accept_prepare_reply(bytes: &[u8]) -> Result<()> {
    let reply = flatbuffers::root::<FirmwarePrepareReply<'_>>(bytes)
        .context("decode FirmwarePrepareReply")?;
    require_accepted(reply.result(), "prepare")
}

fn accept_chunk_reply(bytes: &[u8], operation: &str) -> Result<()> {
    let reply =
        flatbuffers::root::<FirmwareChunkReply<'_>>(bytes).context("decode FirmwareChunkReply")?;
    require_accepted(reply.result(), operation)
}

fn accept_commit_reply(bytes: &[u8]) -> Result<()> {
    let reply = flatbuffers::root::<FirmwareCommitReply<'_>>(bytes)
        .context("decode FirmwareCommitReply")?;
    require_accepted(reply.result(), "commit")
}

fn accept_status_reply(bytes: &[u8]) -> Result<()> {
    let reply = flatbuffers::root::<FirmwareStatusReply<'_>>(bytes)
        .context("decode FirmwareStatusReply")?;
    require_accepted(reply.result(), "status")
}

pub fn query_payload(
    session: &Session,
    key: &str,
    payload: Vec<u8>,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let mut request = session.get(key).payload(payload).timeout(timeout);
    // Stamp the synapse request contract when the key names a catalog
    // command (`<ns>/cmd/firmware_<op>`).
    let command_name = key.rsplit('/').next().unwrap_or(key);
    if let Some(encoding) = crate::policy::command_request_encoding(command_name) {
        request = request.encoding(zenoh::bytes::Encoding::from(encoding));
    }
    let replies = request
        .wait()
        .map_err(|err| anyhow!("send firmware query {key}: {err}"))?;
    let reply = replies
        .recv_timeout(timeout)
        .map_err(|err| anyhow!("receive firmware reply {key}: {err}"))?
        .ok_or_else(|| anyhow!("no firmware reply from {key} within {timeout:?}"))?;
    let sample = reply
        .into_result()
        .map_err(|err| anyhow!("firmware query {key} returned an error: {err}"))?;
    Ok(sample.payload().to_bytes().to_vec())
}

fn firmware_key(prefix: &str, operation: &str) -> String {
    format!("{}_{}", prefix.trim_end_matches('/'), operation)
}

/// Transfer an already validated firmware candidate through the receiver's
/// info/prepare/chunk/commit/status query protocol.
#[allow(clippy::excessive_nesting)]
pub fn transfer_firmware<F>(
    session: &Session,
    config: &TransferConfig,
    update_id: &str,
    version: &str,
    image: &[u8],
    windows: &[GainWindow],
    progress: F,
) -> Result<TransferResult>
where
    F: Fn(&str, u8, &str),
{
    if image.is_empty() {
        bail!("firmware image is empty");
    }
    progress("connect", 0, "Checking firmware update service.");
    let info = query_payload(
        session,
        &firmware_key(&config.key_prefix, "info"),
        build_info_request(&config.target),
        config.timeout,
    )?;
    let receiver_max = accept_info_reply(&info)? as usize;
    let chunk_size = if receiver_max > 0 {
        config.chunk_size.min(receiver_max)
    } else {
        config.chunk_size
    }
    .max(128);

    let image_sha256 = full_sha256(image);
    let selective_sha256 = selective_sha256(image, windows);
    let chunk_count = image.len().div_ceil(chunk_size).max(1);
    let prepare = query_payload(
        session,
        &firmware_key(&config.key_prefix, "prepare"),
        build_prepare_request(
            update_id,
            &config.target,
            &config.board_id,
            version,
            image.len(),
            &image_sha256,
            &selective_sha256,
            chunk_size,
            chunk_count,
        ),
        config.timeout,
    )?;
    accept_prepare_reply(&prepare)?;
    progress("prepare", 5, "Firmware update prepared.");

    for (chunk_index, data) in image.chunks(chunk_size).enumerate() {
        let offset = chunk_index * chunk_size;
        let final_chunk = chunk_index + 1 == chunk_count;
        let payload =
            build_chunk_request_with_hash(update_id, chunk_index, offset, data, final_chunk);
        let mut last_error = None;
        for attempt in 1..=config.retries.max(1) {
            match query_payload(
                session,
                &firmware_key(&config.key_prefix, "chunk"),
                payload.clone(),
                config.timeout,
            )
            .and_then(|reply| accept_chunk_reply(&reply, &format!("chunk {chunk_index}")))
            {
                Ok(_) => {
                    last_error = None;
                    break;
                }
                Err(err) => {
                    last_error = Some(err);
                    if attempt < config.retries.max(1) {
                        tracing::warn!(chunk_index, attempt, "firmware chunk retry");
                    }
                }
            }
        }
        if let Some(err) = last_error {
            return Err(err).with_context(|| format!("chunk {chunk_index} failed"));
        }
        let pct = (5 + (((chunk_index + 1) * 85) / chunk_count)).min(90) as u8;
        progress(
            "chunk",
            pct,
            &format!("Chunk {}/{} accepted.", chunk_index + 1, chunk_count),
        );
    }

    progress("commit", 92, "Finalizing firmware update.");
    let commit = query_payload(
        session,
        &firmware_key(&config.key_prefix, "commit"),
        build_commit_request(update_id, &image_sha256),
        config.timeout,
    )?;
    accept_commit_reply(&commit)?;
    progress("commit", 96, "Firmware image committed.");

    let status = query_payload(
        session,
        &firmware_key(&config.key_prefix, "status"),
        build_status_request(update_id),
        config.timeout,
    )?;
    accept_status_reply(&status)?;
    progress("complete", 100, "Firmware update transfer complete.");

    Ok(TransferResult {
        image_sha256,
        chunk_count,
    })
}

fn build_chunk_request_with_hash(
    update_id: &str,
    chunk_index: usize,
    offset: usize,
    data: &[u8],
    final_chunk: bool,
) -> Vec<u8> {
    let chunk_hash = full_sha256(data);
    let mut builder = FlatBufferBuilder::with_capacity(256 + data.len());
    let update_id = builder.create_string(update_id);
    let data = builder.create_vector(data);
    let chunk_sha256 = builder.create_string(&chunk_hash);
    let table = FirmwareChunkRequest::create(
        &mut builder,
        &FirmwareChunkRequestArgs {
            update_id: Some(update_id),
            chunk_index: chunk_index as u32,
            offset: offset as u64,
            data: Some(data),
            chunk_sha256: Some(chunk_sha256),
            final_chunk,
        },
    );
    finish_table(&mut builder, table)
}

/// Resolve gain windows to merged, sorted, clamped byte ranges — matches
/// `resolveGainWindows` (including adjacent-range merging).
pub fn resolve_gain_windows(length: usize, windows: &[GainWindow]) -> Vec<Range> {
    let mut ranges: Vec<Range> = windows
        .iter()
        .filter_map(|w| {
            if w.length == 0 {
                return None;
            }
            // origin "end" counts back from the tail; negative clamps to 0.
            let start = match w.origin {
                Origin::End => length.saturating_sub(w.offset),
                Origin::Start => w.offset,
            };
            let end = (start + w.length).min(length);
            if start >= length || end <= start {
                return None;
            }
            Some(Range { start, end })
        })
        .collect();

    ranges.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));

    let mut merged: Vec<Range> = Vec::new();
    for range in ranges {
        match merged.last_mut() {
            // New window only when it starts strictly past the last one's end;
            // touching (start == end) ranges merge, as in the JS.
            Some(last) if range.start <= last.end => {
                if range.end > last.end {
                    last.end = range.end;
                }
            }
            _ => merged.push(range),
        }
    }
    merged
}

/// Validate that a candidate differs from the baseline only inside gain windows.
pub fn validate_gain_only_change(
    candidate: &[u8],
    baseline: &[u8],
    windows: &[GainWindow],
) -> GainValidation {
    if candidate.len() != baseline.len() {
        return GainValidation {
            allowed: false,
            reason: format!(
                "File size changed from {} to {} bytes.",
                baseline.len(),
                candidate.len()
            ),
            baseline_size: baseline.len(),
            candidate_size: candidate.len(),
            differing_byte_count: 0,
            gain_differing_byte_count: 0,
            outside_gain_diffs: Vec::new(),
        };
    }

    let ranges = resolve_gain_windows(baseline.len(), windows);
    let inside = |index: usize| ranges.iter().any(|r| index >= r.start && index < r.end);

    let mut differing_byte_count = 0;
    let mut gain_differing_byte_count = 0;
    let mut outside_gain_diffs: Vec<OutsideDiff> = Vec::new();

    for index in 0..baseline.len() {
        if baseline[index] == candidate[index] {
            continue;
        }
        differing_byte_count += 1;
        if inside(index) {
            gain_differing_byte_count += 1;
        } else if outside_gain_diffs.len() < 16 {
            outside_gain_diffs.push(OutsideDiff {
                offset: index,
                baseline: baseline[index],
                candidate: candidate[index],
            });
        }
    }

    let allowed = outside_gain_diffs.is_empty();
    GainValidation {
        allowed,
        reason: if allowed {
            "Only authorized gain-window bytes changed.".to_string()
        } else {
            "Binary changes include bytes outside the authorized gain windows.".to_string()
        },
        baseline_size: baseline.len(),
        candidate_size: candidate.len(),
        differing_byte_count,
        gain_differing_byte_count,
        outside_gain_diffs,
    }
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn full_sha256(buffer: &[u8]) -> String {
    hex(Sha256::digest(buffer))
}

/// SHA-256 over the buffer with gain-window bytes excised — stable regardless of
/// what a legal gain change writes. Matches `buildBinaryMetadata.selectiveSha256`.
pub fn selective_sha256(buffer: &[u8], windows: &[GainWindow]) -> String {
    let ranges = resolve_gain_windows(buffer.len(), windows);
    let mut hasher = Sha256::new();
    let mut cursor = 0;
    for range in &ranges {
        if range.start > cursor {
            hasher.update(&buffer[cursor..range.start]);
        }
        cursor = range.end;
    }
    if cursor < buffer.len() {
        hasher.update(&buffer[cursor..]);
    }
    hex(hasher.finalize())
}

/// Parse gain windows from JSON (`[{origin,offset,length}, ...]`), the same shape
/// the Node config and artifacts use.
pub fn parse_gain_windows(json: &str) -> Vec<GainWindow> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let items = value
        .get("gainWindows")
        .and_then(|v| v.as_array())
        .or_else(|| value.as_array());
    let Some(items) = items else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let length = item.get("length").and_then(|v| v.as_u64())? as usize;
            if length == 0 {
                return None;
            }
            let offset = item.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let origin = match item.get("origin").and_then(|v| v.as_str()) {
                Some("start") => Origin::Start,
                _ => Origin::End,
            };
            Some(GainWindow {
                origin,
                offset,
                length,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOWS: &[GainWindow] = &[GainWindow {
        origin: Origin::Start,
        offset: 4,
        length: 2,
    }];

    #[test]
    fn identical_is_allowed() {
        let base = vec![1u8; 16];
        let result = validate_gain_only_change(&base, &base, WINDOWS);
        assert!(result.allowed);
        assert_eq!(result.differing_byte_count, 0);
    }

    #[test]
    fn gain_only_change_is_allowed() {
        let base = vec![0u8; 16];
        let mut candidate = base.clone();
        candidate[4] = 9; // inside window [4,6)
        candidate[5] = 9;
        let result = validate_gain_only_change(&candidate, &base, WINDOWS);
        assert!(result.allowed);
        assert_eq!(result.differing_byte_count, 2);
        assert_eq!(result.gain_differing_byte_count, 2);
    }

    #[test]
    fn outside_change_is_rejected() {
        let base = vec![0u8; 16];
        let mut candidate = base.clone();
        candidate[10] = 9; // outside the window
        let result = validate_gain_only_change(&candidate, &base, WINDOWS);
        assert!(!result.allowed);
        assert_eq!(result.outside_gain_diffs.len(), 1);
        assert_eq!(result.outside_gain_diffs[0].offset, 10);
    }

    #[test]
    fn size_change_is_rejected() {
        let result = validate_gain_only_change(&[0u8; 8], &[0u8; 16], WINDOWS);
        assert!(!result.allowed);
        assert!(result.reason.contains("File size changed"));
    }

    #[test]
    fn selective_hash_is_stable_across_gain_change() {
        let base = vec![0u8; 16];
        let mut candidate = base.clone();
        candidate[4] = 42;
        candidate[5] = 42;
        assert_ne!(full_sha256(&base), full_sha256(&candidate));
        assert_eq!(
            selective_sha256(&base, WINDOWS),
            selective_sha256(&candidate, WINDOWS)
        );
    }

    #[test]
    fn firmware_upload_tables_round_trip_through_schema_layout() {
        let prepare = build_prepare_request(
            "update-1",
            "",
            "",
            "candidate.bin",
            1025,
            &"a".repeat(64),
            "",
            512,
            3,
        );
        let decoded_prepare = decode_firmware_prepare_upload(&prepare).unwrap();
        assert_eq!(decoded_prepare.update_id, "update-1");
        assert_eq!(decoded_prepare.version, "candidate.bin");
        assert_eq!(decoded_prepare.image_size, 1025);
        assert_eq!(decoded_prepare.image_sha256, "a".repeat(64));
        assert_eq!(decoded_prepare.requested_chunk_size, 512);
        assert_eq!(decoded_prepare.chunk_count, 3);

        let data = b"typed firmware chunk";
        let chunk = build_chunk_request_with_hash("update-1", 2, 1024, data, true);
        let decoded_chunk = decode_firmware_chunk_upload(&chunk).unwrap();
        assert_eq!(decoded_chunk.update_id, "update-1");
        assert_eq!(decoded_chunk.chunk_index, 2);
        assert_eq!(decoded_chunk.offset, 1024);
        assert_eq!(decoded_chunk.data, data);
        assert_eq!(decoded_chunk.chunk_sha256, full_sha256(data));
        assert!(decoded_chunk.final_chunk);

        let commit = build_commit_request("update-1", &"b".repeat(64));
        let decoded_commit = decode_firmware_commit_upload(&commit).unwrap();
        assert_eq!(decoded_commit.update_id, "update-1");
        assert_eq!(decoded_commit.image_sha256, "b".repeat(64));
    }

    #[test]
    fn malformed_firmware_upload_table_is_rejected() {
        assert!(decode_firmware_prepare_upload(&[4, 0, 0, 0]).is_err());
        assert!(decode_firmware_chunk_upload(&[255; 12]).is_err());
        assert!(decode_firmware_commit_upload(&[]).is_err());
    }

    #[test]
    fn firmware_service_keys_use_canonical_schema_names() {
        assert_eq!(
            firmware_key("cmd/firmware", "prepare"),
            "cmd/firmware_prepare"
        );
    }

    #[test]
    fn parses_gain_windows_json() {
        let json = r#"{"gainWindows":[{"origin":"start","offset":18480,"length":8}]}"#;
        let windows = parse_gain_windows(json);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].origin, Origin::Start);
        assert_eq!(windows[0].offset, 18480);
        assert_eq!(windows[0].length, 8);
    }

    #[test]
    fn repository_cubs2_fixtures_match_policy_when_present() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let Some(repository_root) = manifest_dir.ancestors().nth(4) else {
            return;
        };
        let artifacts = repository_root.join("artifacts/cubs2-firmware");
        let baseline_path = artifacts.join("baseline-cubs2-mr-vmu-tropic.bin");
        if !baseline_path.exists() {
            return;
        }

        let baseline = std::fs::read(baseline_path).unwrap();
        let accepted = std::fs::read(artifacts.join("candidate-cubs2-gain-only.bin")).unwrap();
        let rejected =
            std::fs::read(artifacts.join("candidate-cubs2-rejected-outside-gain.bin")).unwrap();
        let windows = parse_gain_windows(
            &std::fs::read_to_string(artifacts.join("gain-windows-cubs2.json")).unwrap(),
        );

        let accepted_result = validate_gain_only_change(&accepted, &baseline, &windows);
        assert!(accepted_result.allowed, "{}", accepted_result.reason);
        assert!(accepted_result.differing_byte_count > 0);
        assert_eq!(
            accepted_result.differing_byte_count,
            accepted_result.gain_differing_byte_count
        );

        let rejected_result = validate_gain_only_change(&rejected, &baseline, &windows);
        assert!(!rejected_result.allowed);
        assert!(!rejected_result.outside_gain_diffs.is_empty());
        assert_eq!(
            full_sha256(&accepted),
            "1a967afa3f54586ecb099d815d391a473774a406813ec969dfe6cbd0a67d3484"
        );
    }
}

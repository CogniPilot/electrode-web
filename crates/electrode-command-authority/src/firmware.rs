//! Firmware gain-window checksum validation — the GCS's second security gate.
//!
//! A candidate firmware image is accepted only if it is byte-identical to a trusted baseline
//! *except* inside authorized "gain windows" (small regions where tuning gains
//! live). Anything else — a size change or a single byte flipped outside a
//! window — is rejected, so the upload surface can only re-tune gains, not
//! replace firmware.

use anyhow::{anyhow, bail, Context, Result};
use flatbuffers::{FlatBufferBuilder, VOffsetT, WIPOffset};
use sha2::{Digest, Sha256};
use std::time::Duration;
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

const RESULT_ACCEPTED: u8 = 0;
const RESULT_IN_PROGRESS: u8 = 5;

fn slot(index: usize) -> VOffsetT {
    (4 + index * 2) as VOffsetT
}

fn finish_table(
    builder: &mut FlatBufferBuilder<'_>,
    table: WIPOffset<flatbuffers::TableFinishedWIPOffset>,
) -> Vec<u8> {
    builder.finish(table, None);
    builder.finished_data().to_vec()
}

fn build_info_request(target: &str) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let target = builder.create_string(target);
    let start = builder.start_table();
    builder.push_slot_always(slot(0), target);
    let table = builder.end_table(start);
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
    let start = builder.start_table();
    builder.push_slot_always(slot(0), update_id);
    builder.push_slot_always(slot(1), target);
    builder.push_slot_always(slot(2), board_id);
    builder.push_slot_always(slot(3), version);
    builder.push_slot(slot(4), image_size as u64, 0);
    builder.push_slot_always(slot(5), image_sha256);
    builder.push_slot_always(slot(6), selective_sha256);
    builder.push_slot(slot(7), chunk_size as u32, 0);
    builder.push_slot(slot(8), chunk_count as u32, 0);
    builder.push_slot_always(slot(10), manifest);
    let table = builder.end_table(start);
    finish_table(&mut builder, table)
}

fn build_commit_request(update_id: &str, image_sha256: &str) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let update_id = builder.create_string(update_id);
    let image_sha256 = builder.create_string(image_sha256);
    let start = builder.start_table();
    builder.push_slot_always(slot(0), update_id);
    builder.push_slot_always(slot(1), image_sha256);
    let table = builder.end_table(start);
    finish_table(&mut builder, table)
}

fn build_status_request(update_id: &str) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let update_id = builder.create_string(update_id);
    let start = builder.start_table();
    builder.push_slot_always(slot(0), update_id);
    let table = builder.end_table(start);
    finish_table(&mut builder, table)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let raw: [u8; 2] = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| anyhow!("FlatBuffer u16 is out of range"))?
        .try_into()?;
    Ok(u16::from_le_bytes(raw))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let raw: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| anyhow!("FlatBuffer u32 is out of range"))?
        .try_into()?;
    Ok(u32::from_le_bytes(raw))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    let raw: [u8; 8] = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| anyhow!("FlatBuffer u64 is out of range"))?
        .try_into()?;
    Ok(u64::from_le_bytes(raw))
}

fn field_position(bytes: &[u8], field: usize) -> Result<Option<usize>> {
    let table = read_u32(bytes, 0)? as usize;
    if table < 4 || table + 4 > bytes.len() {
        bail!("FlatBuffer root table offset is invalid");
    }
    let vtable_distance = read_u32(bytes, table)? as usize;
    let vtable = table
        .checked_sub(vtable_distance)
        .ok_or_else(|| anyhow!("FlatBuffer vtable offset is invalid"))?;
    let vtable_size = read_u16(bytes, vtable)? as usize;
    let object_size = read_u16(bytes, vtable + 2)? as usize;
    if vtable_size < 4
        || vtable + vtable_size > bytes.len()
        || object_size < 4
        || table + object_size > bytes.len()
    {
        bail!("FlatBuffer table bounds are invalid");
    }
    let entry = 4 + field * 2;
    if entry + 2 > vtable_size {
        return Ok(None);
    }
    let field_offset = read_u16(bytes, vtable + entry)? as usize;
    if field_offset == 0 {
        return Ok(None);
    }
    if field_offset >= object_size {
        bail!("FlatBuffer field lies outside its table object");
    }
    let position = table + field_offset;
    if position >= bytes.len() {
        bail!("FlatBuffer field offset is out of range");
    }
    Ok(Some(position))
}

fn table_string(bytes: &[u8], field: usize) -> Result<String> {
    let Some(position) = field_position(bytes, field)? else {
        return Ok(String::new());
    };
    let target = position
        .checked_add(read_u32(bytes, position)? as usize)
        .ok_or_else(|| anyhow!("FlatBuffer string offset overflow"))?;
    let length = read_u32(bytes, target)? as usize;
    let start = target
        .checked_add(4)
        .ok_or_else(|| anyhow!("FlatBuffer string offset overflow"))?;
    let end = start
        .checked_add(length)
        .ok_or_else(|| anyhow!("FlatBuffer string length overflow"))?;
    let value = bytes
        .get(start..end)
        .ok_or_else(|| anyhow!("FlatBuffer string is out of range"))?;
    String::from_utf8(value.to_vec())
        .map_err(|err| anyhow!("FlatBuffer string is not UTF-8: {err}"))
}

fn table_bytes(bytes: &[u8], field: usize) -> Result<Vec<u8>> {
    let Some(position) = field_position(bytes, field)? else {
        return Ok(Vec::new());
    };
    let target = position
        .checked_add(read_u32(bytes, position)? as usize)
        .ok_or_else(|| anyhow!("FlatBuffer vector offset overflow"))?;
    let length = read_u32(bytes, target)? as usize;
    let start = target
        .checked_add(4)
        .ok_or_else(|| anyhow!("FlatBuffer vector offset overflow"))?;
    let end = start
        .checked_add(length)
        .ok_or_else(|| anyhow!("FlatBuffer vector length overflow"))?;
    Ok(bytes
        .get(start..end)
        .ok_or_else(|| anyhow!("FlatBuffer vector is out of range"))?
        .to_vec())
}

fn table_u32(bytes: &[u8], field: usize) -> Result<u32> {
    match field_position(bytes, field)? {
        Some(position) => read_u32(bytes, position),
        None => Ok(0),
    }
}

fn table_u64(bytes: &[u8], field: usize) -> Result<u64> {
    match field_position(bytes, field)? {
        Some(position) => read_u64(bytes, position),
        None => Ok(0),
    }
}

fn table_bool(bytes: &[u8], field: usize) -> Result<bool> {
    match field_position(bytes, field)? {
        Some(position) => Ok(*bytes
            .get(position)
            .ok_or_else(|| anyhow!("FlatBuffer bool is out of range"))?
            != 0),
        None => Ok(false),
    }
}

pub fn decode_firmware_prepare_upload(bytes: &[u8]) -> Result<FirmwarePrepareUpload> {
    Ok(FirmwarePrepareUpload {
        update_id: table_string(bytes, 0)?,
        version: table_string(bytes, 3)?,
        image_size: table_u64(bytes, 4)?,
        image_sha256: table_string(bytes, 5)?,
        requested_chunk_size: table_u32(bytes, 7)?,
        chunk_count: table_u32(bytes, 8)?,
    })
}

pub fn decode_firmware_chunk_upload(bytes: &[u8]) -> Result<FirmwareChunkUpload> {
    Ok(FirmwareChunkUpload {
        update_id: table_string(bytes, 0)?,
        chunk_index: table_u32(bytes, 1)?,
        offset: table_u64(bytes, 2)?,
        data: table_bytes(bytes, 3)?,
        chunk_sha256: table_string(bytes, 4)?,
        final_chunk: table_bool(bytes, 5)?,
    })
}

pub fn decode_firmware_commit_upload(bytes: &[u8]) -> Result<FirmwareCommitUpload> {
    Ok(FirmwareCommitUpload {
        update_id: table_string(bytes, 0)?,
        image_sha256: table_string(bytes, 1)?,
    })
}

fn reply_u8(bytes: &[u8], field: usize, default: u8) -> Result<u8> {
    Ok(match field_position(bytes, field)? {
        Some(position) => *bytes
            .get(position)
            .ok_or_else(|| anyhow!("FlatBuffer u8 is out of range"))?,
        None => default,
    })
}

fn reply_u32(bytes: &[u8], field: usize, default: u32) -> Result<u32> {
    match field_position(bytes, field)? {
        Some(position) => read_u32(bytes, position),
        None => Ok(default),
    }
}

fn require_accepted(bytes: &[u8], operation: &str) -> Result<()> {
    let result = reply_u8(bytes, 0, RESULT_ACCEPTED)?;
    if result == RESULT_ACCEPTED || result == RESULT_IN_PROGRESS {
        return Ok(());
    }
    bail!("{operation} rejected with firmware result code {result}")
}

pub fn query_payload(
    session: &Session,
    key: &str,
    payload: Vec<u8>,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let replies = session
        .get(key)
        .payload(payload)
        .timeout(timeout)
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
    require_accepted(&info, "info")?;
    let receiver_max = reply_u32(&info, 8, config.chunk_size as u32)? as usize;
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
    require_accepted(&prepare, "prepare")?;
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
            .and_then(|reply| {
                require_accepted(&reply, &format!("chunk {chunk_index}"))?;
                Ok(reply)
            }) {
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
    require_accepted(&commit, "commit")?;
    progress("commit", 96, "Firmware image committed.");

    let status = query_payload(
        session,
        &firmware_key(&config.key_prefix, "status"),
        build_status_request(update_id),
        config.timeout,
    )?;
    require_accepted(&status, "status")?;
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
    let start = builder.start_table();
    builder.push_slot_always(slot(0), update_id);
    builder.push_slot(slot(1), chunk_index as u32, 0);
    builder.push_slot(slot(2), offset as u64, 0);
    builder.push_slot_always(slot(3), data);
    builder.push_slot_always(slot(4), chunk_sha256);
    builder.push_slot(slot(5), final_chunk, false);
    let table = builder.end_table(start);
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
            firmware_key("synapse/v1/cmd/firmware", "prepare"),
            "synapse/v1/cmd/firmware_prepare"
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

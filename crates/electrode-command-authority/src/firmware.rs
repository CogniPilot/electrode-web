//! Firmware parameter-region validation — the GCS's second security gate.
//!
//! A candidate firmware image is accepted only if it is byte-identical to a trusted baseline
//! *except* for substitutions inside explicitly authorized regions containing
//! autopilot parameters. Added, removed, or changed bytes elsewhere are
//! outside that parameter-only change set and are rejected.

use anyhow::{anyhow, bail, Context, Result};
use flatbuffers::{FlatBufferBuilder, WIPOffset};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
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

const PARAMETER_MANIFEST_SCHEMA: &str = "electrode.autopilot-parameter-policy.v1";
const PARAMETER_LIMITS: [(&str, f64, f64); 6] = [
    ("route.crossTrackSteeringDistance", 0.25, 50.0),
    ("route.waypointSwitchingDistance", 0.1, 50.0),
    ("attitude.rollLimit", 0.05, 1.2),
    ("attitude.headingPid.kp", 0.0, 10.0),
    ("attitude.headingPid.ki", 0.0, 10.0),
    ("attitude.headingPid.kd", 0.0, 10.0),
];

#[derive(Clone, Debug, PartialEq)]
pub struct ParameterRegion {
    pub name: String,
    pub offset: usize,
    pub length: usize,
    pub minimum: f64,
    pub maximum: f64,
}

#[derive(Clone, Debug)]
pub struct ParameterPolicy {
    pub baseline_sha256: String,
    pub target: String,
    pub board_id: String,
    pub regions: Vec<ParameterRegion>,
    pub manifest_json: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub start: usize,
    pub end: usize,
}

/// A byte that differs outside every authorized autopilot-parameter region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutsideDiff {
    pub offset: usize,
    pub baseline: Option<u8>,
    pub candidate: Option<u8>,
}

#[derive(Clone, Debug)]
pub struct ParameterValidation {
    pub allowed: bool,
    pub reason: String,
    pub baseline_size: usize,
    pub candidate_size: usize,
    pub differing_byte_count: usize,
    pub parameter_differing_byte_count: usize,
    pub outside_parameter_diffs: Vec<OutsideDiff>,
    pub invalid_parameters: Vec<String>,
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
    trusted_manifest: &str,
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
    let manifest = builder.create_string(trusted_manifest);
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
    regions: &[ParameterRegion],
    trusted_manifest: &str,
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
    let selective_sha256 = selective_sha256(image, regions);
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
            trusted_manifest,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ParameterManifestFile {
    schema: String,
    target: String,
    board_id: String,
    baseline_sha256: String,
    parameters: Vec<ParameterManifestRegion>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParameterManifestRegion {
    name: String,
    offset: u64,
    encoding: String,
    minimum: f64,
    maximum: f64,
}

fn parameter_limits(name: &str) -> Option<(f64, f64)> {
    PARAMETER_LIMITS
        .iter()
        .find(|(candidate, _, _)| *candidate == name)
        .map(|(_, minimum, maximum)| (*minimum, *maximum))
}

/// Parse an organizer-owned manifest and bind its declared parameter offsets
/// to one exact baseline image. A manifest may authorize a non-empty subset of
/// the six known parameters when the compiled image does not materialize every
/// parameter as an independently writable value.
pub fn parse_parameter_policy(
    json: &str,
    baseline: &[u8],
    expected_target: &str,
    expected_board_id: &str,
) -> Result<ParameterPolicy> {
    let raw: ParameterManifestFile =
        serde_json::from_str(json).context("parse autopilot parameter manifest")?;
    if raw.schema != PARAMETER_MANIFEST_SCHEMA {
        bail!("unsupported autopilot parameter manifest schema");
    }
    if raw.target != expected_target {
        bail!("autopilot parameter manifest target mismatch");
    }
    if !expected_board_id.is_empty() && raw.board_id != expected_board_id {
        bail!("autopilot parameter manifest board mismatch");
    }
    if raw.baseline_sha256.len() != 64
        || !raw
            .baseline_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || raw.baseline_sha256 != full_sha256(baseline)
    {
        bail!("autopilot parameter manifest baseline hash mismatch");
    }
    if raw.parameters.is_empty() || raw.parameters.len() > PARAMETER_LIMITS.len() {
        bail!("autopilot parameter manifest must contain one to six parameters");
    }

    let mut names = HashSet::new();
    let mut regions = Vec::with_capacity(raw.parameters.len());
    for parameter in raw.parameters {
        let Some((minimum, maximum)) = parameter_limits(&parameter.name) else {
            bail!("unknown autopilot parameter {}", parameter.name);
        };
        if !names.insert(parameter.name.clone()) {
            bail!("duplicate autopilot parameter {}", parameter.name);
        }
        if parameter.encoding != "float64-le"
            || parameter.minimum != minimum
            || parameter.maximum != maximum
        {
            bail!(
                "autopilot parameter metadata mismatch for {}",
                parameter.name
            );
        }
        let offset = usize::try_from(parameter.offset)
            .context("autopilot parameter offset does not fit this host")?;
        let end = offset
            .checked_add(8)
            .context("autopilot parameter offset overflow")?;
        if end > baseline.len() {
            bail!("autopilot parameter offset is outside the baseline");
        }
        let bytes: [u8; 8] = baseline[offset..end]
            .try_into()
            .expect("validated eight-byte parameter region");
        let value = f64::from_le_bytes(bytes);
        if !value.is_finite() || value < minimum || value > maximum {
            bail!("baseline autopilot parameter value is invalid");
        }
        regions.push(ParameterRegion {
            name: parameter.name,
            offset,
            length: 8,
            minimum,
            maximum,
        });
    }
    regions.sort_by_key(|region| region.offset);
    if regions
        .windows(2)
        .any(|pair| pair[0].offset + pair[0].length > pair[1].offset)
    {
        bail!("autopilot parameter regions overlap");
    }

    let manifest_json = serde_json::to_string(
        &serde_json::from_str::<serde_json::Value>(json)
            .context("canonicalize autopilot parameter manifest")?,
    )?;
    Ok(ParameterPolicy {
        baseline_sha256: raw.baseline_sha256,
        target: raw.target,
        board_id: raw.board_id,
        regions,
        manifest_json,
    })
}

pub fn resolve_parameter_regions(length: usize, regions: &[ParameterRegion]) -> Vec<Range> {
    let mut ranges: Vec<Range> = regions
        .iter()
        .filter_map(|region| {
            let end = region.offset.checked_add(region.length)?;
            (region.length > 0 && end <= length).then_some(Range {
                start: region.offset,
                end,
            })
        })
        .collect();
    ranges.sort_by_key(|range| range.start);
    ranges
}

/// Accept the complete candidate only when every changed byte is a
/// substitution in one of the configured parameter fields and every decoded
/// value is finite and within the compiled policy range.
pub fn validate_parameter_only_change(
    candidate: &[u8],
    baseline: &[u8],
    regions: &[ParameterRegion],
) -> ParameterValidation {
    let ranges = resolve_parameter_regions(baseline.len(), regions);
    let inside = |index: usize| ranges.iter().any(|r| index >= r.start && index < r.end);

    let mut differing_byte_count = 0;
    let mut parameter_differing_byte_count = 0;
    let mut outside_parameter_diffs: Vec<OutsideDiff> = Vec::new();

    for index in 0..baseline.len().max(candidate.len()) {
        let baseline_byte = baseline.get(index).copied();
        let candidate_byte = candidate.get(index).copied();
        if baseline_byte == candidate_byte {
            continue;
        }
        differing_byte_count += 1;
        if baseline_byte.is_some() && candidate_byte.is_some() && inside(index) {
            parameter_differing_byte_count += 1;
        } else if outside_parameter_diffs.len() < 16 {
            outside_parameter_diffs.push(OutsideDiff {
                offset: index,
                baseline: baseline_byte,
                candidate: candidate_byte,
            });
        }
    }

    let mut invalid_parameters = Vec::new();
    for region in regions {
        let Some(end) = region.offset.checked_add(8) else {
            invalid_parameters.push(region.name.clone());
            continue;
        };
        let Some(bytes) = candidate.get(region.offset..end) else {
            invalid_parameters.push(region.name.clone());
            continue;
        };
        let value = f64::from_le_bytes(
            bytes
                .try_into()
                .expect("validated eight-byte candidate parameter region"),
        );
        if !value.is_finite() || value < region.minimum || value > region.maximum {
            invalid_parameters.push(region.name.clone());
        }
    }

    let allowed = outside_parameter_diffs.is_empty() && invalid_parameters.is_empty();
    let reason = if allowed {
        "Only authorized autopilot parameter values changed."
    } else if !outside_parameter_diffs.is_empty() {
        "Binary changes are not limited to authorized autopilot parameters."
    } else {
        "One or more autopilot parameter values are invalid."
    };
    ParameterValidation {
        allowed,
        reason: reason.to_string(),
        baseline_size: baseline.len(),
        candidate_size: candidate.len(),
        differing_byte_count,
        parameter_differing_byte_count,
        outside_parameter_diffs,
        invalid_parameters,
    }
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn full_sha256(buffer: &[u8]) -> String {
    hex(Sha256::digest(buffer))
}

/// SHA-256 over the buffer with parameter-region bytes excised — stable
/// regardless of what a legal parameter change writes.
pub fn selective_sha256(buffer: &[u8], regions: &[ParameterRegion]) -> String {
    let ranges = resolve_parameter_regions(buffer.len(), regions);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_baseline() -> Vec<u8> {
        let mut baseline = vec![0u8; 128];
        for (index, value) in [4.25_f64, 4.0, 0.5, 0.5, 0.0, 0.5].into_iter().enumerate() {
            baseline[16 + index * 8..24 + index * 8].copy_from_slice(&value.to_le_bytes());
        }
        baseline
    }

    fn test_manifest(baseline: &[u8]) -> String {
        format!(
            r#"{{
                "schema":"electrode.autopilot-parameter-policy.v1",
                "target":"cubs2",
                "boardId":"mr_vmu_tropic",
                "baselineSha256":"{}",
                "parameters":[
                    {{"name":"route.crossTrackSteeringDistance","offset":16,"encoding":"float64-le","minimum":0.25,"maximum":50.0}},
                    {{"name":"route.waypointSwitchingDistance","offset":24,"encoding":"float64-le","minimum":0.1,"maximum":50.0}},
                    {{"name":"attitude.rollLimit","offset":32,"encoding":"float64-le","minimum":0.05,"maximum":1.2}},
                    {{"name":"attitude.headingPid.kp","offset":40,"encoding":"float64-le","minimum":0.0,"maximum":10.0}},
                    {{"name":"attitude.headingPid.ki","offset":48,"encoding":"float64-le","minimum":0.0,"maximum":10.0}},
                    {{"name":"attitude.headingPid.kd","offset":56,"encoding":"float64-le","minimum":0.0,"maximum":10.0}}
                ]
            }}"#,
            full_sha256(baseline)
        )
    }

    fn test_policy(baseline: &[u8]) -> ParameterPolicy {
        parse_parameter_policy(&test_manifest(baseline), baseline, "cubs2", "mr_vmu_tropic")
            .unwrap()
    }

    fn roll_only_manifest(baseline: &[u8]) -> String {
        format!(
            r#"{{
                "schema":"electrode.autopilot-parameter-policy.v1",
                "target":"cubs2",
                "boardId":"mr_vmu_tropic",
                "baselineSha256":"{}",
                "parameters":[
                    {{"name":"attitude.rollLimit","offset":32,"encoding":"float64-le","minimum":0.05,"maximum":1.2}}
                ]
            }}"#,
            full_sha256(baseline)
        )
    }

    #[test]
    fn identical_is_allowed() {
        let base = test_baseline();
        let policy = test_policy(&base);
        let result = validate_parameter_only_change(&base, &base, &policy.regions);
        assert!(result.allowed);
        assert_eq!(result.differing_byte_count, 0);
    }

    #[test]
    fn parameter_only_change_is_allowed() {
        let base = test_baseline();
        let policy = test_policy(&base);
        let mut candidate = base.clone();
        candidate[16..24].copy_from_slice(&5.0f64.to_le_bytes());
        let result = validate_parameter_only_change(&candidate, &base, &policy.regions);
        assert!(result.allowed);
        assert!(result.differing_byte_count > 0);
        assert_eq!(
            result.differing_byte_count,
            result.parameter_differing_byte_count
        );
    }

    #[test]
    fn one_outside_change_rejects_the_whole_binary() {
        let base = test_baseline();
        let policy = test_policy(&base);
        let mut candidate = base.clone();
        candidate[16..24].copy_from_slice(&5.0f64.to_le_bytes());
        candidate[80] ^= 1;
        let result = validate_parameter_only_change(&candidate, &base, &policy.regions);
        assert!(!result.allowed);
        assert_eq!(result.outside_parameter_diffs.len(), 1);
        assert_eq!(result.outside_parameter_diffs[0].offset, 80);
    }

    #[test]
    fn length_change_is_an_outside_parameter_change() {
        let base = test_baseline();
        let policy = test_policy(&base);
        let result = validate_parameter_only_change(&base[..63], &base, &policy.regions);
        assert!(!result.allowed);
        assert_eq!(result.outside_parameter_diffs[0].offset, 63);
        assert_eq!(result.outside_parameter_diffs[0].candidate, None);
    }

    #[test]
    fn appended_bytes_are_outside_parameter_changes() {
        let base = test_baseline();
        let policy = test_policy(&base);
        let mut candidate = base.clone();
        candidate.extend_from_slice(&[0u8; 4]);
        let result = validate_parameter_only_change(&candidate, &base, &policy.regions);
        assert!(!result.allowed);
        assert_eq!(result.differing_byte_count, 4);
        assert_eq!(result.outside_parameter_diffs[0].offset, 128);
        assert_eq!(result.outside_parameter_diffs[0].baseline, None);
        assert_eq!(result.outside_parameter_diffs[0].candidate, Some(0));
    }

    #[test]
    fn nonfinite_and_out_of_range_parameter_values_are_rejected() {
        let base = test_baseline();
        let policy = test_policy(&base);
        let mut candidate = base.clone();
        candidate[16..24].copy_from_slice(&f64::NAN.to_le_bytes());
        let result = validate_parameter_only_change(&candidate, &base, &policy.regions);
        assert!(!result.allowed);
        assert_eq!(
            result.invalid_parameters,
            ["route.crossTrackSteeringDistance"]
        );

        candidate[16..24].copy_from_slice(&100.0f64.to_le_bytes());
        let result = validate_parameter_only_change(&candidate, &base, &policy.regions);
        assert!(!result.allowed);
    }

    #[test]
    fn selective_hash_is_stable_across_parameter_change() {
        let base = test_baseline();
        let policy = test_policy(&base);
        let mut candidate = base.clone();
        candidate[16..24].copy_from_slice(&5.0f64.to_le_bytes());
        assert_ne!(full_sha256(&base), full_sha256(&candidate));
        assert_eq!(
            selective_sha256(&base, &policy.regions),
            selective_sha256(&candidate, &policy.regions)
        );
    }

    #[test]
    fn manifest_is_bound_to_baseline_names_and_ranges() {
        let base = test_baseline();
        let manifest = test_manifest(&base);
        assert!(parse_parameter_policy(&manifest, &base, "cubs2", "mr_vmu_tropic").is_ok());
        assert!(parse_parameter_policy(&manifest, &[1u8; 128], "cubs2", "mr_vmu_tropic").is_err());
        assert!(parse_parameter_policy(
            &manifest.replace("50.0", "500.0"),
            &base,
            "cubs2",
            "mr_vmu_tropic"
        )
        .is_err());
        assert!(parse_parameter_policy(&manifest, &base, "other", "mr_vmu_tropic").is_err());
    }

    #[test]
    fn manifest_may_authorize_a_known_nonempty_subset() {
        let base = test_baseline();
        let policy =
            parse_parameter_policy(&roll_only_manifest(&base), &base, "cubs2", "mr_vmu_tropic")
                .unwrap();
        assert_eq!(policy.regions.len(), 1);
        assert_eq!(policy.regions[0].name, "attitude.rollLimit");

        let empty = roll_only_manifest(&base).replace(
            r#"{"name":"attitude.rollLimit","offset":32,"encoding":"float64-le","minimum":0.05,"maximum":1.2}"#,
            "",
        );
        assert!(parse_parameter_policy(&empty, &base, "cubs2", "mr_vmu_tropic").is_err());
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
            "{}",
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
}

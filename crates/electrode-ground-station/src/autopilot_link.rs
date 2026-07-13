//! Runs the native cubs2 autopilot and observes its direct localhost Zenoh
//! traffic. The firmware is itself a client of the Ground Station's private
//! vehicle router; this module does not relay or republish control traffic.

use std::io::Write;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zenoh::Wait;

use crate::autopilot::{AutopilotProfile, MOCAP_POSE_TOPIC};

const AUTOPILOT_OUTPUT_TOPICS: &[&str] = &[
    "pwm", "health", "att", "att_sp", "loop", "mission", "pos_sp", "traj", "nav",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AutopilotRunStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub started_at_ms: Option<u128>,
    pub message: String,
    pub binary: String,
    pub log_path: String,
    /// Frames forwarded autopilot → Zenoh since start.
    pub frames_out: u64,
    /// Frames forwarded Zenoh → autopilot since start.
    pub frames_in: u64,
    pub last_exit_at_ms: Option<u128>,
    pub last_exit_code: Option<i32>,
    pub last_exit_signal: Option<i32>,
    pub last_pid: Option<u32>,
    pub stop_requested: bool,
}

struct LinkChild {
    child: Child,
    started_at_ms: u128,
    // Traffic observers live for the link's lifetime; dropping them
    // undeclares without affecting the firmware's own subscriptions.
    _subscribers: Vec<zenoh::pubsub::Subscriber<()>>,
    session: zenoh::Session,
    binary: String,
    log_path: String,
    frames_out: Arc<AtomicU64>,
    frames_in: Arc<AtomicU64>,
}

/// Supervises the autopilot process plus its Zenoh link as one unit.
pub(crate) struct AutopilotLink {
    inner: Mutex<Option<LinkChild>>,
    last_status: Mutex<Option<AutopilotRunStatus>>,
}

impl AutopilotLink {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            last_status: Mutex::new(None),
        }
    }

    pub(crate) fn status(&self) -> AutopilotRunStatus {
        let mut guard = self.inner.lock().expect("autopilot link lock poisoned");
        match guard.as_mut() {
            Some(link) => match link.child.try_wait() {
                Ok(Some(status)) => {
                    let stopped = AutopilotRunStatus {
                        running: false,
                        pid: None,
                        started_at_ms: None,
                        message: format!("autopilot exited with {status}"),
                        binary: link.binary.clone(),
                        log_path: link.log_path.clone(),
                        frames_out: link.frames_out.load(Ordering::Relaxed),
                        frames_in: link.frames_in.load(Ordering::Relaxed),
                        last_exit_at_ms: Some(now_ms()),
                        last_exit_code: status.code(),
                        last_exit_signal: exit_signal(&status),
                        last_pid: Some(link.child.id()),
                        stop_requested: false,
                    };
                    append_lifecycle(&link.log_path, "unexpected_exit", &stopped);
                    shutdown(guard.take());
                    *self.last_status.lock().expect("last status lock poisoned") =
                        Some(stopped.clone());
                    stopped
                }
                _ => AutopilotRunStatus {
                    running: true,
                    pid: Some(link.child.id()),
                    started_at_ms: Some(link.started_at_ms),
                    message: "autopilot running".to_string(),
                    binary: link.binary.clone(),
                    log_path: link.log_path.clone(),
                    frames_out: link.frames_out.load(Ordering::Relaxed),
                    frames_in: link.frames_in.load(Ordering::Relaxed),
                    last_exit_at_ms: None,
                    last_exit_code: None,
                    last_exit_signal: None,
                    last_pid: None,
                    stop_requested: false,
                },
            },
            None => self
                .last_status
                .lock()
                .expect("last status lock poisoned")
                .clone()
                .unwrap_or_else(|| AutopilotRunStatus {
                    running: false,
                    pid: None,
                    started_at_ms: None,
                    message: "autopilot stopped".to_string(),
                    binary: String::new(),
                    log_path: String::new(),
                    frames_out: 0,
                    frames_in: 0,
                    last_exit_at_ms: None,
                    last_exit_code: None,
                    last_exit_signal: None,
                    last_pid: None,
                    stop_requested: false,
                }),
        }
    }

    pub(crate) fn start(
        &self,
        profile: &AutopilotProfile,
        hub_session: Option<zenoh::Session>,
    ) -> anyhow::Result<AutopilotRunStatus> {
        self.stop_with_reason("restart", "autopilot stopped for restart");
        *self.last_status.lock().expect("last status lock poisoned") = None;

        let binary = validate_native_binary(profile)?;
        let (log_path, log, log_err) = create_log_files(profile)?;
        // Use a dedicated client of the private vehicle router. Reusing the
        // command authority's publishing session suppresses local loopback,
        // leaving frames_in at zero even while the relay is active.
        let session = match hub_session {
            Some(session) => session,
            None => open_autopilot_session(profile)?,
        };
        let frames_out = Arc::new(AtomicU64::new(0));
        let frames_in = Arc::new(AtomicU64::new(0));
        let subscribers =
            declare_traffic_observers(profile, &session, frames_in.clone(), frames_out.clone())?;

        // The firmware last: everything it may immediately talk to is ready.
        // Do not put this in a separate process group. Earlier versions tried
        // to kill the whole group during Stop; on this workstation that has
        // proven unsafe enough to crash the host.
        let child = spawn_native_binary(&binary, log, log_err)?;

        let started_at_ms = now_ms();
        let pid = child.id();
        *self.inner.lock().expect("autopilot link lock poisoned") = Some(LinkChild {
            child,
            started_at_ms,
            _subscribers: subscribers,
            session,
            binary: binary.clone(),
            log_path: log_path.clone(),
            frames_out,
            frames_in,
        });
        append_lifecycle(
            &log_path,
            "start",
            &AutopilotRunStatus {
                running: true,
                pid: Some(pid),
                started_at_ms: Some(started_at_ms),
                message: "autopilot started".to_string(),
                binary: binary.clone(),
                log_path: log_path.clone(),
                frames_out: 0,
                frames_in: 0,
                last_exit_at_ms: None,
                last_exit_code: None,
                last_exit_signal: None,
                last_pid: None,
                stop_requested: false,
            },
        );
        Ok(self.status())
    }

    pub(crate) fn stop(&self) -> AutopilotRunStatus {
        self.stop_with_reason("requested_stop", "autopilot stopped by operator request")
    }

    pub(crate) fn stop_for_shutdown(&self) -> AutopilotRunStatus {
        self.stop_with_reason(
            "ground_station_shutdown",
            "autopilot stopped with ground station",
        )
    }

    fn stop_with_reason(&self, event: &str, message: &str) -> AutopilotRunStatus {
        let link = self
            .inner
            .lock()
            .expect("autopilot link lock poisoned")
            .take();
        if let Some(link_ref) = link.as_ref() {
            let stopped = AutopilotRunStatus {
                running: false,
                pid: None,
                started_at_ms: None,
                message: message.to_string(),
                binary: link_ref.binary.clone(),
                log_path: link_ref.log_path.clone(),
                frames_out: link_ref.frames_out.load(Ordering::Relaxed),
                frames_in: link_ref.frames_in.load(Ordering::Relaxed),
                last_exit_at_ms: Some(now_ms()),
                last_exit_code: None,
                last_exit_signal: None,
                last_pid: Some(link_ref.child.id()),
                stop_requested: true,
            };
            append_lifecycle(&link_ref.log_path, event, &stopped);
            *self.last_status.lock().expect("last status lock poisoned") = Some(stopped);
        }
        shutdown(link);
        self.status()
    }
}

impl Drop for AutopilotLink {
    fn drop(&mut self) {
        self.stop_for_shutdown();
    }
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

fn append_lifecycle(log_path: &str, event: &str, status: &AutopilotRunStatus) {
    let path = Path::new(log_path).with_file_name("autopilot-lifecycle.jsonl");
    let record = serde_json::json!({
        "timestampMs": now_ms(),
        "event": event,
        "status": status,
    });
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{record}"));
    if let Err(error) = result {
        tracing::warn!(path = %path.display(), %error, "failed to append autopilot lifecycle event");
    }
}

struct MonitoredTopic {
    key: String,
    /// Catalog topic whose value contract samples must satisfy. None is used
    /// only for legacy custom mocap keys.
    contract: Option<&'static synapse_fbs::topic_catalog::TopicInfo>,
}

fn validate_native_binary(profile: &AutopilotProfile) -> anyhow::Result<String> {
    let binary = profile.native_binary.trim().to_string();
    if binary.is_empty() {
        anyhow::bail!("autopilot native binary is not configured");
    }
    if !Path::new(&binary).exists() {
        anyhow::bail!(
            "autopilot binary not found: {binary} — build it with \
             `west build -b native_sim -d build-native_sim cerebri_cubs2`"
        );
    }
    Ok(binary)
}

fn create_log_files(
    profile: &AutopilotProfile,
) -> anyhow::Result<(String, std::fs::File, std::fs::File)> {
    let log_path = profile.native_log_path();
    if let Some(parent) = Path::new(&log_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let log = std::fs::File::create(&log_path)?;
    let log_err = log.try_clone()?;
    Ok((log_path, log, log_err))
}

fn open_autopilot_session(profile: &AutopilotProfile) -> anyhow::Result<zenoh::Session> {
    let mut config = zenoh::Config::default();
    // Client of the daemon's own zenoh listener. A connect-only peer over UDP
    // never joins the mesh, so its subscribers receive nothing (field debug
    // 2026-07-08: framesIn stayed 0 while the pose stream ran at 240 Hz).
    config
        .insert_json5("mode", "\"client\"")
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;

    let endpoints = autopilot_zenoh_endpoints(profile);
    if !endpoints.is_empty() {
        let endpoints_json = endpoints
            .iter()
            .map(|endpoint| format!("\"{endpoint}\""))
            .collect::<Vec<_>>()
            .join(",");
        config
            .insert_json5("connect/endpoints", &format!("[{endpoints_json}]"))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }

    zenoh::open(config)
        .wait()
        .map_err(|error| anyhow::anyhow!("zenoh open failed: {error}"))
}

fn declare_traffic_observers(
    profile: &AutopilotProfile,
    session: &zenoh::Session,
    frames_in: Arc<AtomicU64>,
    frames_out: Arc<AtomicU64>,
) -> anyhow::Result<Vec<zenoh::pubsub::Subscriber<()>>> {
    let mut subscribers = Vec::new();
    for spec in profile.inbound_topics() {
        let Some(topic) = resolve_monitored_topic(&spec) else {
            tracing::warn!(spec, "unknown autopilot input topic; not counting");
            continue;
        };
        subscribers.push(declare_counter_subscriber(
            session,
            frames_in.clone(),
            topic,
        )?);
    }
    for key in AUTOPILOT_OUTPUT_TOPICS {
        let topic = synapse_fbs::topic_catalog::parse_key(key)
            .map(|parsed| MonitoredTopic {
                key: (*key).to_string(),
                contract: Some(parsed.topic),
            })
            .ok_or_else(|| anyhow::anyhow!("unknown autopilot output topic {key}"))?;
        subscribers.push(declare_counter_subscriber(
            session,
            frames_out.clone(),
            topic,
        )?);
    }
    Ok(subscribers)
}

fn declare_counter_subscriber(
    session: &zenoh::Session,
    count: Arc<AtomicU64>,
    topic: MonitoredTopic,
) -> anyhow::Result<zenoh::pubsub::Subscriber<()>> {
    let key = topic.key;
    let contract = topic.contract;
    let callback_key = key.clone();
    session
        .declare_subscriber(key.clone())
        .callback(move |sample| {
            if let Some(topic) = contract {
                let encoding = sample.encoding().to_string();
                let valid = synapse_fbs::value_contract::topic_for_encoding(&encoding)
                    .is_ok_and(|resolved| resolved.id == topic.id);
                if !valid {
                    tracing::debug!(key = %callback_key, encoding, "sample not counted: value contract mismatch");
                    return;
                }
            }
            count.fetch_add(1, Ordering::Relaxed);
        })
        .wait()
        .map_err(|error| anyhow::anyhow!("zenoh traffic observer {key} failed: {error}"))
}

fn spawn_native_binary(
    binary: &str,
    log: std::fs::File,
    log_err: std::fs::File,
) -> anyhow::Result<Child> {
    let mut command = Command::new(binary);
    command
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    // Do not use Linux PR_SET_PDEATHSIG here. `Command::spawn` may run on a
    // short-lived Tokio worker, and Linux defines the parent as that thread;
    // the kernel would then kill a healthy autopilot when the worker exits.
    // AutopilotLink owns the Child and performs explicit stop/shutdown cleanup.
    Ok(command.spawn()?)
}

fn autopilot_zenoh_endpoints(profile: &AutopilotProfile) -> Vec<String> {
    let mut endpoints = Vec::new();
    push_unique_endpoint(&mut endpoints, profile.runtime_endpoint.trim());
    endpoints
}

fn push_unique_endpoint(endpoints: &mut Vec<String>, endpoint: &str) {
    if !endpoint.is_empty() && !endpoints.iter().any(|existing| existing == endpoint) {
        endpoints.push(endpoint.to_string());
    }
}

fn resolve_monitored_topic(spec: &str) -> Option<MonitoredTopic> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }

    // Any public mocap stream maps to the MocapFrame CSYN id; cubs2 decodes
    // both wire forms (compact 28-byte pose and MocapFrame FlatBuffer).
    if spec == MOCAP_POSE_TOPIC
        || spec.contains("/mocap/rigid_body/")
        || spec.contains("/mocap/selected/rigid_body/")
        || spec.ends_with("mocap/frame")
        || spec.ends_with("mocap_frame")
    {
        synapse_fbs::topic_catalog::topic_by_name("MocapFrame")?;
        return Some(MonitoredTopic {
            key: spec.to_string(),
            contract: None,
        });
    }

    // Compact catalog key, bare or namespaced. Subscribe to the spec as
    // written so any namespace is preserved.
    let parsed = synapse_fbs::topic_catalog::parse_key(spec)?;
    Some(MonitoredTopic {
        key: spec.to_string(),
        contract: Some(parsed.topic),
    })
}

fn shutdown(link: Option<LinkChild>) {
    let Some(link) = link else {
        return;
    };

    let LinkChild {
        mut child,
        _subscribers,
        session,
        binary,
        ..
    } = link;

    drop(_subscribers);
    drop(session);

    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) => {
            if let Err(err) = child.kill() {
                tracing::warn!(binary, error = %err, "failed to kill autopilot child");
            }
            wait_child_bounded(&mut child, Duration::from_secs(2), &binary);
        }
        Err(err) => {
            tracing::warn!(binary, error = %err, "failed to query autopilot child status");
        }
    }
}

fn wait_child_bounded(child: &mut Child, timeout: Duration, binary: &str) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                tracing::warn!(binary, "autopilot child did not exit before stop timeout");
                return;
            }
            Err(err) => {
                tracing::warn!(binary, error = %err, "failed while waiting for autopilot child");
                return;
            }
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_raw_pose_resolves_to_raw_pose_contract() {
        let inbound = resolve_monitored_topic("qualisys/cub1/pose_raw").unwrap();
        let expected = synapse_fbs::topic_catalog::topic_by_name("RawPose").unwrap();

        assert_eq!(inbound.contract.map(|topic| topic.id), Some(expected.id));
    }

    #[test]
    fn output_topics_include_mission_segments() {
        assert!(AUTOPILOT_OUTPUT_TOPICS.contains(&"traj"));
        assert!(synapse_fbs::topic_catalog::parse_key("traj").is_some());
    }

    #[test]
    #[ignore = "requires loopback network listeners"]
    fn direct_zenoh_observers_count_inputs_and_outputs() {
        let endpoint = "udp/127.0.0.1:17449";
        let mut router_config = zenoh::Config::default();
        router_config.insert_json5("mode", "\"router\"").unwrap();
        router_config
            .insert_json5("listen/endpoints", &format!("[\"{endpoint}\"]"))
            .unwrap();
        let router = zenoh::open(router_config).wait().unwrap();

        let open_client = || {
            let mut config = zenoh::Config::default();
            config.insert_json5("mode", "\"client\"").unwrap();
            config
                .insert_json5("connect/endpoints", &format!("[\"{endpoint}\"]"))
                .unwrap();
            zenoh::open(config).wait().unwrap()
        };
        let observer = open_client();
        let publisher = open_client();
        let frames_in = Arc::new(AtomicU64::new(0));
        let frames_out = Arc::new(AtomicU64::new(0));
        let profile = AutopilotProfile {
            inbound_topics: vec!["qualisys/cub1/pose_raw".to_string()],
            ..AutopilotProfile::default()
        };
        let _subscriptions =
            declare_traffic_observers(&profile, &observer, frames_in.clone(), frames_out.clone())
                .unwrap();
        std::thread::sleep(Duration::from_millis(100));

        for (key, topic_name, payload_len) in [
            ("qualisys/cub1/pose_raw", "RawPose", 40),
            ("pwm", "PwmSignalOutputs", 48),
        ] {
            let topic = synapse_fbs::topic_catalog::topic_by_name(topic_name).unwrap();
            publisher
                .put(key, vec![0_u8; payload_len])
                .encoding(zenoh::bytes::Encoding::from(
                    synapse_fbs::value_contract::encoding_for_topic(topic),
                ))
                .wait()
                .unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while (frames_in.load(Ordering::Relaxed) == 0 || frames_out.load(Ordering::Relaxed) == 0)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(frames_in.load(Ordering::Relaxed), 1);
        assert_eq!(frames_out.load(Ordering::Relaxed), 1);

        let _ = publisher.close().wait();
        let _ = observer.close().wait();
        let _ = router.close().wait();
    }

    #[test]
    fn pose_streams_resolve_to_the_mocap_frame_id() {
        let pose = resolve_monitored_topic("synapse/mocap/rigid_body/cub1/pose").unwrap();
        let selected =
            resolve_monitored_topic("synapse/mocap/selected/rigid_body/cub1/pose").unwrap();
        assert_eq!(
            pose.contract.map(|topic| topic.id),
            selected.contract.map(|topic| topic.id)
        );
        assert_eq!(pose.key, "synapse/mocap/rigid_body/cub1/pose");
    }
}

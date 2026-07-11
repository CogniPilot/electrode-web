//! Runs the native cubs2 autopilot and bridges it onto Zenoh.
//!
//! The cubs2 `native_sim` build talks csyn's UDP transport: framed topic
//! packets ("CSYN" magic + LE u16 synapse catalog id + LE u16 payload length)
//! on localhost — the firmware listens on `udp_rx_port` and sends on
//! `udp_tx_port`. Payloads are the canonical synapse_fbs wire encodings, so
//! this link is a pure re-framer:
//!
//!  - autopilot → UDP → strip header → Zenoh put on the catalog key
//!  - Zenoh subscribe (inbound whitelist) → add header → UDP → autopilot
//!
//! The whitelist forwards only the topics the autopilot consumes (mocap pose
//! from the sim or a real mocap system, manual control from the RC bridge) so
//! the autopilot's own publications never loop back at it.

use std::io::ErrorKind;
use std::io::Write;
use std::net::UdpSocket;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zenoh::Wait;

use crate::autopilot::{AutopilotProfile, MOCAP_POSE_TOPIC};

const CSYN_MAGIC: [u8; 4] = *b"CSYN";
const CSYN_HEADER: usize = 8;
const MAX_FRAME: usize = 2048;

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
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    // Subscribers live for the link's lifetime; dropping them undeclares.
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
        let rx = UdpSocket::bind(("127.0.0.1", profile.udp_tx_port))?;
        rx.set_read_timeout(Some(Duration::from_millis(200)))?;
        let tx = UdpSocket::new_target(profile.udp_rx_port)?;

        let stop = Arc::new(AtomicBool::new(false));
        let frames_out = Arc::new(AtomicU64::new(0));
        let frames_in = Arc::new(AtomicU64::new(0));

        let udp_to_zenoh =
            spawn_udp_to_zenoh(rx, session.clone(), stop.clone(), frames_out.clone());
        let subscribers = declare_inbound_subscribers(profile, &session, &tx, frames_in.clone())?;

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
            stop,
            threads: vec![udp_to_zenoh],
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

struct InboundTopic {
    key: String,
    id: u16,
    /// Catalog topic whose value contract inbound samples must satisfy.
    /// None for the custom mocap keys, whose wire forms are not catalog
    /// encodings (compact 28-byte pose and bridge MocapFrame).
    contract: Option<&'static synapse_fbs::topic_catalog::TopicInfo>,
}

enum UdpRead {
    Frame(usize),
    Timeout,
    Closed,
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

fn spawn_udp_to_zenoh(
    rx: UdpSocket,
    session: zenoh::Session,
    stop: Arc<AtomicBool>,
    count: Arc<AtomicU64>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buf = [0_u8; MAX_FRAME];
        while !stop.load(Ordering::Relaxed) {
            match read_udp(&rx, &mut buf) {
                UdpRead::Frame(len) => forward_udp_frame(&session, &count, &buf[..len]),
                UdpRead::Timeout => {}
                UdpRead::Closed => break,
            }
        }
    })
}

fn read_udp(rx: &UdpSocket, buf: &mut [u8]) -> UdpRead {
    match rx.recv(buf) {
        Ok(len) => UdpRead::Frame(len),
        Err(err) if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut => {
            UdpRead::Timeout
        }
        Err(_) => UdpRead::Closed,
    }
}

fn forward_udp_frame(session: &zenoh::Session, count: &AtomicU64, bytes: &[u8]) {
    let Some((id, payload)) = parse_frame(bytes) else {
        return;
    };
    let Some(topic) = synapse_fbs::topic_catalog::topic_by_id(id) else {
        return;
    };
    let encoding = synapse_fbs::value_contract::encoding_for_topic(topic);
    if session
        .put(topic.key, payload.to_vec())
        .encoding(zenoh::bytes::Encoding::from(encoding))
        .wait()
        .is_ok()
    {
        count.fetch_add(1, Ordering::Relaxed);
    }
}

fn declare_inbound_subscribers(
    profile: &AutopilotProfile,
    session: &zenoh::Session,
    tx: &UdpSocket,
    count: Arc<AtomicU64>,
) -> anyhow::Result<Vec<zenoh::pubsub::Subscriber<()>>> {
    let mut subscribers = Vec::new();
    for spec in profile.inbound_topics() {
        let Some(topic) = resolve_inbound_topic(&spec) else {
            tracing::warn!(spec, "unknown inbound topic; skipping");
            continue;
        };
        subscribers.push(declare_inbound_subscriber(
            session,
            tx,
            count.clone(),
            topic,
        )?);
    }
    Ok(subscribers)
}

#[allow(clippy::excessive_nesting)]
fn declare_inbound_subscriber(
    session: &zenoh::Session,
    tx: &UdpSocket,
    count: Arc<AtomicU64>,
    topic: InboundTopic,
) -> anyhow::Result<zenoh::pubsub::Subscriber<()>> {
    let tx = tx.try_clone()?;
    let id = topic.id;
    let key = topic.key;
    let contract = topic.contract;
    let callback_key = key.clone();
    let logged = Arc::new(AtomicBool::new(false));
    let rejected = Arc::new(AtomicBool::new(false));
    // No zenoh republish here: zenoh-transport firmware subscribes to the
    // pose keys directly (CSYN_ZENOH_MOCAP_POSE_KEY) with its own
    // selected-vs-fallback arbitration; republishing onto the catalog key
    // would bypass that arbitration and double the stream on the bus.
    session
        .declare_subscriber(key.clone())
        .callback(move |sample| {
            if let Some(topic) = contract {
                let encoding = sample.encoding().to_string();
                let valid = synapse_fbs::value_contract::topic_for_encoding(&encoding)
                    .is_ok_and(|resolved| resolved.id == topic.id);
                if !valid {
                    if !rejected.swap(true, Ordering::Relaxed) {
                        tracing::warn!(
                            key = %callback_key,
                            encoding,
                            "inbound sample rejected: value contract mismatch"
                        );
                    }
                    return;
                }
            }
            let payload = sample.payload().to_bytes();
            log_inbound_once(&logged, &callback_key, id, payload.len());
            send_inbound_frame(&tx, &count, id, &payload);
        })
        .wait()
        .map_err(|error| anyhow::anyhow!("zenoh subscribe {key} failed: {error}"))
}

fn log_inbound_once(logged: &AtomicBool, key: &str, id: u16, bytes: usize) {
    if logged.swap(true, Ordering::Relaxed) {
        return;
    }
    tracing::info!(key, id, bytes, "autopilot inbound sample");
}

fn send_inbound_frame(tx: &UdpSocket, count: &AtomicU64, id: u16, payload: &[u8]) {
    let frame = build_frame(id, payload);
    if tx.send(&frame).is_ok() {
        count.fetch_add(1, Ordering::Relaxed);
    }
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
    #[cfg(target_os = "linux")]
    // SAFETY: `pre_exec` runs after fork and before exec. `prctl`, `getppid`,
    // and `raise` are async-signal-safe syscalls and do not touch Rust-owned
    // state. The parent check closes the race where the GCS exits between
    // spawning the child and installing the parent-death signal.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() == 1 {
                libc::raise(libc::SIGKILL);
            }
            Ok(())
        });
    }
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

fn resolve_inbound_topic(spec: &str) -> Option<InboundTopic> {
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
        let topic = synapse_fbs::topic_catalog::topic_by_name("MocapFrame")?;
        return Some(InboundTopic {
            key: spec.to_string(),
            id: topic.id,
            contract: None,
        });
    }

    // Compact catalog key, bare or namespaced. Subscribe to the spec as
    // written so any namespace is preserved.
    let parsed = synapse_fbs::topic_catalog::parse_key(spec)?;
    Some(InboundTopic {
        key: spec.to_string(),
        id: parsed.topic.id,
        contract: Some(parsed.topic),
    })
}

fn shutdown(link: Option<LinkChild>) {
    let Some(link) = link else {
        return;
    };

    let LinkChild {
        mut child,
        stop,
        threads,
        _subscribers,
        session,
        binary,
        ..
    } = link;

    stop.store(true, Ordering::Relaxed);
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

    // Do not join these threads in the HTTP stop path. They poll with short
    // timeouts and will observe `stop`; blocking here risks freezing the GCS.
    for thread in threads {
        drop(thread);
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

/// Parse a csyn UDP frame; returns (catalog id, payload) when valid.
fn parse_frame(buf: &[u8]) -> Option<(u16, &[u8])> {
    if buf.len() < CSYN_HEADER || buf[..4] != CSYN_MAGIC {
        return None;
    }
    let id = u16::from_le_bytes([buf[4], buf[5]]);
    let len = u16::from_le_bytes([buf[6], buf[7]]) as usize;
    if CSYN_HEADER + len != buf.len() {
        return None;
    }
    Some((id, &buf[CSYN_HEADER..]))
}

/// Build a csyn UDP frame around a synapse payload.
fn build_frame(id: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(CSYN_HEADER + payload.len());
    frame.extend_from_slice(&CSYN_MAGIC);
    frame.extend_from_slice(&id.to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

/// Small helper: a connected UDP sender to the firmware's RX port.
trait UdpTarget: Sized {
    fn new_target(port: u16) -> std::io::Result<Self>;
}

impl UdpTarget for UdpSocket {
    fn new_target(port: u16) -> std::io::Result<Self> {
        let socket = UdpSocket::bind(("127.0.0.1", 0))?;
        socket.connect(("127.0.0.1", port))?;
        Ok(socket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let payload = [1_u8, 2, 3, 4];
        let frame = build_frame(27, &payload);
        let (id, body) = parse_frame(&frame).expect("valid frame");
        assert_eq!(id, 27);
        assert_eq!(body, payload);
    }

    #[test]
    fn bridge_raw_pose_resolves_to_raw_pose_contract() {
        let inbound = resolve_inbound_topic("qualisys/cub1/pose_raw").unwrap();
        let expected = synapse_fbs::topic_catalog::topic_by_name("RawPose").unwrap();

        assert_eq!(inbound.id, expected.id);
        assert_eq!(inbound.contract.map(|topic| topic.id), Some(expected.id));
    }

    #[test]
    #[ignore = "requires loopback network listeners"]
    fn raw_mocap_is_forwarded_from_zenoh_to_firmware_udp() {
        let endpoint = "udp/127.0.0.1:17448";
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
        let publisher = open_client();
        let subscriber = open_client();
        let udp_rx = UdpSocket::bind(("127.0.0.1", 18450)).unwrap();
        udp_rx
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let udp_tx = UdpSocket::new_target(18450).unwrap();
        let count = Arc::new(AtomicU64::new(0));
        let inbound = resolve_inbound_topic("qualisys/cub1/pose_raw").unwrap();
        let topic_id = inbound.id;
        let contract = inbound.contract.unwrap();
        let _subscription =
            declare_inbound_subscriber(&subscriber, &udp_tx, count.clone(), inbound).unwrap();

        std::thread::sleep(Duration::from_millis(100));
        publisher
            .put("qualisys/cub1/pose_raw", vec![0_u8; 40])
            .encoding(zenoh::bytes::Encoding::from(
                synapse_fbs::value_contract::encoding_for_topic(contract),
            ))
            .wait()
            .unwrap();

        let mut frame = [0_u8; 128];
        let len = udp_rx.recv(&mut frame).unwrap();
        let (id, payload) = parse_frame(&frame[..len]).unwrap();
        assert_eq!(id, topic_id);
        assert_eq!(payload.len(), 40);
        assert_eq!(count.load(Ordering::Relaxed), 1);

        let _ = publisher.close().wait();
        let _ = subscriber.close().wait();
        let _ = router.close().wait();
    }

    #[test]
    fn rejects_bad_magic_and_length() {
        assert!(parse_frame(b"NOPE\x01\x00\x00\x00").is_none());
        let mut frame = build_frame(1, &[9, 9]);
        frame.push(0); // trailing garbage breaks the declared length
        assert!(parse_frame(&frame).is_none());
    }

    #[test]
    fn pose_streams_resolve_to_the_mocap_frame_id() {
        let pose = resolve_inbound_topic("synapse/mocap/rigid_body/cub1/pose").unwrap();
        let selected =
            resolve_inbound_topic("synapse/mocap/selected/rigid_body/cub1/pose").unwrap();
        assert_eq!(pose.id, selected.id);
        assert_eq!(pose.key, "synapse/mocap/rigid_body/cub1/pose");
    }
}

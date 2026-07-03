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
use std::net::UdpSocket;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zenoh::Wait;

use crate::autopilot::AutopilotProfile;

const CSYN_MAGIC: [u8; 4] = *b"CSYN";
const CSYN_HEADER: usize = 8;
const MAX_FRAME: usize = 2048;
const CUB1_MOCAP_TOPIC: &str = "synapse/mocap/rigid_body/cub1/pose";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutopilotRunStatus {
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
pub struct AutopilotLink {
    inner: Mutex<Option<LinkChild>>,
}

impl AutopilotLink {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    pub fn status(&self) -> AutopilotRunStatus {
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
                    };
                    shutdown(guard.take());
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
                },
            },
            None => AutopilotRunStatus {
                running: false,
                pid: None,
                started_at_ms: None,
                message: "autopilot stopped".to_string(),
                binary: String::new(),
                log_path: String::new(),
                frames_out: 0,
                frames_in: 0,
            },
        }
    }

    pub fn start(&self, profile: &AutopilotProfile) -> anyhow::Result<AutopilotRunStatus> {
        self.stop();

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

        // Capture firmware console output for diagnosis.
        let log_path = profile.native_log_path();
        if let Some(parent) = Path::new(&log_path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        let log = std::fs::File::create(&log_path)?;
        let log_err = log.try_clone()?;

        // Zenoh side of the link: peer on the same network as the viewer,
        // the sim, and the RC bridge.
        let mut zconfig = zenoh::Config::default();
        zconfig
            .insert_json5("mode", "\"peer\"")
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let endpoint = profile.runtime_endpoint.trim();
        if !endpoint.is_empty() {
            zconfig
                .insert_json5("connect/endpoints", &format!("[\"{endpoint}\"]"))
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        }
        let session = zenoh::open(zconfig)
            .wait()
            .map_err(|e| anyhow::anyhow!("zenoh open failed: {e}"))?;

        // UDP sockets. The firmware sends to udp_tx_port and listens on
        // udp_rx_port (csyn CONFIG_CSYN_NATIVE_UDP_{TX,RX}_PORT).
        let rx = UdpSocket::bind(("127.0.0.1", profile.udp_tx_port))?;
        rx.set_read_timeout(Some(Duration::from_millis(200)))?;
        let tx = UdpSocket::new_target(profile.udp_rx_port)?;

        let stop = Arc::new(AtomicBool::new(false));
        let frames_out = Arc::new(AtomicU64::new(0));
        let frames_in = Arc::new(AtomicU64::new(0));

        // autopilot → Zenoh.
        let out_session = session.clone();
        let out_stop = stop.clone();
        let out_count = frames_out.clone();
        let udp_to_zenoh = std::thread::spawn(move || {
            let mut buf = [0_u8; MAX_FRAME];
            while !out_stop.load(Ordering::Relaxed) {
                let len = match rx.recv(&mut buf) {
                    Ok(len) => len,
                    Err(err)
                        if err.kind() == ErrorKind::WouldBlock
                            || err.kind() == ErrorKind::TimedOut =>
                    {
                        continue;
                    }
                    Err(_) => break,
                };
                let Some((id, payload)) = parse_frame(&buf[..len]) else {
                    continue;
                };
                let Some(topic) = synapse_fbs::topic_catalog::TOPICS
                    .iter()
                    .find(|topic| topic.id == id)
                else {
                    continue;
                };
                if out_session.put(topic.key, payload.to_vec()).wait().is_ok() {
                    out_count.fetch_add(1, Ordering::Relaxed);
                }
            }
        });

        // Zenoh → autopilot, one subscriber per whitelisted inbound topic.
        let mut subscribers = Vec::new();
        for spec in profile.inbound_topics() {
            let Some(topic) = resolve_inbound_topic(&spec) else {
                tracing::warn!(spec, "unknown inbound topic; skipping");
                continue;
            };
            let tx = tx.try_clone()?;
            let count = frames_in.clone();
            let id = topic.id;
            let key = topic.key.clone();
            let subscriber = session
                .declare_subscriber(key.clone())
                .callback(move |sample| {
                    let payload = sample.payload().to_bytes();
                    let frame = build_frame(id, &payload);
                    if tx.send(&frame).is_ok() {
                        count.fetch_add(1, Ordering::Relaxed);
                    }
                })
                .wait()
                .map_err(|e| anyhow::anyhow!("zenoh subscribe {key} failed: {e}"))?;
            subscribers.push(subscriber);
        }

        // The firmware last: everything it may immediately talk to is ready.
        // Do not put this in a separate process group. Earlier versions tried
        // to kill the whole group during Stop; on this workstation that has
        // proven unsafe enough to crash the host.
        let mut command = Command::new(&binary);
        command
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        let child = command.spawn()?;

        *self.inner.lock().expect("autopilot link lock poisoned") = Some(LinkChild {
            child,
            started_at_ms: now_ms(),
            stop,
            threads: vec![udp_to_zenoh],
            _subscribers: subscribers,
            session,
            binary,
            log_path,
            frames_out,
            frames_in,
        });
        Ok(self.status())
    }

    pub fn stop(&self) -> AutopilotRunStatus {
        let link = self
            .inner
            .lock()
            .expect("autopilot link lock poisoned")
            .take();
        shutdown(link);
        self.status()
    }
}

struct InboundTopic {
    key: String,
    id: u16,
}

fn resolve_inbound_topic(spec: &str) -> Option<InboundTopic> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }

    if spec == CUB1_MOCAP_TOPIC || spec.contains("/mocap/rigid_body/") {
        let topic = synapse_fbs::topic_catalog::TOPICS
            .iter()
            .find(|topic| topic.key_suffix == "mocap_frame")?;
        return Some(InboundTopic {
            key: spec.to_string(),
            id: topic.id,
        });
    }

    if let Some(topic) = synapse_fbs::topic_catalog::TOPICS
        .iter()
        .find(|topic| topic.key_suffix == spec || topic.key == spec)
    {
        return Some(InboundTopic {
            key: topic.key.to_string(),
            id: topic.id,
        });
    }

    None
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
    fn rejects_bad_magic_and_length() {
        assert!(parse_frame(b"NOPE\x01\x00\x00\x00").is_none());
        let mut frame = build_frame(1, &[9, 9]);
        frame.push(0); // trailing garbage breaks the declared length
        assert!(parse_frame(&frame).is_none());
    }
}

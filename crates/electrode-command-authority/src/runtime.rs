//! Zenoh runtime with a hard browser/vehicle session boundary.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use synapse_fbs::cmd::{ParamGetReply, ParamSetReply, TrajectorySetReply};
use synapse_fbs::topic::{MocapFrame, RawPoseData};
use synapse_fbs::types::CommandResultCode;
use zenoh::{Session, Wait};

use crate::firmware_gate::{publish_policy_rejection, FirmwareGate};
use crate::policy::{AuthorizedCommand, CommandPolicy, Delivery, PolicyConfig};

const PARAMETER_QUEUE_CAPACITY: usize = 32;
const PRIVATE_MOCAP_TOPIC: &str = "electrode/sim/rumoca/mocap_frame";
const PUBLIC_MOCAP_TOPIC: &str = "synapse/mocap/frame";
const RIGID_BODY_NAMES_TOPIC: &str = "synapse/mocap/rigid_body_names";
const RAW_POSE_TOPIC: &str = "qualisys/cub1/pose_raw";
const PRIVATE_PWM_TOPIC: &str = "electrode/sim/rumoca/radio_pwm_signal_outputs";
const SYNAPSE_CATALOG_KEY: &str = "electrode/catalog/synapse";
const SYNAPSE_CATALOG_INTERVAL: Duration = Duration::from_secs(1);

/// Runtime endpoints. The two sessions never discover one another directly.
#[derive(Clone, Debug)]
pub struct CommandAuthorityConfig {
    pub browser_listen: String,
    pub lan_request_listen: String,
    pub telemetry_connect: Option<String>,
    pub vehicle_listen: String,
    pub vehicle_connect: Option<String>,
    pub query_timeout: Duration,
    pub policy: PolicyConfig,
}

impl Default for CommandAuthorityConfig {
    fn default() -> Self {
        Self {
            browser_listen: "ws/127.0.0.1:7447".to_string(),
            lan_request_listen: "ws/0.0.0.0:7448".to_string(),
            telemetry_connect: None,
            vehicle_listen: "udp/127.0.0.1:7447".to_string(),
            vehicle_connect: None,
            query_timeout: Duration::from_secs(2),
            policy: PolicyConfig::default(),
        }
    }
}

impl CommandAuthorityConfig {
    #[must_use]
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            browser_listen: value_or("ELECTRODE_GCS_ZENOH_WS_LISTEN", defaults.browser_listen),
            lan_request_listen: value_or(
                "ELECTRODE_GCS_LAN_REQUEST_LISTEN",
                defaults.lan_request_listen,
            ),
            telemetry_connect: optional("ELECTRODE_GCS_TELEMETRY_ZENOH_CONNECT"),
            vehicle_listen: value_or("ELECTRODE_GCS_ZENOH_LISTEN", defaults.vehicle_listen),
            vehicle_connect: optional("ELECTRODE_GCS_ZENOH_CONNECT"),
            query_timeout: Duration::from_millis(
                env::var("ELECTRODE_GCS_QUERY_TIMEOUT_MS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(2_000)
                    .clamp(100, 60_000),
            ),
            policy: PolicyConfig {
                intent_prefix: value_or(
                    "ELECTRODE_GCS_INTENT_PREFIX",
                    defaults.policy.intent_prefix,
                ),
                vehicle_topic_prefix: value_or(
                    "ELECTRODE_GCS_VEHICLE_TOPIC_PREFIX",
                    defaults.policy.vehicle_topic_prefix,
                ),
                parameter_key: value_or(
                    "ELECTRODE_GCS_PARAMETER_KEY",
                    defaults.policy.parameter_key,
                ),
                firmware_key_prefix: value_or(
                    "ELECTRODE_GCS_FIRMWARE_KEY_PREFIX",
                    defaults.policy.firmware_key_prefix,
                ),
                velocity_min_mps: 1.0,
                velocity_max_mps: 4.0,
                velocity_budget: env::var("ELECTRODE_GCS_VELOCITY_BUDGET")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(defaults.policy.velocity_budget)
                    .clamp(1, 5),
                velocity_budget_json: path_or(
                    "ELECTRODE_GCS_VELOCITY_BUDGET_DB",
                    defaults.policy.velocity_budget_json,
                ),
                velocity_budget_csv: path_or(
                    "ELECTRODE_GCS_VELOCITY_BUDGET_CSV",
                    defaults.policy.velocity_budget_csv,
                ),
                raw_max_bytes: defaults.policy.raw_max_bytes,
            },
        }
    }
}

/// Owns both isolated sessions and all allowlisted relays.
pub struct CommandAuthority {
    browser_router: Session,
    browser_session: Session,
    lan_request_router: Session,
    lan_request_session: Session,
    telemetry_session: Option<Session>,
    vehicle_router: Session,
    vehicle_session: Session,
    subscribers: Vec<zenoh::pubsub::Subscriber<()>>,
    query_sender: Option<SyncSender<QueuedQuery>>,
    query_worker: Option<JoinHandle<()>>,
    listeners: Vec<String>,
    query_timeout: Duration,
    vehicle_endpoint: String,
}

struct QueuedQuery {
    command: AuthorizedCommand,
    response: Session,
}

impl CommandAuthority {
    #[allow(clippy::excessive_nesting, clippy::too_many_lines)]
    pub fn start(config: CommandAuthorityConfig) -> Result<Self> {
        ensure_loopback_vehicle_endpoint(&config.vehicle_listen)?;
        ensure_loopback_browser_endpoint(&config.browser_listen)?;
        if let Some(connect) = config.vehicle_connect.as_deref() {
            ensure_loopback_vehicle_endpoint(connect)?;
        }
        let browser_router = open_session("router", &config.browser_listen, None)?;
        let browser_session = open_session("client", "", Some(&config.browser_listen))?;
        let lan_request_router = open_session("router", &config.lan_request_listen, None)?;
        let lan_request_session = open_session("client", "", Some(&config.lan_request_listen))?;
        let telemetry_session = match config.telemetry_connect.as_deref() {
            Some(endpoint) => Some(open_session("client", "", Some(endpoint))?),
            None => None,
        };
        let vehicle_router = open_session("router", &config.vehicle_listen, None)?;
        let vehicle_session = open_session("client", "", Some(&config.vehicle_listen))?;
        let policy = Arc::new(CommandPolicy::new(config.policy.clone()));
        let firmware_gate = Arc::new(FirmwareGate::from_env(
            config.policy.firmware_key_prefix.clone(),
            config.query_timeout,
        ));
        let (query_sender, query_receiver) = sync_channel::<QueuedQuery>(PARAMETER_QUEUE_CAPACITY);
        let worker_vehicle = vehicle_session.clone();
        let worker_config = config.clone();
        let worker_firmware_gate = firmware_gate;
        let query_worker = std::thread::Builder::new()
            .name("electrode-command-query".to_string())
            .spawn(move || {
                while let Ok(queued) = query_receiver.recv() {
                    let command = queued.command;
                    match command.delivery {
                        Delivery::Query => execute_query(
                            &worker_vehicle,
                            &queued.response,
                            &worker_config,
                            command,
                        ),
                        Delivery::Firmware => worker_firmware_gate.handle_intent(
                            &worker_vehicle,
                            &queued.response,
                            &worker_config.policy.intent_prefix,
                            &command.target,
                            &command.payload,
                        ),
                        Delivery::Publish => {
                            tracing::error!("publish command reached query worker")
                        }
                        Delivery::Budget => tracing::error!("budget command reached query worker"),
                    }
                }
            })
            .map_err(|error| anyhow!("start command query worker: {error}"))?;

        let mut subscribers = vec![
            subscribe_intents(
                &browser_session,
                &vehicle_session,
                policy.clone(),
                query_sender.clone(),
                &config,
                true,
            )?,
            subscribe_intents(
                &lan_request_session,
                &vehicle_session,
                policy,
                query_sender.clone(),
                &config,
                false,
            )?,
            relay_synapse_to_browser(
                &vehicle_session,
                &browser_session,
                &config.policy.vehicle_topic_prefix,
            )?,
            relay_to_browser(&vehicle_session, &browser_session, PRIVATE_PWM_TOPIC)?,
            relay_verified_mocap(&browser_session, &vehicle_session)?,
        ];
        if let Some(telemetry) = telemetry_session.as_ref() {
            subscribers.push(relay_verified_lan_mocap(
                telemetry,
                &vehicle_session,
                &browser_session,
            )?);
            subscribers.push(relay_verified_rigid_body_names(
                telemetry,
                &browser_session,
            )?);
            subscribers.push(relay_verified_raw_pose(
                telemetry,
                &vehicle_session,
                &browser_session,
            )?);
        }
        let listeners = [
            config.vehicle_listen.clone(),
            config.browser_listen.clone(),
            config.lan_request_listen.clone(),
        ]
        .into_iter()
        .filter(|endpoint| !endpoint.trim().is_empty())
        .collect::<Vec<_>>();

        tracing::info!(
            browser = %config.browser_listen,
            lan_requests = %config.lan_request_listen,
            telemetry = ?config.telemetry_connect,
            vehicle = %config.vehicle_listen,
            intents = %config.policy.intent_prefix,
            "isolated command authority listening"
        );
        Ok(Self {
            browser_router,
            browser_session,
            lan_request_router,
            lan_request_session,
            telemetry_session,
            vehicle_router,
            vehicle_session,
            subscribers,
            query_sender: Some(query_sender),
            query_worker: Some(query_worker),
            listeners,
            query_timeout: config.query_timeout,
            vehicle_endpoint: config.vehicle_listen,
        })
    }

    #[must_use]
    pub fn listeners(&self) -> &[String] {
        &self.listeners
    }

    /// Open an isolated vehicle-side client for an in-process ground-station
    /// service. Each service needs its own Zenoh session: publications are not
    /// looped back to subscribers cloned from the publishing session.
    pub fn vehicle_client(&self) -> Result<Session> {
        open_session("client", "", Some(&self.vehicle_endpoint))
    }

    /// Execute a trusted localhost request against the private vehicle router.
    pub fn trusted_query(&self, target: &str, payload: Vec<u8>) -> Result<Vec<u8>> {
        let session = open_session("client", "", Some(&self.vehicle_endpoint))?;
        let mut request = session
            .get(target)
            .payload(payload)
            .timeout(self.query_timeout);
        // Stamp the synapse request contract when the target is a catalog
        // command key (`<ns>/cmd/<name>`).
        let command_name = target.rsplit('/').next().unwrap_or(target);
        if let Some(encoding) = crate::policy::command_request_encoding(command_name) {
            request = request.encoding(zenoh::bytes::Encoding::from(encoding));
        }
        let replies = request.wait().map_err(|error| anyhow!(error.to_string()))?;
        let reply = replies
            .recv_timeout(self.query_timeout)
            .map_err(|error| anyhow!(error.to_string()))?
            .ok_or_else(|| anyhow!("target service returned no reply"))?
            .into_result()
            .map_err(|error| anyhow!(error.to_string()))?;
        let payload = reply.payload().to_bytes().to_vec();
        let _ = session.close().wait();
        Ok(payload)
    }
}

fn ensure_loopback_browser_endpoint(endpoint: &str) -> Result<()> {
    let endpoint = endpoint.trim();
    if endpoint.starts_with("ws/127.0.0.1:") || endpoint.starts_with("ws/localhost:") {
        return Ok(());
    }
    Err(anyhow!(
        "trusted browser Zenoh endpoint must be loopback-only, got {endpoint:?}"
    ))
}

fn ensure_loopback_vehicle_endpoint(endpoint: &str) -> Result<()> {
    let endpoint = endpoint.trim();
    if endpoint.starts_with("udp/127.0.0.1:") || endpoint.starts_with("udp/localhost:") {
        return Ok(());
    }
    Err(anyhow!(
        "vehicle-side Zenoh endpoint must be loopback-only, got {endpoint:?}"
    ))
}

impl Drop for CommandAuthority {
    fn drop(&mut self) {
        self.subscribers.clear();
        self.query_sender.take();
        if let Some(worker) = self.query_worker.take() {
            if worker.join().is_err() {
                tracing::warn!("command query worker panicked during shutdown");
            }
        }
        let _ = self.browser_session.close().wait();
        let _ = self.browser_router.close().wait();
        let _ = self.lan_request_session.close().wait();
        let _ = self.lan_request_router.close().wait();
        if let Some(session) = self.telemetry_session.take() {
            let _ = session.close().wait();
        }
        let _ = self.vehicle_session.close().wait();
        let _ = self.vehicle_router.close().wait();
    }
}

fn subscribe_intents(
    browser: &Session,
    vehicle: &Session,
    policy: Arc<CommandPolicy>,
    query_sender: SyncSender<QueuedQuery>,
    config: &CommandAuthorityConfig,
    trusted: bool,
) -> Result<zenoh::pubsub::Subscriber<()>> {
    let key = format!("{}/**", config.policy.intent_prefix.trim_end_matches('/'));
    let callback_vehicle = vehicle.clone();
    let callback_browser = browser.clone();
    let callback_config = config.clone();
    browser
        .declare_subscriber(key.clone())
        .callback(move |sample| {
            let intent_key = sample.key_expr().as_str();
            let payload = sample.payload().to_bytes();
            tracing::debug!(
                key = intent_key,
                trusted,
                bytes = payload.len(),
                "command intent received"
            );
            let authorization = if trusted {
                policy.authorize_trusted(intent_key, &payload)
            } else {
                policy.authorize(intent_key, &payload)
            };
            match authorization {
                Ok(command) if command.delivery == Delivery::Publish => {
                    execute_publish(
                        &callback_vehicle,
                        &callback_browser,
                        &callback_config,
                        policy.as_ref(),
                        command,
                    );
                }
                Ok(command) if command.delivery == Delivery::Budget => {
                    publish_velocity_command_status(
                        &callback_browser,
                        &callback_config,
                        "state",
                        &command,
                        "authoritative velocity budget",
                    );
                }
                Ok(command) => {
                    enqueue_query(&query_sender, &callback_browser, &callback_config, command)
                }
                Err(error) => {
                    tracing::warn!(key = intent_key, %error, "browser command rejected");
                    publish_policy_error(
                        &callback_browser,
                        &callback_config,
                        policy.as_ref(),
                        intent_key,
                        &payload,
                        &error.to_string(),
                    );
                }
            }
        })
        .wait()
        .map_err(|error| anyhow!("subscribe browser intents {key}: {error}"))
}

fn publish_policy_error(
    browser: &Session,
    config: &CommandAuthorityConfig,
    policy: &CommandPolicy,
    intent_key: &str,
    payload: &[u8],
    message: &str,
) {
    let prefix = format!("{}/", config.policy.intent_prefix.trim_end_matches('/'));
    let suffix = intent_key.strip_prefix(&prefix).unwrap_or("");
    match suffix {
        "velocity" | "velocity_budget" => match policy.velocity_state_for_payload(payload) {
            Ok(state) => publish_velocity_state(browser, config, "rejected", &state, message),
            Err(_) => publish_unknown_velocity_status(
                browser,
                config,
                CommandPolicy::credential_id_for_payload(payload).as_deref(),
                message,
            ),
        },
        "gain" => publish_status(browser, config, "gain", "rejected", message),
        firmware if firmware.starts_with("firmware/") => {
            let update_id = firmware[9..].split('/').next().unwrap_or("");
            if !publish_policy_rejection(browser, &config.policy.intent_prefix, update_id, message)
            {
                publish_status(browser, config, "command", "rejected", message);
            }
        }
        _ => publish_status(browser, config, "command", "rejected", message),
    }
}

fn execute_publish(
    vehicle: &Session,
    browser: &Session,
    config: &CommandAuthorityConfig,
    policy: &CommandPolicy,
    command: AuthorizedCommand,
) {
    let mut put = vehicle.put(&command.target, command.payload.clone());
    if let Some(encoding) = command.encoding.as_deref() {
        put = put.encoding(zenoh::bytes::Encoding::from(encoding));
    }
    let result = put.wait();
    if let Err(error) = result {
        if let (Some(device), Some(credential_id)) = (
            command.velocity_device.as_deref(),
            command.velocity_credential_id.as_deref(),
        ) {
            match policy.refund_velocity(device, credential_id) {
                Ok(state) => {
                    publish_velocity_state(browser, config, "failed", &state, &error.to_string())
                }
                Err(refund) => publish_velocity_command_status(
                    browser,
                    config,
                    "failed",
                    &command,
                    &format!("{error}; budget refund failed: {refund}"),
                ),
            }
        } else {
            publish_status(
                browser,
                config,
                &command.status_leaf,
                "failed",
                &error.to_string(),
            );
        }
        return;
    }
    if command.velocity_remaining.is_some() {
        publish_velocity_command_status(
            browser,
            config,
            "accepted",
            &command,
            "command forwarded to the vehicle session",
        );
    } else {
        publish_status(
            browser,
            config,
            &command.status_leaf,
            "accepted",
            "command forwarded to the vehicle session",
        );
    }
}

fn enqueue_query(
    sender: &SyncSender<QueuedQuery>,
    browser: &Session,
    config: &CommandAuthorityConfig,
    command: AuthorizedCommand,
) {
    let (command, message) = match sender.try_send(QueuedQuery {
        command,
        response: browser.clone(),
    }) {
        Ok(()) => return,
        Err(TrySendError::Full(queued)) => (queued.command, "command query queue is full"),
        Err(TrySendError::Disconnected(queued)) => {
            (queued.command, "command query worker is unavailable")
        }
    };
    publish_query_status(browser, config, &command, "rejected", message);
}

fn execute_query(
    _vehicle: &Session,
    browser: &Session,
    config: &CommandAuthorityConfig,
    command: AuthorizedCommand,
) {
    let query_session = match open_session("client", "", Some(&config.vehicle_listen)) {
        Ok(session) => session,
        Err(error) => {
            publish_query_status(browser, config, &command, "rejected", &error.to_string());
            return;
        }
    };
    let mut request = query_session
        .get(&command.target)
        .payload(command.payload.clone())
        .timeout(config.query_timeout);
    if let Some(encoding) = command.encoding.as_deref() {
        request = request.encoding(zenoh::bytes::Encoding::from(encoding));
    }
    let replies = match request.wait() {
        Ok(replies) => replies,
        Err(error) => {
            publish_query_status(browser, config, &command, "rejected", &error.to_string());
            return;
        }
    };
    match replies.recv_timeout(config.query_timeout) {
        Ok(Some(reply)) => match reply.into_result() {
            Ok(sample) => {
                let payload = sample.payload().to_bytes().to_vec();
                let reply_key = format!(
                    "{}/reply/{}",
                    status_prefix(&config.policy.intent_prefix),
                    command.status_leaf
                );
                let _ = browser.put(reply_key, payload.clone()).wait();
                match command_reply_status(&command.status_leaf, &payload) {
                    Ok((status, message)) => {
                        publish_query_status(browser, config, &command, status, &message)
                    }
                    Err(error) => publish_query_status(
                        browser,
                        config,
                        &command,
                        "rejected",
                        &error.to_string(),
                    ),
                }
            }
            Err(error) => {
                publish_query_status(browser, config, &command, "rejected", &error.to_string())
            }
        },
        Ok(None) => publish_query_status(
            browser,
            config,
            &command,
            "rejected",
            "target service returned no reply",
        ),
        Err(error) => {
            publish_query_status(browser, config, &command, "rejected", &error.to_string())
        }
    }
}

fn command_reply_status(leaf: &str, payload: &[u8]) -> Result<(&'static str, String)> {
    if leaf == "parameters" {
        let reply = flatbuffers::root::<ParamGetReply<'_>>(payload)
            .map_err(|error| anyhow!("invalid ParamGetReply: {error}"))?;
        return match reply.result() {
            CommandResultCode::Accepted => {
                Ok(("accepted", "target service returned the parameter".into()))
            }
            result => Err(anyhow!(
                "target service returned {} (detail {})",
                result.variant_name().unwrap_or("unknown result"),
                reply.result_detail()
            )),
        };
    }
    if leaf == "trajectory" {
        let reply = flatbuffers::root::<TrajectorySetReply<'_>>(payload)
            .map_err(|error| anyhow!("invalid TrajectorySetReply: {error}"))?;
        return match reply.result() {
            CommandResultCode::Accepted => {
                Ok(("accepted", "target service accepted the trajectory".into()))
            }
            CommandResultCode::InProgress => Ok((
                "in_progress",
                "target service is applying the trajectory".into(),
            )),
            result => Err(anyhow!(
                "target service returned {} (detail {})",
                result.variant_name().unwrap_or("unknown result"),
                reply.result_detail()
            )),
        };
    }
    parameter_reply_status(
        payload,
        if leaf == "velocity" {
            "velocity setpoint"
        } else {
            "parameter"
        },
    )
}

fn parameter_reply_status(payload: &[u8], subject: &str) -> Result<(&'static str, String)> {
    let reply = flatbuffers::root::<ParamSetReply<'_>>(payload)
        .map_err(|error| anyhow!("invalid ParamSetReply: {error}"))?;
    let result = reply.result();
    let detail = reply.result_detail();
    match result {
        CommandResultCode::Accepted => {
            Ok(("accepted", format!("target service accepted the {subject}")))
        }
        CommandResultCode::InProgress => Ok((
            "in_progress",
            format!("target service is applying the {subject}"),
        )),
        _ => Err(anyhow!(
            "target service returned {} (detail {detail})",
            result.variant_name().unwrap_or("unknown result")
        )),
    }
}

fn relay_to_browser(
    vehicle: &Session,
    browser: &Session,
    key: &str,
) -> Result<zenoh::pubsub::Subscriber<()>> {
    let destination = browser.clone();
    vehicle
        .declare_subscriber(key)
        .callback(move |sample| {
            let key = sample.key_expr().as_str().to_string();
            let payload = sample.payload().to_bytes().to_vec();
            if let Err(error) = destination
                .put(key.clone(), payload)
                .encoding(sample.encoding().clone())
                .wait()
            {
                tracing::warn!(%key, %error, "vehicle telemetry relay failed");
            }
        })
        .wait()
        .map_err(|error| anyhow!("subscribe vehicle relay {key}: {error}"))
}

/// Relay Synapse payloads into the isolated browser session and publish a
/// throttled, payload-free catalog announcement for every observed key. Zenoh
/// only delivers the relayed payload when a browser subscriber requests that
/// key, while the catalog lets the UI discover topics without subscribing to
/// every high-rate stream.
///
/// Compact 0.6.0 catalog keys are bare (`att`, `pwm`) or namespaced
/// (`cub1/att`), so there is no fixed `synapse/**` marker left to anchor on:
/// the relay discovers everything on the vehicle session and skips the keys
/// other relays own. The sample encoding carries the value contract and is
/// forwarded untouched so browser-side validation still holds.
fn relay_synapse_to_browser(
    vehicle: &Session,
    browser: &Session,
    namespace: &str,
) -> Result<zenoh::pubsub::Subscriber<()>> {
    let destination = browser.clone();
    let announcements = Arc::new(Mutex::new(HashMap::<String, Instant>::new()));
    let namespace = namespace.trim_matches('/');
    let keyexpr = if namespace.is_empty() {
        "**".to_string()
    } else {
        format!("{namespace}/**")
    };
    vehicle
        .declare_subscriber(keyexpr.clone())
        .callback(move |sample| {
            let key = sample.key_expr().as_str().to_string();
            // This input originates on the external session and is already
            // visible there. Relaying it back would form an external↔vehicle
            // loop through the allowlisted mocap bridge below.
            if key == PUBLIC_MOCAP_TOPIC || key == RAW_POSE_TOPIC {
                return;
            }
            // Private electrode keys have their own dedicated relays, and the
            // GCS namespace never crosses the vehicle boundary.
            if key.starts_with("electrode/") || key.starts_with("gcs/") || key.starts_with('@') {
                return;
            }
            let payload = sample.payload().to_bytes().to_vec();
            let now = Instant::now();
            let should_announce = should_announce_topic(&announcements, &key, now);

            if should_announce {
                let announcement = serde_json::json!({
                    "key": key,
                    "lastBytes": payload.len(),
                })
                .to_string();
                if let Err(error) = destination.put(SYNAPSE_CATALOG_KEY, announcement).wait() {
                    tracing::warn!(%error, "Synapse catalog announcement failed");
                }
            }

            if let Err(error) = destination
                .put(key.clone(), payload)
                .encoding(sample.encoding().clone())
                .wait()
            {
                tracing::warn!(%key, %error, "vehicle Synapse relay failed");
            }
        })
        .wait()
        .map_err(|error| anyhow!("subscribe vehicle relay {keyexpr}: {error}"))
}

fn should_announce_topic(
    announcements: &Mutex<HashMap<String, Instant>>,
    key: &str,
    now: Instant,
) -> bool {
    let mut announcements = announcements
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match announcements.get_mut(key) {
        Some(last) if now.duration_since(*last) < SYNAPSE_CATALOG_INTERVAL => false,
        Some(last) => {
            *last = now;
            true
        }
        None => {
            announcements.insert(key.to_string(), now);
            true
        }
    }
}

fn relay_verified_mocap(
    browser: &Session,
    vehicle: &Session,
) -> Result<zenoh::pubsub::Subscriber<()>> {
    let destination = vehicle.clone();
    browser
        .declare_subscriber(PRIVATE_MOCAP_TOPIC)
        .callback(move |sample| {
            let payload = sample.payload().to_bytes();
            if flatbuffers::root::<MocapFrame<'_>>(&payload).is_err() {
                tracing::warn!("invalid private MocapFrame dropped at browser boundary");
                return;
            }
            if let Err(error) = destination
                .put(PRIVATE_MOCAP_TOPIC, payload)
                .encoding(sample.encoding().clone())
                .wait()
            {
                tracing::warn!(%error, "private MocapFrame relay failed");
            }
        })
        .wait()
        .map_err(|error| anyhow!("subscribe private MocapFrame: {error}"))
}

fn relay_verified_lan_mocap(
    external: &Session,
    vehicle: &Session,
    browser: &Session,
) -> Result<zenoh::pubsub::Subscriber<()>> {
    let vehicle_destination = vehicle.clone();
    let browser_destination = browser.clone();
    external
        .declare_subscriber(PUBLIC_MOCAP_TOPIC)
        .callback(move |sample| {
            let payload = sample.payload().to_bytes();
            if flatbuffers::root::<MocapFrame<'_>>(&payload).is_err() {
                tracing::warn!("invalid LAN MocapFrame dropped at external boundary");
                return;
            }
            if let Err(error) = vehicle_destination
                .put(PUBLIC_MOCAP_TOPIC, payload.clone())
                .encoding(sample.encoding().clone())
                .wait()
            {
                tracing::warn!(%error, "validated LAN MocapFrame relay failed");
            }
            if let Err(error) = browser_destination
                .put(PUBLIC_MOCAP_TOPIC, payload)
                .encoding(sample.encoding().clone())
                .wait()
            {
                tracing::warn!(%error, "validated LAN MocapFrame browser relay failed");
            }
        })
        .wait()
        .map_err(|error| anyhow!("subscribe LAN MocapFrame: {error}"))
}

fn relay_verified_rigid_body_names(
    telemetry: &Session,
    browser: &Session,
) -> Result<zenoh::pubsub::Subscriber<()>> {
    let destination = browser.clone();
    telemetry
        .declare_subscriber(RIGID_BODY_NAMES_TOPIC)
        .callback(move |sample| {
            let payload = sample.payload().to_bytes();
            let valid = payload.len() <= 16 * 1024
                && serde_json::from_slice::<serde_json::Value>(&payload)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("rigidBodies")
                            .and_then(|items| items.as_array())
                            .cloned()
                    })
                    .is_some();
            if !valid {
                tracing::warn!("invalid LAN rigid-body names payload dropped");
                return;
            }
            if let Err(error) = destination.put(RIGID_BODY_NAMES_TOPIC, payload).wait() {
                tracing::warn!(%error, "validated rigid-body names browser relay failed");
            }
        })
        .wait()
        .map_err(|error| anyhow!("subscribe LAN rigid-body names: {error}"))
}

fn relay_verified_raw_pose(
    telemetry: &Session,
    vehicle: &Session,
    browser: &Session,
) -> Result<zenoh::pubsub::Subscriber<()>> {
    let vehicle_destination = vehicle.clone();
    let browser_destination = browser.clone();
    telemetry
        .declare_subscriber(RAW_POSE_TOPIC)
        .callback(move |sample| {
            let key = sample.key_expr().as_str().to_string();
            let payload = sample.payload().to_bytes();
            // The Qualisys bridge is the source of truth for this stream. Its
            // raw pose uses Synapse 0.7 RawPoseData and must carry that exact
            // value contract.
            let encoding = sample.encoding().to_string();
            let normalized = encoding.strip_prefix("zenoh/bytes;").unwrap_or(&encoding);
            let contract_ok = synapse_fbs::value_contract::topic_for_encoding(normalized)
                .is_ok_and(|topic| topic.name == "RawPose");
            if !contract_ok {
                tracing::warn!(%key, encoding, "raw pose rejected: value contract mismatch");
                return;
            }
            let Ok(bytes) = <[u8; 40]>::try_from(payload.as_ref()) else {
                tracing::warn!(%key, bytes = payload.len(), "invalid LAN raw pose dropped");
                return;
            };
            let data = RawPoseData(bytes);
            let pose = data.pose();
            let position = pose.position_enu_m();
            let attitude = pose.attitude();
            if [
                position.x(),
                position.y(),
                position.z(),
                attitude.w(),
                attitude.x(),
                attitude.y(),
                attitude.z(),
            ]
            .iter()
            .any(|value| !value.is_finite())
            {
                tracing::warn!(%key, "non-finite LAN raw pose dropped");
                return;
            }
            if let Err(error) = vehicle_destination
                .put(key.clone(), payload.clone())
                .encoding(sample.encoding().clone())
                .wait()
            {
                tracing::warn!(%key, %error, "validated raw pose vehicle relay failed");
            }
            if let Err(error) = browser_destination
                .put(key.clone(), payload)
                .encoding(sample.encoding().clone())
                .wait()
            {
                tracing::warn!(%key, %error, "validated raw pose browser relay failed");
            }
        })
        .wait()
        .map_err(|error| anyhow!("subscribe LAN raw pose: {error}"))
}

fn publish_status(
    browser: &Session,
    config: &CommandAuthorityConfig,
    leaf: &str,
    status: &str,
    message: &str,
) {
    let key = format!(
        "{}/status/{leaf}",
        status_prefix(&config.policy.intent_prefix)
    );
    let payload = serde_json::json!({ "status": status, "message": message }).to_string();
    if let Err(error) = browser.put(key.clone(), payload).wait() {
        tracing::warn!(%key, %error, "browser status publish failed");
    }
}

fn publish_query_status(
    browser: &Session,
    config: &CommandAuthorityConfig,
    command: &AuthorizedCommand,
    status: &str,
    message: &str,
) {
    if command.velocity_remaining.is_some() {
        publish_velocity_command_status(browser, config, status, command, message);
    } else {
        publish_status(browser, config, &command.status_leaf, status, message);
    }
}

fn publish_velocity_command_status(
    browser: &Session,
    config: &CommandAuthorityConfig,
    status: &str,
    command: &AuthorizedCommand,
    message: &str,
) {
    let payload = serde_json::json!({
        "status": status,
        "message": message,
        "teamName": command.velocity_device,
        "credentialId": command.velocity_credential_id,
        "limit": command.velocity_limit,
        "used": command.velocity_used,
        "remaining": command.velocity_remaining,
        "budgetVersion": command.velocity_budget_version,
    });
    publish_velocity_payload(browser, config, payload);
}

fn publish_velocity_state(
    browser: &Session,
    config: &CommandAuthorityConfig,
    status: &str,
    state: &crate::velocity_budget::BudgetState,
    message: &str,
) {
    let payload = serde_json::json!({
        "status": status, "message": message, "teamName": state.device_id,
        "credentialId": state.credential_id, "limit": state.limit, "used": state.used,
        "remaining": state.remaining, "budgetVersion": state.budget_version,
    });
    publish_velocity_payload(browser, config, payload);
}

fn publish_unknown_velocity_status(
    browser: &Session,
    config: &CommandAuthorityConfig,
    credential_id: Option<&str>,
    message: &str,
) {
    let payload = serde_json::json!({ "status": "rejected", "message": message, "credentialId": credential_id });
    publish_velocity_payload(browser, config, payload);
}

fn publish_velocity_payload(
    browser: &Session,
    config: &CommandAuthorityConfig,
    payload: serde_json::Value,
) {
    let key = format!(
        "{}/status/velocity",
        status_prefix(&config.policy.intent_prefix)
    );
    if let Err(error) = browser.put(key.clone(), payload.to_string()).wait() {
        tracing::warn!(%key, %error, "browser velocity status publish failed");
    }
}

fn status_prefix(intent_prefix: &str) -> &str {
    intent_prefix.strip_suffix("/cmd").unwrap_or(intent_prefix)
}

fn open_session(mode: &str, listen: &str, connect: Option<&str>) -> Result<Session> {
    let mut config = zenoh::Config::default();
    config
        .insert_json5("mode", &format!("\"{mode}\""))
        .map_err(|error| anyhow!(error.to_string()))?;
    if !listen.trim().is_empty() {
        config
            .insert_json5("listen/endpoints", &format!("[\"{}\"]", listen.trim()))
            .map_err(|error| anyhow!(error.to_string()))?;
    }
    if let Some(connect) = connect.filter(|endpoint| !endpoint.trim().is_empty()) {
        config
            .insert_json5("connect/endpoints", &format!("[\"{}\"]", connect.trim()))
            .map_err(|error| anyhow!(error.to_string()))?;
    }
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .map_err(|error| anyhow!(error.to_string()))?;
    zenoh::open(config)
        .wait()
        .map_err(|error| anyhow!("open isolated Zenoh session: {error}"))
}

fn optional(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

fn value_or(key: &str, default: String) -> String {
    optional(key).unwrap_or(default)
}

fn path_or(key: &str, default: PathBuf) -> PathBuf {
    env::var_os(key).map(PathBuf::from).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::{
        command_reply_status, ensure_loopback_browser_endpoint, ensure_loopback_vehicle_endpoint,
        parameter_reply_status, should_announce_topic,
    };
    use flatbuffers::FlatBufferBuilder;
    use synapse_fbs::cmd::{ParamSetReply, ParamSetReplyArgs};
    use synapse_fbs::types::CommandResultCode;

    fn reply(result: CommandResultCode, result_detail: i32) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::new();
        let reply = ParamSetReply::create(
            &mut builder,
            &ParamSetReplyArgs {
                result,
                result_detail,
                ..Default::default()
            },
        );
        builder.finish(reply, None);
        builder.finished_data().to_vec()
    }

    #[test]
    fn parameter_reply_result_controls_browser_status() {
        assert_eq!(
            parameter_reply_status(&reply(CommandResultCode::Accepted, 0), "gain")
                .unwrap()
                .0,
            "accepted"
        );
        assert_eq!(
            parameter_reply_status(&reply(CommandResultCode::InProgress, 0), "gain")
                .unwrap()
                .0,
            "in_progress"
        );
        let denied = parameter_reply_status(&reply(CommandResultCode::Denied, 7), "gain")
            .unwrap_err()
            .to_string();
        assert!(denied.contains("Denied"));
        assert!(denied.contains("detail 7"));

        let velocity =
            command_reply_status("velocity", &reply(CommandResultCode::Accepted, 0)).unwrap();
        assert_eq!(velocity.0, "accepted");
        assert_eq!(velocity.1, "target service accepted the velocity setpoint");
    }

    #[test]
    fn trusted_vehicle_endpoint_rejects_lan_binds_and_connects() {
        assert!(ensure_loopback_vehicle_endpoint("udp/127.0.0.1:7447").is_ok());
        assert!(ensure_loopback_vehicle_endpoint("udp/localhost:7447").is_ok());
        assert!(ensure_loopback_vehicle_endpoint("udp/0.0.0.0:7447").is_err());
        assert!(ensure_loopback_vehicle_endpoint("udp/192.168.10.2:7447").is_err());
    }

    #[test]
    fn trusted_browser_endpoint_rejects_lan_binds() {
        assert!(ensure_loopback_browser_endpoint("ws/127.0.0.1:7447").is_ok());
        assert!(ensure_loopback_browser_endpoint("ws/localhost:7447").is_ok());
        assert!(ensure_loopback_browser_endpoint("ws/0.0.0.0:7447").is_err());
        assert!(ensure_loopback_browser_endpoint("ws/192.168.10.3:7447").is_err());
    }

    #[test]
    fn synapse_catalog_announcements_are_immediate_and_throttled_per_key() {
        let announcements = Mutex::new(HashMap::new());
        let start = Instant::now();

        assert!(should_announce_topic(&announcements, "att", start));
        assert!(!should_announce_topic(
            &announcements,
            "att",
            start + Duration::from_millis(999)
        ));
        assert!(should_announce_topic(
            &announcements,
            "health",
            start + Duration::from_millis(999)
        ));
        assert!(should_announce_topic(
            &announcements,
            "att",
            start + Duration::from_secs(1)
        ));
    }
}

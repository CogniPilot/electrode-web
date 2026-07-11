//! Live Zenoh subscription, topic discovery, and telemetry forwarding.
//!
//! A background thread opens a Zenoh session and subscribes to a wildcard key
//! expression (default `synapse/**`). Every sample updates a discovery registry
//! (so operators can *see* what is being published) and, when its key is
//! selected, is decoded and broadcast to connected browsers as a telemetry
//! frame (so operators can *receive* it).

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::json;
use tokio::sync::broadcast;
use zenoh::{config::Config, Wait};

use crate::synapse_decode;

/// Runtime knobs for the Zenoh subscriber, sourced from the environment.
#[derive(Debug, Clone)]
pub(crate) struct ZenohConfig {
    /// Zenoh session mode: `client` (dial a router/peer) or `peer` (host the
    /// network so hardware bridges can connect directly into the ground station).
    pub mode: String,
    /// Zenoh router locator to dial when in client mode, e.g. `udp/127.0.0.1:7447`.
    /// Empty when running as a peer that only listens.
    pub connect: String,
    /// Locator to listen on when hosting the network as a peer, e.g.
    /// `udp/0.0.0.0:7447` (for native hardware / autopilots).
    pub listen: Option<String>,
    /// Extra WebSocket listen locator for browser (zenoh-wasm) clients — the
    /// viewer and the in-browser sim can only reach Zenoh over WebSocket. Kept
    /// separate from `listen` because ws and a same-port tcp listener would
    /// collide, so the hub hosts `udp/…:7447` (hardware) + `ws/…:7447` (browser).
    pub ws_listen: Option<String>,
    /// Wildcard key expression to discover, e.g. `**`. Compact 0.6.0 catalog
    /// keys live under arbitrary vehicle namespaces (`cub1/att`), so discovery
    /// defaults to everything and classification filters.
    pub keyexpr: String,
    /// Auto-subscribe (forward) topics we can decode as soon as they appear.
    pub auto_select_known: bool,
    /// Vehicle id stamped on forwarded frames.
    pub vehicle_id: String,
}

impl ZenohConfig {
    pub(crate) fn from_env(vehicle_id: String) -> Self {
        let mode = std::env::var("ELECTRODE_ZENOH_MODE")
            .unwrap_or_else(|_| "client".to_string())
            .to_lowercase();
        let connect_env = std::env::var("ELECTRODE_ZENOH_CONNECT").ok();
        let mut connect = connect_env
            .clone()
            .unwrap_or_else(|| "udp/127.0.0.1:7447".to_string());
        let mut listen = std::env::var("ELECTRODE_ZENOH_LISTEN").ok();
        let mut ws_listen = std::env::var("ELECTRODE_ZENOH_WS_LISTEN").ok();
        // Peer with no explicit listen locator: host the hub on the standard
        // endpoints and don't dial out, so `ELECTRODE_ZENOH_MODE=peer` alone makes
        // the ground station the rendezvous that hardware bridges *and browsers*
        // connect into.
        if mode == "peer" && listen.is_none() {
            // Bind all interfaces so a LAN autopilot / zephyr.exe on another host
            // can reach the hub. UDP (not TCP) so it can coexist with the ws
            // listener on the same port, matching electrode-fake-sim.
            listen = Some(connect_env.unwrap_or_else(|| "udp/0.0.0.0:7447".to_string()));
            connect = String::new();
        }
        // In peer mode, always offer a WebSocket listener for browsers unless one
        // was given or explicitly disabled with an empty ELECTRODE_ZENOH_WS_LISTEN.
        if mode == "peer" && ws_listen.is_none() {
            ws_listen = Some("ws/0.0.0.0:7447".to_string());
        }
        if ws_listen.as_deref() == Some("") {
            ws_listen = None;
        }
        let keyexpr = std::env::var("ELECTRODE_ZENOH_KEYEXPR").unwrap_or_else(|_| "**".to_string());
        let auto_select_known = std::env::var("ELECTRODE_ZENOH_AUTOSELECT")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        Self {
            mode,
            connect,
            listen,
            ws_listen,
            keyexpr,
            auto_select_known,
            vehicle_id,
        }
    }

    /// The locator to show in logs/status: the listen endpoint when hosting the
    /// network, otherwise the router we dial.
    pub(crate) fn endpoint_label(&self) -> &str {
        self.listen.as_deref().unwrap_or(&self.connect)
    }
}

/// Per-topic discovery statistics.
#[derive(Debug, Clone)]
struct TopicStat {
    schema: &'static str,
    decodable: bool,
    count: u64,
    prev_count: u64,
    last_bytes: usize,
    rate_hz: f32,
    last_seen_ms: u64,
}

/// One row in the discovery catalog sent to the browser.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogEntry {
    key: String,
    schema: &'static str,
    decodable: bool,
    selected: bool,
    count: u64,
    rate_hz: f32,
    last_bytes: usize,
    last_seen_ms: u64,
}

/// Shared state between the Zenoh thread, catalog emitter, and WebSocket clients.
pub(crate) struct ZenohShared {
    registry: Mutex<HashMap<String, TopicStat>>,
    selected: RwLock<std::collections::HashSet<String>>,
    tx: broadcast::Sender<String>,
    sequence: AtomicU64,
    vehicle_id: String,
    auto_select_known: bool,
    connected: RwLock<bool>,
    endpoint: String,
}

impl ZenohShared {
    pub(crate) fn new(config: &ZenohConfig, tx: broadcast::Sender<String>) -> Arc<Self> {
        Arc::new(Self {
            registry: Mutex::new(HashMap::new()),
            selected: RwLock::new(std::collections::HashSet::new()),
            tx,
            sequence: AtomicU64::new(1),
            vehicle_id: config.vehicle_id.clone(),
            auto_select_known: config.auto_select_known,
            connected: RwLock::new(false),
            endpoint: config.endpoint_label().to_string(),
        })
    }

    /// A client just connected: replace its subscription selection.
    pub(crate) fn set_selection<I: IntoIterator<Item = String>>(&self, keys: I) {
        let mut selected = self.selected.write().expect("selected lock poisoned");
        *selected = keys.into_iter().collect();
    }

    /// Serialize the current discovery catalog as a `topicCatalog` message.
    pub(crate) fn catalog_message(&self) -> String {
        let selected = self.selected.read().expect("selected lock poisoned");
        let registry = self.registry.lock().expect("registry lock poisoned");
        let mut entries: Vec<CatalogEntry> = registry
            .iter()
            .map(|(key, stat)| CatalogEntry {
                key: key.clone(),
                schema: stat.schema,
                decodable: stat.decodable,
                selected: selected.contains(key),
                count: stat.count,
                rate_hz: stat.rate_hz,
                last_bytes: stat.last_bytes,
                last_seen_ms: stat.last_seen_ms,
            })
            .collect();
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        serde_json::to_string(&json!({
            "kind": "topicCatalog",
            "connected": *self.connected.read().expect("connected lock poisoned"),
            "endpoint": self.endpoint,
            "generatedAtMs": now_ms(),
            "topics": entries,
        }))
        .unwrap_or_else(|_| "{\"kind\":\"topicCatalog\",\"topics\":[]}".to_string())
    }

    fn record_sample(&self, key: &str, bytes: &[u8]) {
        let schema = synapse_decode::classify(key);
        let now = now_ms();
        let mut registry = self.registry.lock().expect("registry lock poisoned");
        let stat = registry.entry(key.to_string()).or_insert_with(|| {
            self.auto_select_topic(key, schema);
            TopicStat {
                schema,
                decodable: schema != "Raw",
                count: 0,
                prev_count: 0,
                last_bytes: 0,
                rate_hz: 0.0,
                last_seen_ms: now,
            }
        });
        stat.count += 1;
        stat.last_bytes = bytes.len();
        stat.last_seen_ms = now;
    }

    fn auto_select_topic(&self, key: &str, schema: &str) {
        // Auto-select decodable topics on first sight so data flows immediately.
        if !self.auto_select_known || schema == "Raw" {
            return;
        }
        if let Ok(mut selected) = self.selected.write() {
            selected.insert(key.to_string());
        }
    }

    fn is_selected(&self, key: &str) -> bool {
        self.selected
            .read()
            .expect("selected lock poisoned")
            .contains(key)
    }

    fn build_frame(&self, key: &str, encoding: &str, bytes: &[u8]) -> String {
        // Zenoh's default encoding means the publisher stamped nothing.
        let encoding = Some(encoding).filter(|e| !e.is_empty() && *e != "zenoh/bytes");
        let decoded = synapse_decode::decode(key, encoding, bytes);
        let now = now_ms();
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let frame = json!({
            "kind": "telemetry",
            "topic": key,
            "header": {
                "sequence": sequence,
                "sourceTimeNs": now * 1_000_000,
                "receiveTimeNs": now * 1_000_000,
                "expireTimeNs": 0,
                "vehicleId": self.vehicle_id,
                "schemaVersion": electrode_web_core::SCHEMA_VERSION,
                "messageType": decoded.schema,
                "priority": "normal",
                "streamId": key,
            },
            "payload": decoded.payload,
        });
        frame.to_string()
    }

    fn recompute_rates(&self) {
        let mut registry = self.registry.lock().expect("registry lock poisoned");
        for stat in registry.values_mut() {
            let delta = stat.count.saturating_sub(stat.prev_count);
            stat.prev_count = stat.count;
            stat.rate_hz = delta as f32 / CATALOG_INTERVAL.as_secs_f32();
        }
    }
}

const CATALOG_INTERVAL: Duration = Duration::from_millis(500);

/// Spawn the Zenoh subscriber thread and the periodic catalog emitter.
pub(crate) fn spawn(shared: Arc<ZenohShared>, config: ZenohConfig) {
    let subscriber_shared = Arc::clone(&shared);
    thread::Builder::new()
        .name("zenoh-subscriber".to_string())
        .spawn(move || run_subscriber(subscriber_shared, config))
        .expect("failed to spawn zenoh subscriber thread");

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(CATALOG_INTERVAL);
        loop {
            ticker.tick().await;
            shared.recompute_rates();
            // Ignore send errors: they just mean no browser is connected yet.
            let _ = shared.tx.send(shared.catalog_message());
        }
    });
}

fn run_subscriber(shared: Arc<ZenohShared>, config: ZenohConfig) {
    // In a listening (peer) mode we expect these locators to actually bind.
    let expected_listeners: Vec<String> = [config.listen.clone(), config.ws_listen.clone()]
        .into_iter()
        .flatten()
        .filter(|locator| !locator.trim().is_empty())
        .collect();

    // Open, then verify the kernel actually bound our listeners — Zenoh logs
    // "reachable at …" even when a listener's bind silently fails (observed on
    // rapid restarts / port reuse), which would leave the hub advertising an
    // endpoint nothing is listening on. Retry a few times, then fail loudly.
    let mut attempt = 0;
    let session = loop {
        attempt += 1;
        let zconfig = match zenoh_config(&config) {
            Ok(zconfig) => zconfig,
            Err(err) => {
                tracing::error!(%err, "invalid zenoh config");
                return;
            }
        };

        let session = match zenoh::open(zconfig).wait() {
            Ok(session) => session,
            Err(err) => {
                tracing::warn!(%err, endpoint = %config.endpoint_label(), "zenoh session open failed; discovery disabled until a router is reachable");
                return;
            }
        };

        if expected_listeners.is_empty() {
            break session;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let unbound = electrode_web_core::unbound_listeners(&expected_listeners);
        if unbound.is_empty() {
            break session;
        }
        if attempt >= 5 {
            tracing::error!(
                ?unbound,
                attempts = attempt,
                "Zenoh hub listener(s) never bound; browsers/clients cannot connect. Another \
                 process may hold the port(s) — check `ss -lnp | grep 7447`. Giving up."
            );
            return;
        }
        tracing::warn!(
            ?unbound,
            attempt,
            "Zenoh listener(s) not bound; closing and retrying"
        );
        let _ = session.close().wait();
        std::thread::sleep(std::time::Duration::from_millis(400 * attempt));
    };

    let subscriber = match session.declare_subscriber(config.keyexpr.clone()).wait() {
        Ok(subscriber) => subscriber,
        Err(err) => {
            tracing::error!(%err, keyexpr = %config.keyexpr, "failed to declare zenoh subscriber");
            return;
        }
    };

    if let Ok(mut connected) = shared.connected.write() {
        *connected = true;
    }
    tracing::info!(mode = %config.mode, endpoint = %config.endpoint_label(), keyexpr = %config.keyexpr, "zenoh subscriber active");

    while let Ok(sample) = subscriber.recv() {
        let key = sample.key_expr().as_str().to_string();
        let bytes = sample.payload().to_bytes();
        let encoding = sample.encoding().to_string();
        shared.record_sample(&key, &bytes);
        if shared.is_selected(&key) {
            let _ = shared.tx.send(shared.build_frame(&key, &encoding, &bytes));
        }
    }

    if let Ok(mut connected) = shared.connected.write() {
        *connected = false;
    }
    tracing::warn!("zenoh subscriber stream ended");
}

fn zenoh_config(config: &ZenohConfig) -> anyhow::Result<Config> {
    let mut zconfig = Config::default();
    zconfig
        .insert_json5("mode", &format!("\"{}\"", config.mode))
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if !config.connect.is_empty() {
        zconfig
            .insert_json5("connect/endpoints", &format!("[\"{}\"]", config.connect))
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    }
    let listen_endpoints: Vec<String> = [config.listen.as_deref(), config.ws_listen.as_deref()]
        .into_iter()
        .flatten()
        .filter(|locator| !locator.is_empty())
        .map(|locator| format!("\"{locator}\""))
        .collect();
    if !listen_endpoints.is_empty() {
        zconfig
            .insert_json5(
                "listen/endpoints",
                &format!("[{}]", listen_endpoints.join(",")),
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    }
    zconfig
        .insert_json5("scouting/multicast/enabled", "false")
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(zconfig)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}

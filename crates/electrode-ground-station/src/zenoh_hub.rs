//! Ground-station-owned Zenoh rendezvous.
//!
//! The browser/WASM side can only use Zenoh over WebSocket, while native local
//! processes use UDP. Owning both listeners here removes the fragile startup
//! dance where a separate process had to be running before the autopilot,
//! controller, or sim could communicate.

use zenoh::Wait;

#[derive(Debug, Clone)]
pub struct ZenohHubConfig {
    pub enabled: bool,
    pub mode: String,
    pub listen: String,
    pub ws_listen: String,
    pub connect: String,
}

impl ZenohHubConfig {
    pub fn from_env() -> Self {
        let enabled = std::env::var("ELECTRODE_GCS_ZENOH_HUB")
            .map(|value| value != "0" && !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let listen = std::env::var("ELECTRODE_GCS_ZENOH_LISTEN")
            .unwrap_or_else(|_| "udp/0.0.0.0:7447".to_string());
        let ws_listen = std::env::var("ELECTRODE_GCS_ZENOH_WS_LISTEN")
            .unwrap_or_else(|_| "ws/0.0.0.0:7447".to_string());
        let connect = std::env::var("ELECTRODE_GCS_ZENOH_CONNECT").unwrap_or_default();
        Self {
            enabled,
            mode: "peer".to_string(),
            listen,
            ws_listen,
            connect,
        }
    }

    fn listeners(&self) -> Vec<String> {
        [self.listen.trim(), self.ws_listen.trim()]
            .into_iter()
            .filter(|locator| !locator.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn connections(&self) -> Vec<String> {
        self.connect
            .split(',')
            .map(str::trim)
            .filter(|locator| !locator.is_empty())
            .map(str::to_string)
            .collect()
    }
}

pub struct ZenohHub {
    _session: Option<zenoh::Session>,
    listeners: Vec<String>,
}

impl ZenohHub {
    pub fn start(config: ZenohHubConfig) -> anyhow::Result<Self> {
        if !config.enabled {
            tracing::info!("ground-station Zenoh hub disabled");
            return Ok(Self {
                _session: None,
                listeners: Vec::new(),
            });
        }

        let listeners = config.listeners();
        let connections = config.connections();
        if listeners.is_empty() {
            anyhow::bail!("Zenoh hub is enabled but no listen endpoints are configured");
        }

        let mut zconfig = zenoh::Config::default();
        zconfig
            .insert_json5("mode", &format!("\"{}\"", config.mode))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let listen_json = listeners
            .iter()
            .map(|endpoint| format!("\"{endpoint}\""))
            .collect::<Vec<_>>()
            .join(",");
        zconfig
            .insert_json5("listen/endpoints", &format!("[{listen_json}]"))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if !connections.is_empty() {
            let connect_json = connections
                .iter()
                .map(|endpoint| format!("\"{endpoint}\""))
                .collect::<Vec<_>>()
                .join(",");
            zconfig
                .insert_json5("connect/endpoints", &format!("[{connect_json}]"))
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        }
        zconfig
            .insert_json5("scouting/multicast/enabled", "false")
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;

        let session = zenoh::open(zconfig)
            .wait()
            .map_err(|error| anyhow::anyhow!("zenoh hub open failed: {error}"))?;

        // Zenoh can report success before a listener has really bound. Give it a
        // short settling window, then verify the kernel sockets.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let unbound = electrode_web_core::unbound_listeners(&listeners);
        if !unbound.is_empty() {
            let _ = session.close().wait();
            anyhow::bail!(
                "Zenoh hub listener(s) did not bind: {unbound:?}. \
                 Stop any stale process on port 7447 and restart Ground Station."
            );
        }

        tracing::info!(?listeners, ?connections, "ground-station Zenoh hub listening");
        Ok(Self {
            _session: Some(session),
            listeners,
        })
    }

    pub fn listeners(&self) -> &[String] {
        &self.listeners
    }
}

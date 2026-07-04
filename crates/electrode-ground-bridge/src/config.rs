use std::{env, net::SocketAddr};

#[derive(Debug, Clone)]
pub(crate) struct BridgeConfig {
    pub bind_addr: SocketAddr,
    pub vehicle_id: String,
}

impl BridgeConfig {
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        let bind_addr = env::var("ELECTRODE_BRIDGE_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8787".to_string())
            .parse()?;
        let vehicle_id =
            env::var("ELECTRODE_VEHICLE_ID").unwrap_or_else(|_| "electrode-01".to_string());

        Ok(Self {
            bind_addr,
            vehicle_id,
        })
    }
}

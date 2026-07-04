use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};

use serde_json::Value;
use tokio::sync::broadcast;

use crate::zenoh_bridge::ZenohShared;

#[derive(Debug, Clone)]
pub(crate) struct VehicleRuntime {
    pub mode: String,
    pub armed: bool,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub vehicle_id: String,
    command_sequence: Arc<AtomicU64>,
    runtime: Arc<RwLock<VehicleRuntime>>,
    pub zenoh: Arc<ZenohShared>,
    pub frame_tx: broadcast::Sender<String>,
}

impl AppState {
    pub(crate) fn new(
        vehicle_id: impl Into<String>,
        zenoh: Arc<ZenohShared>,
        frame_tx: broadcast::Sender<String>,
    ) -> Self {
        Self {
            vehicle_id: vehicle_id.into(),
            command_sequence: Arc::new(AtomicU64::new(0)),
            runtime: Arc::new(RwLock::new(VehicleRuntime {
                mode: "hold".to_string(),
                armed: true,
            })),
            zenoh,
            frame_tx,
        }
    }

    pub(crate) fn accept_command_sequence(&self, sequence: u64) -> bool {
        let mut current = self.command_sequence.load(Ordering::Relaxed);
        loop {
            if sequence <= current {
                return false;
            }

            match self.command_sequence.compare_exchange(
                current,
                sequence,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(next) => current = next,
            }
        }
    }

    pub(crate) fn apply_command_effect(&self, command: &str, args: &Value) {
        let mut runtime = self
            .runtime
            .write()
            .expect("vehicle runtime lock is poisoned");
        match command {
            "arm" => runtime.armed = true,
            "disarm" => runtime.armed = false,
            "setMode" => {
                if let Some(mode) = args.get("mode").and_then(Value::as_str) {
                    runtime.mode = mode.to_string();
                }
            }
            "land" => runtime.mode = "land".to_string(),
            "return" => runtime.mode = "return".to_string(),
            _ => {}
        }
    }
}

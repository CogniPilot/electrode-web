//! Ground Station boundary for the in-browser Rumoca plant.
//!
//! Rumoca WASM is a private plant behind the Ground Station. Public Synapse
//! topics stay on the hardware/autopilot side; this bridge mirrors only the
//! selected data into private `electrode/sim/rumoca/*` topics and republishes
//! plant pose back as the public mocap topic real hardware would provide.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use electrode_ppm_bridge::{
    FAILSAFE_CHANNELS, PpmChannels, channels_to_pwm_signal_outputs_payload,
    manual_control_to_channels, pwm_signal_outputs_to_channels,
};
use synapse_fbs::topic::{ManualControlData, ManualControlFlags};
use zenoh::Wait;

pub const PRIVATE_RADIO_PWM_TOPIC: &str = "electrode/sim/rumoca/radio_pwm_signal_outputs";
pub const PRIVATE_MOCAP_TOPIC: &str = "electrode/sim/rumoca/mocap_frame";

const PUBLIC_PWM_TOPIC: &str = "synapse/v1/topic/pwm_signal_outputs";
const PUBLIC_MANUAL_TOPIC: &str = "synapse/v1/topic/manual_control_command";
const PUBLIC_MOCAP_TOPIC: &str = "synapse/mocap/rigid_body/cub1/pose";
const MANUAL_CONTROL_PAYLOAD_SIZE: usize = 40;

pub struct SimBridge {
    _session: zenoh::Session,
    _subscribers: Vec<zenoh::pubsub::Subscriber<()>>,
    radio_pwm_frames: Arc<AtomicU64>,
    mocap_frames: Arc<AtomicU64>,
}

#[derive(Debug, Clone, Copy)]
enum ManualMode {
    Manual,
    Auto,
    Failsafe,
}

struct RadioState {
    manual_mode: ManualMode,
    manual_channels: PpmChannels,
    manual_pwm_payload: Vec<u8>,
    control_channels: Option<PpmChannels>,
    control_pwm_payload: Option<Vec<u8>>,
}

impl SimBridge {
    pub fn start(endpoint: &str) -> anyhow::Result<Self> {
        let mut config = zenoh::Config::default();
        config
            .insert_json5("mode", "\"peer\"")
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let endpoint = endpoint.trim();
        if !endpoint.is_empty() {
            config
                .insert_json5("connect/endpoints", &format!("[\"{endpoint}\"]"))
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        }

        let session = zenoh::open(config)
            .wait()
            .map_err(|error| anyhow::anyhow!("sim bridge zenoh open failed: {error}"))?;

        let radio_pwm_frames = Arc::new(AtomicU64::new(0));
        let mocap_frames = Arc::new(AtomicU64::new(0));
        let radio_state = Arc::new(Mutex::new(RadioState {
            manual_mode: ManualMode::Failsafe,
            manual_channels: PpmChannels(FAILSAFE_CHANNELS),
            manual_pwm_payload: channels_to_pwm_signal_outputs_payload(PpmChannels(
                FAILSAFE_CHANNELS,
            )),
            control_channels: None,
            control_pwm_payload: None,
        }));
        let mut subscribers = Vec::new();

        {
            let subscribe_session = session.clone();
            let publish_session = session.clone();
            let state = radio_state.clone();
            let count = radio_pwm_frames.clone();
            subscribers.push(
                subscribe_session
                    .declare_subscriber(PUBLIC_PWM_TOPIC)
                    .callback(move |sample| {
                        let bytes = sample.payload().to_bytes();
                        let Some(channels) = pwm_signal_outputs_to_channels(&bytes) else {
                            return;
                        };
                        {
                            let mut state = state.lock().expect("sim radio state lock poisoned");
                            state.control_channels = Some(channels);
                            state.control_pwm_payload = Some(bytes.to_vec());
                        }
                        publish_selected_radio_pwm(&publish_session, &state, &count);
                    })
                    .wait()
                    .map_err(|error| {
                        anyhow::anyhow!("sim bridge subscribe {PUBLIC_PWM_TOPIC} failed: {error}")
                    })?,
            );
        }

        {
            let subscribe_session = session.clone();
            let publish_session = session.clone();
            let state = radio_state.clone();
            let count = radio_pwm_frames.clone();
            subscribers.push(
                subscribe_session
                    .declare_subscriber(PUBLIC_MANUAL_TOPIC)
                    .callback(move |sample| {
                        let payload = sample.payload().to_bytes();
                        let Some((mode, channels, pwm_payload)) =
                            manual_selection_from_payload(&payload)
                        else {
                            return;
                        };
                        {
                            let mut state = state.lock().expect("sim radio state lock poisoned");
                            state.manual_mode = mode;
                            state.manual_channels = channels;
                            state.manual_pwm_payload = pwm_payload;
                        }
                        publish_selected_radio_pwm(&publish_session, &state, &count);
                    })
                    .wait()
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "sim bridge subscribe {PUBLIC_MANUAL_TOPIC} failed: {error}"
                        )
                    })?,
            );
        }

        {
            let subscribe_session = session.clone();
            let publish_session = session.clone();
            let count = mocap_frames.clone();
            subscribers.push(
                subscribe_session
                    .declare_subscriber(PRIVATE_MOCAP_TOPIC)
                    .callback(move |sample| {
                        let bytes = sample.payload().to_bytes();
                        if publish_session
                            .put(PUBLIC_MOCAP_TOPIC, bytes.to_vec())
                            .wait()
                            .is_ok()
                        {
                            count.fetch_add(1, Ordering::Relaxed);
                        }
                    })
                    .wait()
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "sim bridge subscribe {PRIVATE_MOCAP_TOPIC} failed: {error}"
                        )
                    })?,
            );
        }

        tracing::info!(
            radio_pwm = PRIVATE_RADIO_PWM_TOPIC,
            mocap_in = PRIVATE_MOCAP_TOPIC,
            mocap_out = PUBLIC_MOCAP_TOPIC,
            "ground-station Rumoca WASM bridge listening"
        );

        Ok(Self {
            _session: session,
            _subscribers: subscribers,
            radio_pwm_frames,
            mocap_frames,
        })
    }

    pub fn counts(&self) -> SimBridgeCounts {
        SimBridgeCounts {
            radio_pwm_frames: self.radio_pwm_frames.load(Ordering::Relaxed),
            mocap_frames: self.mocap_frames.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimBridgeCounts {
    pub radio_pwm_frames: u64,
    pub mocap_frames: u64,
}

fn manual_selection_from_payload(payload: &[u8]) -> Option<(ManualMode, PpmChannels, Vec<u8>)> {
    if payload.len() != MANUAL_CONTROL_PAYLOAD_SIZE {
        return None;
    }
    let data = unsafe { <ManualControlData as flatbuffers::Follow>::follow(payload, 0) };
    let flags = ManualControlFlags::from_bits_retain(data.flags());
    let mode = if !flags.contains(ManualControlFlags::Valid)
        || flags.contains(ManualControlFlags::KillSwitch)
    {
        ManualMode::Failsafe
    } else if data.flight_mode() == 0 {
        ManualMode::Manual
    } else {
        ManualMode::Auto
    };
    let channels = manual_control_to_channels(&data);
    Some((
        mode,
        channels,
        channels_to_pwm_signal_outputs_payload(channels),
    ))
}

fn publish_selected_radio_pwm(
    session: &zenoh::Session,
    state: &Mutex<RadioState>,
    count: &AtomicU64,
) {
    let payload = {
        let state = state.lock().expect("sim radio state lock poisoned");
        match state.manual_mode {
            ManualMode::Manual => state.manual_pwm_payload.clone(),
            ManualMode::Auto => state.control_pwm_payload.clone().unwrap_or_else(|| {
                channels_to_pwm_signal_outputs_payload(PpmChannels(FAILSAFE_CHANNELS))
            }),
            ManualMode::Failsafe => {
                channels_to_pwm_signal_outputs_payload(PpmChannels(FAILSAFE_CHANNELS))
            }
        }
    };
    if session.put(PRIVATE_RADIO_PWM_TOPIC, payload).wait().is_ok() {
        count.fetch_add(1, Ordering::Relaxed);
    }
}

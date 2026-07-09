//! Ground Station boundary for the in-browser Rumoca WASM sim plant.
//!
//! The plant is private behind the Ground Station: it publishes MocapFrame
//! FlatBuffers on `electrode/sim/rumoca/mocap_frame`, and this bridge
//! republishes them on the exact public wire contract the Qualisys bridge
//! (synapse_qualisys_bridge) uses, so simulation traffic is byte-compatible
//! with a real mocap system:
//!   - `synapse/mocap/frame`                  — MocapFrame FlatBuffer, verbatim
//!   - `synapse/mocap/rigid_body/cub1/pose`   — compact 28-byte pose
//!     (little-endian f32 `[px, py, pz, qx, qy, qz, qw]`, ENU metres, w last)

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use electrode_ppm_bridge::{
    channels_to_pwm_signal_outputs_payload, manual_control_to_channels,
    pwm_signal_outputs_to_channels, PpmChannels, FAILSAFE_CHANNELS,
};
use synapse_fbs::topic::{ManualControlData, ManualControlFlags, MocapFrame, MocapRigidBodyData};
use synapse_fbs::types::RotationMatrix3f;
use zenoh::Wait;

pub(crate) const PRIVATE_RADIO_PWM_TOPIC: &str = "electrode/sim/rumoca/radio_pwm_signal_outputs";
pub(crate) const PRIVATE_MOCAP_TOPIC: &str = "electrode/sim/rumoca/mocap_frame";

const PUBLIC_PWM_TOPIC: &str = "synapse/v1/topic/pwm_signal_outputs";
const PUBLIC_MANUAL_TOPIC: &str = "synapse/v1/topic/manual_control_command";
const PUBLIC_MOCAP_FRAME_TOPIC: &str = "synapse/mocap/frame";
const PUBLIC_MOCAP_POSE_TOPIC: &str = "synapse/mocap/rigid_body/cub1/pose";
const MANUAL_CONTROL_PAYLOAD_SIZE: usize = 40;

pub(crate) struct SimBridge {
    _session: zenoh::Session,
    _subscribers: Vec<zenoh::pubsub::Subscriber<()>>,
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
    pub(crate) fn start(endpoint: &str) -> anyhow::Result<Self> {
        let session = open_session(endpoint)?;
        let radio_pwm_frames = Arc::new(AtomicU64::new(0));
        let mocap_frames = Arc::new(AtomicU64::new(0));
        let radio_state = initial_radio_state();
        let subscribers = vec![
            subscribe_public_pwm(&session, radio_state.clone(), radio_pwm_frames.clone())?,
            subscribe_public_manual(&session, radio_state, radio_pwm_frames.clone())?,
            subscribe_private_mocap(&session, mocap_frames.clone())?,
        ];

        tracing::info!(
            radio_pwm = PRIVATE_RADIO_PWM_TOPIC,
            mocap_in = PRIVATE_MOCAP_TOPIC,
            mocap_frame_out = PUBLIC_MOCAP_FRAME_TOPIC,
            mocap_pose_out = PUBLIC_MOCAP_POSE_TOPIC,
            "ground-station sim plant bridge listening"
        );

        Ok(Self {
            _session: session,
            _subscribers: subscribers,
        })
    }
}

fn open_session(endpoint: &str) -> anyhow::Result<zenoh::Session> {
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

    zenoh::open(config)
        .wait()
        .map_err(|error| anyhow::anyhow!("sim bridge zenoh open failed: {error}"))
}

fn initial_radio_state() -> Arc<Mutex<RadioState>> {
    Arc::new(Mutex::new(RadioState {
        manual_mode: ManualMode::Failsafe,
        manual_channels: PpmChannels(FAILSAFE_CHANNELS),
        manual_pwm_payload: channels_to_pwm_signal_outputs_payload(PpmChannels(FAILSAFE_CHANNELS)),
        control_channels: None,
        control_pwm_payload: None,
    }))
}

fn subscribe_public_pwm(
    session: &zenoh::Session,
    state: Arc<Mutex<RadioState>>,
    count: Arc<AtomicU64>,
) -> anyhow::Result<zenoh::pubsub::Subscriber<()>> {
    let subscribe_session = session.clone();
    let publish_session = session.clone();
    subscribe_session
        .declare_subscriber(PUBLIC_PWM_TOPIC)
        .callback(move |sample| {
            let bytes = sample.payload().to_bytes();
            handle_public_pwm_sample(&publish_session, state.as_ref(), &count, &bytes);
        })
        .wait()
        .map_err(|error| anyhow::anyhow!("sim bridge subscribe {PUBLIC_PWM_TOPIC} failed: {error}"))
}

fn subscribe_public_manual(
    session: &zenoh::Session,
    state: Arc<Mutex<RadioState>>,
    count: Arc<AtomicU64>,
) -> anyhow::Result<zenoh::pubsub::Subscriber<()>> {
    let subscribe_session = session.clone();
    let publish_session = session.clone();
    subscribe_session
        .declare_subscriber(PUBLIC_MANUAL_TOPIC)
        .callback(move |sample| {
            let payload = sample.payload().to_bytes();
            handle_public_manual_sample(&publish_session, state.as_ref(), &count, &payload);
        })
        .wait()
        .map_err(|error| {
            anyhow::anyhow!("sim bridge subscribe {PUBLIC_MANUAL_TOPIC} failed: {error}")
        })
}

fn subscribe_private_mocap(
    session: &zenoh::Session,
    count: Arc<AtomicU64>,
) -> anyhow::Result<zenoh::pubsub::Subscriber<()>> {
    let subscribe_session = session.clone();
    let publish_session = session.clone();
    subscribe_session
        .declare_subscriber(PRIVATE_MOCAP_TOPIC)
        .callback(move |sample| {
            let bytes = sample.payload().to_bytes();
            publish_mocap_sample(&publish_session, &count, &bytes);
        })
        .wait()
        .map_err(|error| {
            anyhow::anyhow!("sim bridge subscribe {PRIVATE_MOCAP_TOPIC} failed: {error}")
        })
}

fn handle_public_pwm_sample(
    session: &zenoh::Session,
    state: &Mutex<RadioState>,
    count: &AtomicU64,
    bytes: &[u8],
) {
    let Some(channels) = pwm_signal_outputs_to_channels(bytes) else {
        return;
    };
    {
        let mut state = state.lock().expect("sim radio state lock poisoned");
        state.control_channels = Some(channels);
        state.control_pwm_payload = Some(bytes.to_vec());
    }
    publish_selected_radio_pwm(session, state, count);
}

fn handle_public_manual_sample(
    session: &zenoh::Session,
    state: &Mutex<RadioState>,
    count: &AtomicU64,
    payload: &[u8],
) {
    let Some((mode, channels, pwm_payload)) = manual_selection_from_payload(payload) else {
        return;
    };
    {
        let mut state = state.lock().expect("sim radio state lock poisoned");
        state.manual_mode = mode;
        state.manual_channels = channels;
        state.manual_pwm_payload = pwm_payload;
    }
    publish_selected_radio_pwm(session, state, count);
}

fn publish_mocap_sample(session: &zenoh::Session, count: &AtomicU64, bytes: &[u8]) {
    // The plant publishes schema-verified MocapFrame FlatBuffers; drop anything
    // else rather than republishing garbage on the public wire.
    let Ok(frame) = flatbuffers::root::<MocapFrame>(bytes) else {
        tracing::debug!("sim bridge dropped non-MocapFrame payload on private mocap topic");
        return;
    };
    let Some(body) = frame.rigid_bodies().and_then(|bodies| bodies.iter().next()) else {
        return;
    };

    let frame_ok = session
        .put(PUBLIC_MOCAP_FRAME_TOPIC, bytes.to_vec())
        .wait()
        .is_ok();
    let pose_ok = session
        .put(PUBLIC_MOCAP_POSE_TOPIC, compact_pose_payload(body).to_vec())
        .wait()
        .is_ok();
    if frame_ok && pose_ok {
        count.fetch_add(1, Ordering::Relaxed);
    }
}

/// Compact per-rigid-body pose exactly as synapse_qualisys_bridge encodes it:
/// little-endian f32 `[px, py, pz, qx, qy, qz, qw]` (ENU metres, scalar last).
fn compact_pose_payload(body: &MocapRigidBodyData) -> [u8; 28] {
    let position = body.position_enu_m();
    let attitude = rotation_matrix_to_quaternion(body.rotation());
    let values = [
        position.x(),
        position.y(),
        position.z(),
        attitude.0,
        attitude.1,
        attitude.2,
        attitude.3,
    ];
    let mut payload = [0u8; 28];
    for (index, value) in values.into_iter().enumerate() {
        payload[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    payload
}

fn rotation_matrix_to_quaternion(rotation: &RotationMatrix3f) -> (f32, f32, f32, f32) {
    let trace = rotation.r11() + rotation.r22() + rotation.r33();
    let quaternion = if trace > 0.0 {
        let scale = (trace + 1.0).sqrt() * 2.0;
        (
            (rotation.r32() - rotation.r23()) / scale,
            (rotation.r13() - rotation.r31()) / scale,
            (rotation.r21() - rotation.r12()) / scale,
            0.25 * scale,
        )
    } else if rotation.r11() > rotation.r22() && rotation.r11() > rotation.r33() {
        let scale = (1.0 + rotation.r11() - rotation.r22() - rotation.r33()).sqrt() * 2.0;
        (
            0.25 * scale,
            (rotation.r12() + rotation.r21()) / scale,
            (rotation.r13() + rotation.r31()) / scale,
            (rotation.r32() - rotation.r23()) / scale,
        )
    } else if rotation.r22() > rotation.r33() {
        let scale = (1.0 + rotation.r22() - rotation.r11() - rotation.r33()).sqrt() * 2.0;
        (
            (rotation.r12() + rotation.r21()) / scale,
            0.25 * scale,
            (rotation.r23() + rotation.r32()) / scale,
            (rotation.r13() - rotation.r31()) / scale,
        )
    } else {
        let scale = (1.0 + rotation.r33() - rotation.r11() - rotation.r22()).sqrt() * 2.0;
        (
            (rotation.r13() + rotation.r31()) / scale,
            (rotation.r23() + rotation.r32()) / scale,
            0.25 * scale,
            (rotation.r21() - rotation.r12()) / scale,
        )
    };
    normalize_quaternion(quaternion)
}

fn normalize_quaternion(quaternion: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let norm = (quaternion.0.mul_add(
        quaternion.0,
        quaternion.1.mul_add(
            quaternion.1,
            quaternion
                .2
                .mul_add(quaternion.2, quaternion.3 * quaternion.3),
        ),
    ))
    .sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return (0.0, 0.0, 0.0, 1.0);
    }
    (
        quaternion.0 / norm,
        quaternion.1 / norm,
        quaternion.2 / norm,
        quaternion.3 / norm,
    )
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
    let channels = manual_control_to_channels(data);
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

#[cfg(test)]
mod tests {
    use super::*;
    use synapse_fbs::topic::{MocapFrameArgs, MocapRawComponent, MocapRawFlags};
    use synapse_fbs::types::Vec3f;

    fn mocap_frame_bytes() -> Vec<u8> {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        let flags = (MocapRawFlags::Valid | MocapRawFlags::ResidualValid).bits();
        let body = MocapRigidBodyData::new(
            &Vec3f::new(1.5, -2.25, 0.75),
            &RotationMatrix3f::new(1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0),
            0.001,
            1,
            flags,
            MocapRawComponent::RigidBody6d,
        );
        let bodies = builder.create_vector(&[body]);
        let frame = MocapFrame::create(
            &mut builder,
            &MocapFrameArgs {
                timestamp_us: 123_456,
                frame_number: 42,
                rigid_bodies: Some(bodies),
                ..Default::default()
            },
        );
        builder.finish(frame, None);
        builder.finished_data().to_vec()
    }

    /// The compact pose payload must match synapse_qualisys_bridge byte for
    /// byte: 7 little-endian f32 `[px, py, pz, qx, qy, qz, qw]` — w LAST.
    #[test]
    fn compact_pose_matches_qualisys_bridge_wire_layout() {
        let bytes = mocap_frame_bytes();
        let frame = flatbuffers::root::<MocapFrame>(&bytes).expect("frame parses");
        let body = frame.rigid_bodies().unwrap().iter().next().unwrap();

        let payload = compact_pose_payload(body);
        assert_eq!(payload.len(), 28);
        let f32_at =
            |offset: usize| f32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap());
        assert_eq!(f32_at(0), 1.5); // px
        assert_eq!(f32_at(4), -2.25); // py
        assert_eq!(f32_at(8), 0.75); // pz
        assert_eq!(f32_at(12), 0.0); // qx
        assert_eq!(f32_at(16), 0.0); // qy
        assert_eq!(f32_at(20), 0.0); // qz
        assert_eq!(f32_at(24), 1.0); // qw — scalar is last on the wire
    }

    #[test]
    fn compact_pose_payload_is_never_republished_as_a_frame() {
        // The bridge only republishes verified MocapFrame FlatBuffers carrying
        // at least one rigid body. A compact 28-byte pose must fail that gate:
        // either the verifier rejects it outright, or (for degenerate byte
        // patterns that happen to verify as an empty table) it has no rigid
        // bodies and is dropped.
        let bytes = mocap_frame_bytes();
        let frame = flatbuffers::root::<MocapFrame>(&bytes).unwrap();
        let body = frame.rigid_bodies().unwrap().iter().next().unwrap();
        let compact = compact_pose_payload(body);

        let republishable = flatbuffers::root::<MocapFrame>(&compact)
            .ok()
            .and_then(|frame| frame.rigid_bodies())
            .is_some_and(|bodies| !bodies.is_empty());
        assert!(!republishable);

        let zeroed = [0u8; 28];
        let republishable = flatbuffers::root::<MocapFrame>(&zeroed)
            .ok()
            .and_then(|frame| frame.rigid_bodies())
            .is_some_and(|bodies| !bodies.is_empty());
        assert!(!republishable);
    }
}

//! Minimal native Zenoh client that connects to an endpoint (default a WebSocket
//! locator) and prints a few `synapse/**` samples. Used to isolate whether a
//! Zenoh WS listener accepts client sessions independent of the browser wasm.
//!
//!   cargo run -p electrode-fake-sim --bin wsprobe -- ws/127.0.0.1:7447 synapse/** 5
//!   cargo run -p electrode-fake-sim --bin wsprobe -- peer synapse/** 5
use synapse_fbs::topic::{
    AttitudeEstimateData, AttitudeEstimateFlags, ManualControlData, ManualControlFlags,
    PwmSignalOutputsData, RadioControlData, VehicleHealthData, VehicleHealthFlags,
};
use zenoh::Wait;

fn main() -> anyhow::Result<()> {
    zenoh::init_log_from_env_or("info");
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "ws/127.0.0.1:7447".to_string());
    let key_expr = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "synapse/**".to_string());
    let sample_count = std::env::args()
        .nth(3)
        .as_deref()
        .unwrap_or("5")
        .parse::<usize>()
        .unwrap_or(5);

    let mut config = zenoh::Config::default();
    let set = |config: &mut zenoh::Config, key: &str, value: &str| {
        config
            .insert_json5(key, value)
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    };
    if endpoint == "peer" {
        set(&mut config, "mode", "\"peer\"")?;
    } else {
        set(&mut config, "mode", "\"client\"")?;
        set(
            &mut config,
            "connect/endpoints",
            &format!("[\"{endpoint}\"]"),
        )?;
        set(&mut config, "scouting/multicast/enabled", "false")?;
    }

    println!("wsprobe: opening {endpoint} session ...");
    let session = zenoh::open(config)
        .wait()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    println!("wsprobe: CONNECTED, zid={}", session.zid());

    let subscriber = session
        .declare_subscriber(key_expr.clone())
        .wait()
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    println!("wsprobe: subscribed {key_expr}, waiting for {sample_count} samples ...");

    for _ in 0..sample_count {
        match subscriber.recv() {
            Ok(sample) => {
                let bytes = sample.payload().to_bytes();
                println!(
                    "wsprobe: SAMPLE key={} bytes={} {}",
                    sample.key_expr(),
                    bytes.len(),
                    decode_sample(&sample.key_expr().to_string(), &bytes)
                );
            }
            Err(err) => {
                println!("wsprobe: recv error: {err}");
                break;
            }
        }
    }
    println!("wsprobe: done");
    Ok(())
}

fn hex_preview(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(32)
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

fn decode_sample(key: &str, bytes: &[u8]) -> String {
    if key.ends_with("manual_control_command") && bytes.len() == 40 {
        let data = unsafe { <ManualControlData as flatbuffers::Follow>::follow(bytes, 0) };
        let flags = ManualControlFlags::from_bits_retain(data.flags());
        return format!(
            "manual roll={:+.3} pitch={:+.3} yaw={:+.3} throttle={:.3} mode={} active={} arm={} kill={} valid={} timestamp_us={}",
            f32::from(data.roll_milli()) / 1000.0,
            f32::from(data.pitch_milli()) / 1000.0,
            f32::from(data.yaw_milli()) / 1000.0,
            f32::from(data.throttle_milli()) / 1000.0,
            data.flight_mode(),
            flags.contains(ManualControlFlags::Active),
            flags.contains(ManualControlFlags::ArmSwitch),
            flags.contains(ManualControlFlags::KillSwitch),
            flags.contains(ManualControlFlags::Valid),
            data.timestamp_us()
        );
    }
    if key.ends_with("vehicle_health") && bytes.len() == 48 {
        let data = unsafe { <VehicleHealthData as flatbuffers::Follow>::follow(bytes, 0) };
        let flags = VehicleHealthFlags::from_bits_retain(data.flags());
        return format!(
            "health flight_mode={} armed={} failsafe={} link={} load={} timestamp_us={}",
            data.flight_mode(),
            flags.contains(VehicleHealthFlags::Armed),
            flags.contains(VehicleHealthFlags::Failsafe),
            data.link_quality_pct(),
            f32::from(data.load_dpermille()) / 10.0,
            data.timestamp_us()
        );
    }
    if key.ends_with("attitude_estimate") && bytes.len() == 40 {
        let data = unsafe { <AttitudeEstimateData as flatbuffers::Follow>::follow(bytes, 0) };
        let q = data.attitude();
        let flags = AttitudeEstimateFlags::from_bits_retain(data.flags());
        let (roll, pitch, yaw) = euler_deg(q.w(), q.x(), q.y(), q.z());
        return format!(
            "attitude q=[{:.4},{:.4},{:.4},{:.4}] rpy_deg=[{:.1},{:.1},{:.1}] valid={} timestamp_us={}",
            q.w(),
            q.x(),
            q.y(),
            q.z(),
            roll,
            pitch,
            yaw,
            flags.contains(AttitudeEstimateFlags::AttitudeValid),
            data.timestamp_us()
        );
    }
    if (key.ends_with("pwm_signal_outputs") || key.ends_with("motor_output")) && bytes.len() == 48 {
        let data = unsafe { <PwmSignalOutputsData as flatbuffers::Follow>::follow(bytes, 0) };
        return format!(
            "pwm out0={} out1={} out2={} out3={} out4={} out5={} mask={} timestamp_us={}",
            data.output0_us(),
            data.output1_us(),
            data.output2_us(),
            data.output3_us(),
            data.output4_us(),
            data.output5_us(),
            data.active_mask(),
            data.timestamp_us()
        );
    }
    if key.contains("/mocap/rigid_body/") && bytes.len() == 28 {
        let value = |index: usize| -> f32 {
            f32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap_or([0; 4]))
        };
        return format!(
            "compact_pose x={:.3} y={:.3} z={:.3} raw_q=[{:.4},{:.4},{:.4},{:.4}]",
            value(0),
            value(1),
            value(2),
            value(3),
            value(4),
            value(5),
            value(6)
        );
    }
    if key.ends_with("radio_control") && bytes.len() == 48 {
        let data = unsafe { <RadioControlData as flatbuffers::Follow>::follow(bytes, 0) };
        return format!(
            "radio ch1={} ch2={} ch3={} ch4={} ch5={} count={} link={} timestamp_us={}",
            data.chan0_raw_us(),
            data.chan1_raw_us(),
            data.chan2_raw_us(),
            data.chan3_raw_us(),
            data.chan4_raw_us(),
            data.channel_count(),
            data.link_quality_pct(),
            data.timestamp_us()
        );
    }
    format!("hex={}", hex_preview(bytes))
}

fn euler_deg(qw: f32, qx: f32, qy: f32, qz: f32) -> (f32, f32, f32) {
    let sinr_cosp = 2.0 * ((qw * qx) + (qy * qz));
    let cosr_cosp = 1.0 - (2.0 * ((qx * qx) + (qy * qy)));
    let sinp = 2.0 * ((qw * qy) - (qz * qx));
    let siny_cosp = 2.0 * ((qw * qz) + (qx * qy));
    let cosy_cosp = 1.0 - (2.0 * ((qy * qy) + (qz * qz)));
    (
        sinr_cosp.atan2(cosr_cosp).to_degrees(),
        sinp.clamp(-1.0, 1.0).asin().to_degrees(),
        siny_cosp.atan2(cosy_cosp).to_degrees(),
    )
}

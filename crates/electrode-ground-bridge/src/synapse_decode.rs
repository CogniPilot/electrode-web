//! Best-effort decoding of Synapse FlatBuffer payloads observed on Zenoh.
//!
//! Real Cerebri/Synapse vehicles (and the SIL) publish Synapse messages on
//! `synapse/v1/topic/**` key expressions. We cannot infer the message type
//! from the raw bytes reliably, so we classify by the Zenoh key and decode the
//! topics we understand. Anything we do not recognize is still surfaced to the
//! operator as a raw payload so it remains discoverable.
//!
//! Wire encoding (synapse_fbs 0.5.0): every fixed-layout topic is transmitted
//! as the *bare* `*Data` struct (raw fixed-size bytes), not a FlatBuffers root
//! table. We decode those exactly like `synapse_fbs::topic_decode::decode_struct`
//! (size-check then `Follow::follow`). Only `mocap_frame` is a root table.

use flatbuffers::root;
use serde_json::{Value, json};
use synapse_fbs::topic::{
    AttitudeCommandData, AttitudeEstimateData, AttitudeEstimateFlags, ControlLoopMetricsData,
    ManualControlData, ManualControlFlags, MocapFrame, MocapRawFlags, NavigationTargetData,
    PowerStatusData, PwmSignalOutputsData, RadioControlData, VehicleHealthData, VehicleHealthFlags,
};
use synapse_fbs::types::RotationMatrix3f;

/// A payload decoded (or passed through) from a Zenoh sample.
pub(crate) struct Decoded {
    /// Human-facing message type, e.g. `AttitudeEstimate` or `Raw`.
    pub schema: &'static str,
    /// JSON payload forwarded to the browser.
    pub payload: Value,
}

/// Classify a Zenoh key into the Synapse schema we expect on it.
pub(crate) fn classify(key: &str) -> &'static str {
    // Resolve the canonical topic through the catalog when the key is on the
    // `synapse/v1/topic/<suffix>` scheme (handles namespaces and instances).
    if let Some(topic) = synapse_fbs::topic_catalog::topic_by_key(key) {
        if let Some(schema) = schema_for_suffix(topic.key_suffix) {
            return schema;
        }
    }
    // Fall back to the trailing path segment for legacy bare keys.
    let leaf = key.rsplit('/').next().unwrap_or(key);
    if let Some(schema) = schema_for_suffix(leaf) {
        return schema;
    }
    if key.contains("mocap") {
        "MocapFrame"
    } else {
        "Raw"
    }
}

/// Map a topic key suffix to the schema name we decode it as. Optical flow and
/// everything else fall through to the raw passthrough.
fn schema_for_suffix(suffix: &str) -> Option<&'static str> {
    Some(match suffix {
        "mocap_frame" => "MocapFrame",
        "manual_control_command" => "ManualControl",
        "radio_control" => "RadioControl",
        "pwm_signal_outputs" => "PwmSignalOutputs",
        "attitude_estimate" => "AttitudeEstimate",
        "attitude_command" => "AttitudeCommand",
        "navigation_target" => "NavigationTarget",
        "control_loop_metrics" => "ControlLoopMetrics",
        "vehicle_health" => "VehicleHealth",
        "power_status" => "PowerStatus",
        _ => return None,
    })
}

/// Decode a Zenoh sample by key, falling back to a raw preview.
pub(crate) fn decode(key: &str, bytes: &[u8]) -> Decoded {
    match classify(key) {
        "MocapFrame" => decode_or_raw("MocapFrame", bytes, decode_mocap_frame),
        "ManualControl" => decode_or_raw("ManualControl", bytes, decode_manual_control),
        "RadioControl" => decode_or_raw("RadioControl", bytes, decode_radio_control),
        "PwmSignalOutputs" => decode_or_raw("PwmSignalOutputs", bytes, decode_pwm_signal_outputs),
        "AttitudeEstimate" => decode_or_raw("AttitudeEstimate", bytes, decode_attitude_estimate),
        "AttitudeCommand" => decode_or_raw("AttitudeCommand", bytes, decode_attitude_command),
        "NavigationTarget" => decode_or_raw("NavigationTarget", bytes, decode_navigation_target),
        "ControlLoopMetrics" => {
            decode_or_raw("ControlLoopMetrics", bytes, decode_control_loop_metrics)
        }
        "VehicleHealth" => decode_or_raw("VehicleHealth", bytes, decode_vehicle_health),
        "PowerStatus" => decode_or_raw("PowerStatus", bytes, decode_power_status),
        schema => Decoded {
            schema,
            payload: raw_payload(bytes),
        },
    }
}

fn decode_or_raw(
    schema: &'static str,
    bytes: &[u8],
    decoder: fn(&[u8]) -> Option<Value>,
) -> Decoded {
    match decoder(bytes) {
        Some(payload) => Decoded { schema, payload },
        None => Decoded {
            schema,
            payload: raw_payload(bytes),
        },
    }
}

fn raw_payload(bytes: &[u8]) -> Value {
    let preview: String = bytes
        .iter()
        .take(32)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join("");
    json!({ "bytes": bytes.len(), "hexPreview": preview })
}

/// Decode a bare fixed-layout struct topic. Mirrors
/// `synapse_fbs::topic_decode::decode_struct`: verify the payload is exactly
/// the catalog struct size, then follow it at offset 0.
fn decode_struct<'a, T>(payload: &'a [u8], expected: usize) -> Option<T::Inner>
where
    T: flatbuffers::Follow<'a>,
{
    if payload.len() != expected {
        return None;
    }
    // Safety: generated fixed-layout structs are repr(transparent) byte arrays
    // with unaligned accessors, and the exact-size check above guarantees the
    // buffer covers the struct.
    Some(unsafe { T::follow(payload, 0) })
}

fn decode_attitude_command(bytes: &[u8]) -> Option<Value> {
    let data = decode_struct::<AttitudeCommandData>(bytes, 48)?;
    let attitude = data.attitude();
    let rates = data.body_rate_flu_rad_s();
    Some(json!({
        "data": {
            "timestamp_us": data.timestamp_us(),
            "attitude": {
                "w": attitude.w(),
                "x": attitude.x(),
                "y": attitude.y(),
                "z": attitude.z()
            },
            "body_rate_flu_rad_s": {
                "roll": rates.roll(),
                "pitch": rates.pitch(),
                "yaw": rates.yaw()
            },
            "thrust": data.thrust(),
            "type_mask": data.type_mask()
        }
    }))
}

fn decode_navigation_target(bytes: &[u8]) -> Option<Value> {
    let data = decode_struct::<NavigationTargetData>(bytes, 32)?;
    Some(json!({
        "data": {
            "timestamp_us": data.timestamp_us(),
            "altitude_error_m": data.altitude_error_m(),
            "airspeed_error_m_s": data.airspeed_error_m_s(),
            "xtrack_error_m": data.xtrack_error_m(),
            "desired_roll_deg": f64::from(data.desired_roll_cdeg()) / 100.0,
            "desired_pitch_deg": f64::from(data.desired_pitch_cdeg()) / 100.0,
            "desired_yaw_deg": f64::from(data.desired_yaw_cdeg()) / 100.0,
            "target_yaw_deg": f64::from(data.target_yaw_cdeg()) / 100.0,
            "distance_to_waypoint_m": data.distance_to_waypoint_m()
        }
    }))
}

fn decode_control_loop_metrics(bytes: &[u8]) -> Option<Value> {
    let data = decode_struct::<ControlLoopMetricsData>(bytes, 24)?;
    Some(json!({
        "data": {
            "timestamp_us": data.timestamp_us(),
            "period_us": data.period_us(),
            "latency_us": data.latency_us(),
            "overrun_count": data.overrun_count(),
            "load_pct": f64::from(data.load_dpermille()) / 10.0
        }
    }))
}

fn decode_attitude_estimate(bytes: &[u8]) -> Option<Value> {
    let data = decode_struct::<AttitudeEstimateData>(bytes, 40)?;
    let attitude = data.attitude();
    let rates = data.angular_velocity_flu_rad_s();
    let flags = AttitudeEstimateFlags::from_bits_retain(data.flags());
    Some(json!({
        "data": {
            "timestamp_us": data.timestamp_us(),
            "attitude": {
                "w": attitude.w(),
                "x": attitude.x(),
                "y": attitude.y(),
                "z": attitude.z()
            },
            "angular_velocity": {
                "roll": rates.roll(),
                "pitch": rates.pitch(),
                "yaw": rates.yaw()
            },
            "attitude_valid": flags.contains(AttitudeEstimateFlags::AttitudeValid),
            "rates_valid": flags.contains(AttitudeEstimateFlags::RatesValid)
        }
    }))
}

fn decode_vehicle_health(bytes: &[u8]) -> Option<Value> {
    let data = decode_struct::<VehicleHealthData>(bytes, 48)?;
    let flags = VehicleHealthFlags::from_bits_retain(data.flags());
    Some(json!({
        "data": {
            "timestamp_us": data.timestamp_us(),
            "flight_mode": data.flight_mode(),
            "link_quality_pct": data.link_quality_pct(),
            "voltage_battery_v": f64::from(data.voltage_battery_cv()) / 100.0,
            "current_battery_a": f64::from(data.current_battery_da()) / 10.0,
            "battery_remaining_pct": data.battery_remaining_pct(),
            "armed": flags.contains(VehicleHealthFlags::Armed),
            "failsafe": flags.contains(VehicleHealthFlags::Failsafe),
            "system_state": data.system_state(),
            "load_pct": f64::from(data.load_dpermille()) / 10.0
        }
    }))
}

fn decode_power_status(bytes: &[u8]) -> Option<Value> {
    let data = decode_struct::<PowerStatusData>(bytes, 64)?;
    let voltages = data.voltages();
    let cells_mv: Vec<u16> = vec![
        voltages.cell0_mv(),
        voltages.cell1_mv(),
        voltages.cell2_mv(),
        voltages.cell3_mv(),
        voltages.cell4_mv(),
        voltages.cell5_mv(),
        voltages.cell6_mv(),
        voltages.cell7_mv(),
        voltages.cell8_mv(),
        voltages.cell9_mv(),
        voltages.cell10_mv(),
        voltages.cell11_mv(),
        voltages.cell12_mv(),
        voltages.cell13_mv(),
        voltages.cell14_mv(),
        voltages.cell15_mv(),
    ];
    // Pack voltage is the sum of populated (non-zero) cells, millivolts to volts.
    let pack_mv: u32 = cells_mv.iter().map(|&cell| u32::from(cell)).sum();
    Some(json!({
        "data": {
            "timestamp_us": data.timestamp_us(),
            "voltage_v": f64::from(pack_mv) / 1000.0,
            "current_a": f64::from(data.current_battery_da()) / 10.0,
            "remaining_pct": data.remaining_pct(),
            "connected": data.connected(),
            "cells_mv": cells_mv,
            "temperature_c": f64::from(data.temperature_cdeg()) / 100.0
        }
    }))
}

fn decode_manual_control(bytes: &[u8]) -> Option<Value> {
    let data = decode_struct::<ManualControlData>(bytes, 40)?;
    let flags = ManualControlFlags::from_bits_retain(data.flags());
    // Axes and aux channels are scaled shorts in -1000..1000 => normalized /1000.
    let milli = |value: i16| f64::from(value) / 1000.0;
    Some(json!({
        "data": {
            "timestamp_us": data.timestamp_us(),
            "axes": {
                "roll": milli(data.roll_milli()),
                "pitch": milli(data.pitch_milli()),
                "yaw": milli(data.yaw_milli()),
                "throttle": milli(data.throttle_milli())
            },
            "aux": [
                milli(data.aux0_milli()),
                milli(data.aux1_milli()),
                milli(data.aux2_milli()),
                milli(data.aux3_milli()),
                milli(data.aux4_milli()),
                milli(data.aux5_milli())
            ],
            "flight_mode": data.flight_mode(),
            "arm_switch": flags.contains(ManualControlFlags::ArmSwitch),
            "kill_switch": flags.contains(ManualControlFlags::KillSwitch),
            "active": flags.contains(ManualControlFlags::Active),
            "valid": flags.contains(ManualControlFlags::Valid),
            "buttons": data.buttons()
        }
    }))
}

fn decode_radio_control(bytes: &[u8]) -> Option<Value> {
    let data = decode_struct::<RadioControlData>(bytes, 48)?;
    let channels: Vec<u16> = vec![
        data.chan0_raw_us(),
        data.chan1_raw_us(),
        data.chan2_raw_us(),
        data.chan3_raw_us(),
        data.chan4_raw_us(),
        data.chan5_raw_us(),
        data.chan6_raw_us(),
        data.chan7_raw_us(),
        data.chan8_raw_us(),
        data.chan9_raw_us(),
        data.chan10_raw_us(),
        data.chan11_raw_us(),
        data.chan12_raw_us(),
        data.chan13_raw_us(),
        data.chan14_raw_us(),
        data.chan15_raw_us(),
        data.chan16_raw_us(),
        data.chan17_raw_us(),
    ];
    Some(json!({
        "data": {
            "timestamp_us": data.timestamp_us(),
            "channel_count": data.channel_count(),
            "link_quality_pct": data.link_quality_pct()
        },
        "channels": {
            "ch0": channels[0],
            "ch1": channels[1],
            "ch2": channels[2],
            "ch3": channels[3],
            "ch4": channels[4],
            "ch5": channels[5],
            "ch6": channels[6],
            "ch7": channels[7],
            "ch8": channels[8],
            "ch9": channels[9],
            "ch10": channels[10],
            "ch11": channels[11],
            "ch12": channels[12],
            "ch13": channels[13],
            "ch14": channels[14],
            "ch15": channels[15],
            "ch16": channels[16],
            "ch17": channels[17]
        }
    }))
}

fn decode_pwm_signal_outputs(bytes: &[u8]) -> Option<Value> {
    let data = decode_struct::<PwmSignalOutputsData>(bytes, 48)?;
    let outputs_us: Vec<u16> = vec![
        data.output0_us(),
        data.output1_us(),
        data.output2_us(),
        data.output3_us(),
        data.output4_us(),
        data.output5_us(),
        data.output6_us(),
        data.output7_us(),
        data.output8_us(),
        data.output9_us(),
        data.output10_us(),
        data.output11_us(),
        data.output12_us(),
        data.output13_us(),
        data.output14_us(),
        data.output15_us(),
    ];
    Some(json!({
        "data": {
            "timestamp_us": data.timestamp_us(),
            "active_mask": data.active_mask(),
            "port": data.port(),
            "outputs_us": outputs_us
        },
        // First four outputs kept as a motors object so state-store's
        // parseMotorOutputs stays simple.
        "motors": {
            "m0": outputs_us[0],
            "m1": outputs_us[1],
            "m2": outputs_us[2],
            "m3": outputs_us[3]
        }
    }))
}

fn decode_mocap_frame(bytes: &[u8]) -> Option<Value> {
    let frame = root::<MocapFrame>(bytes).ok()?;
    // Report every tracked rigid body's pose (position + attitude), matching
    // the wire schema's `rigid_bodies` vector so downstream consumers can index
    // `rigid_bodies[0]`.
    let mut rigid_bodies: Vec<Value> = Vec::new();
    if let Some(bodies) = frame.rigid_bodies() {
        for body in bodies.iter() {
            let p = body.position_enu_m();
            let q = rotation_matrix_to_quaternion(body.rotation());
            let flags = MocapRawFlags::from_bits_retain(body.flags());
            rigid_bodies.push(json!({
                "id": body.id(),
                "position": { "x": p.x(), "y": p.y(), "z": p.z() },
                "attitude": { "x": q.0, "y": q.1, "z": q.2, "w": q.3 },
                "residual": body.residual_mm(),
                "tracking_valid": flags.contains(MocapRawFlags::Valid)
            }));
        }
    }
    let mut labeled_markers: Vec<Value> = Vec::new();
    if let Some(markers) = frame.markers() {
        for marker in markers.iter() {
            let p = marker.position_enu_m();
            labeled_markers.push(json!({
                "id": marker.id(),
                "position": { "x": p.x(), "y": p.y(), "z": p.z() },
                "residual": marker.residual_mm()
            }));
        }
    }
    Some(json!({
        "timestamp_us": frame.timestamp_us(),
        "frame_number": frame.frame_number(),
        "rigid_bodies": rigid_bodies,
        "labeled_markers": labeled_markers
    }))
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

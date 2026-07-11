//! Fake flying vehicle for testing electrode without real hardware or a sim.
//!
//! Models the real network shape as two independent publishers:
//!
//! * `mocap` - pose only, like CogniPilot/synapse_qualisys_bridge:
//!   a MocapFrame (position + attitude quaternion) on
//!   `mocap`.
//! * `autopilot` - standard vehicle telemetry the flight controller emits:
//!   AttitudeEstimate on `att` and
//!   PwmSignalOutputs on `pwm`.
//!
//! Run them together (default) or as two separate processes to mirror the two
//! machines on a real setup:
//!
//!   cargo run -p electrode-fake-sim -- --role mocap
//!   cargo run -p electrode-fake-sim -- --role autopilot
//!   npm run bridge          # electrode-web, another terminal
//!
//! Both roles fly the SAME deterministic coordinated-turn loiter from t=0, so
//! two processes stay in sync without sharing state.

use std::{
    f32::consts::PI,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use flatbuffers::FlatBufferBuilder;
use synapse_fbs::topic::{
    AttitudeEstimateData, AttitudeEstimateFlags, MocapFrame, MocapFrameArgs, MocapRawComponent,
    MocapRawFlags, MocapRigidBodyData, PwmSignalOutputsData,
};
use synapse_fbs::types::{Quaternionf, RateTriplet, RotationMatrix3f, Vec3f};
use zenoh::{config::Config, pubsub::Publisher, Session, Wait};

const G: f32 = 9.81;

#[derive(Debug, Parser)]
#[command(
    name = "electrode-fake-sim",
    version,
    about = "Publish fake flying Synapse telemetry over Zenoh for electrode testing"
)]
struct Cli {
    #[arg(
        long,
        default_value = "both",
        value_parser = ["mocap", "autopilot", "both"],
        help = "Which publisher(s) to run: mocap (pose only), autopilot (telemetry), or both"
    )]
    role: String,

    #[arg(
        long,
        default_value = "udp/127.0.0.1:7447",
        help = "Zenoh endpoint. In peer mode this is the (network) listen locator; in client mode the router to connect to."
    )]
    endpoint: String,

    #[arg(
        long,
        default_value = "ws/127.0.0.1:7447",
        help = "Extra WebSocket listen locator for browser (zenoh-wasm) clients in peer mode. Empty to disable."
    )]
    ws_endpoint: String,

    #[arg(
        long,
        default_value = "peer",
        value_parser = ["peer", "client", "router"],
        help = "peer/router: listen on --endpoint (+ --ws-endpoint); client: connect to a router at --endpoint. Use router for browser (zenoh-wasm) clients."
    )]
    mode: String,

    #[arg(
        long,
        default_value = "",
        help = "Optional deployment namespace prepended to the compact catalog keys (empty = bare keys, matching csyn firmware)"
    )]
    prefix: String,

    #[arg(
        long,
        default_value_t = 240.0,
        help = "Mocap (pose) publish rate in Hz"
    )]
    mocap_hz: f32,

    #[arg(
        long,
        default_value_t = 50.0,
        help = "Autopilot telemetry publish rate in Hz"
    )]
    autopilot_hz: f32,

    #[arg(
        long,
        default_value_t = 18.0,
        help = "Cruise airspeed in m/s (drives the turn geometry)"
    )]
    airspeed: f32,

    #[arg(
        long,
        default_value_t = 18.0,
        help = "Bank angle for the loiter turn, degrees"
    )]
    bank_deg: f32,
}

/// Rigid-body flight state, integrated each tick.
struct FlightState {
    /// Heading / yaw, radians, wraps 0..2π.
    psi: f32,
    /// East, North, Up position in metres.
    east: f32,
    north: f32,
    up: f32,
    /// Seconds since takeoff.
    t: f32,
}

#[derive(Debug, Clone, Copy)]
struct ActiveStreams {
    mocap: bool,
    autopilot: bool,
}

struct SimPublishers<'a> {
    mocap: Option<Publisher<'a>>,
    attitude: Option<Publisher<'a>>,
    motor: Option<Publisher<'a>>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // Route Zenoh's tracing output through RUST_LOG (e.g. RUST_LOG=zenoh=debug).
    zenoh::init_log_from_env_or("error");
    let session = open_verified(&cli)?;
    let streams = active_streams(&cli);
    let publishers = declare_publishers(&session, &cli, streams)?;

    print_startup(&cli, streams);
    run_publish_loop(&cli, streams, publishers)
}

fn active_streams(cli: &Cli) -> ActiveStreams {
    ActiveStreams {
        mocap: cli.role == "mocap" || cli.role == "both",
        autopilot: cli.role == "autopilot" || cli.role == "both",
    }
}

fn declare_publishers<'a>(
    session: &'a Session,
    cli: &Cli,
    streams: ActiveStreams,
) -> anyhow::Result<SimPublishers<'a>> {
    Ok(SimPublishers {
        mocap: streams
            .mocap
            .then(|| declare(session, topic_key(cli, "mocap"), "MocapFrame"))
            .transpose()?,
        attitude: streams
            .autopilot
            .then(|| declare(session, topic_key(cli, "att"), "AttitudeEstimate"))
            .transpose()?,
        motor: streams
            .autopilot
            .then(|| declare(session, topic_key(cli, "pwm"), "PwmSignalOutputs"))
            .transpose()?,
    })
}

/// `[<namespace>/]<catalog key>` — bare when no namespace is configured.
fn topic_key(cli: &Cli, key: &str) -> String {
    let namespace = cli.prefix.trim_matches('/');
    if namespace.is_empty() {
        key.to_string()
    } else {
        format!("{namespace}/{key}")
    }
}

fn print_startup(cli: &Cli, streams: ActiveStreams) {
    println!(
        "electrode-fake-sim: role={} mode={} endpoint={}",
        cli.role, cli.mode, cli.endpoint
    );
    if streams.mocap {
        println!(
            "  mocap     → {} (MocapFrame: pose) @ {:.0} Hz",
            topic_key(cli, "mocap"),
            cli.mocap_hz
        );
    }
    if streams.autopilot {
        println!(
            "  autopilot → {} (AttitudeEstimate), {} (PwmSignalOutputs) @ {:.0} Hz",
            topic_key(cli, "att"),
            topic_key(cli, "pwm"),
            cli.autopilot_hz
        );
    }
}

fn run_publish_loop(
    cli: &Cli,
    streams: ActiveStreams,
    publishers: SimPublishers<'_>,
) -> anyhow::Result<()> {
    // Integrate the physics at the fastest active stream's rate; each stream is
    // then published on its own timer so mocap (240 Hz) and autopilot (50 Hz)
    // run at independent, non-integer-ratio rates.
    let mut base_hz = 1.0_f32;
    if streams.mocap {
        base_hz = base_hz.max(cli.mocap_hz);
    }
    if streams.autopilot {
        base_hz = base_hz.max(cli.autopilot_hz);
    }
    let dt = 1.0 / base_hz.max(1.0);
    let period = Duration::from_secs_f32(dt);
    let mocap_interval = 1.0 / cli.mocap_hz.max(1.0);
    let autopilot_interval = 1.0 / cli.autopilot_hz.max(1.0);
    // Seed at the interval so both streams emit on the first tick.
    let mut mocap_accum = mocap_interval;
    let mut autopilot_accum = autopilot_interval;

    let bank = cli.bank_deg.to_radians();
    // Coordinated-turn yaw rate: omega = g*tan(phi)/V.
    let yaw_rate = G * bank.tan() / cli.airspeed.max(1.0);

    let mut state = FlightState {
        psi: 0.0,
        east: 0.0,
        north: 0.0,
        up: 60.0,
        t: 0.0,
    };

    let mut next = Instant::now();
    let mut ticks: u64 = 0;

    loop {
        // Integrate the loiter: constant-speed level turn with gentle altitude bob.
        state.psi = (state.psi + yaw_rate * dt).rem_euclid(2.0 * PI);
        state.east += cli.airspeed * state.psi.sin() * dt;
        state.north += cli.airspeed * state.psi.cos() * dt;
        state.up = 60.0 + (state.t * 0.25).sin() * 5.0;
        state.t += dt;

        let (roll, pitch) = attitude(&state, bank);

        // Mocap pose at its own rate.
        mocap_accum += dt;
        if mocap_accum >= mocap_interval {
            mocap_accum -= mocap_interval;
            if let Some(publisher) = &publishers.mocap {
                put(publisher, encode_mocap_frame(&state, roll, pitch, ticks))?;
            }
        }

        // Autopilot telemetry at its own rate.
        autopilot_accum += dt;
        if autopilot_accum >= autopilot_interval {
            autopilot_accum -= autopilot_interval;
            if let Some(publisher) = &publishers.attitude {
                put(
                    publisher,
                    encode_attitude_estimate(&state, roll, pitch, yaw_rate),
                )?;
            }
            if let Some(publisher) = &publishers.motor {
                put(publisher, encode_pwm_signal_outputs(&state))?;
            }
        }

        ticks += 1;
        if ticks.is_multiple_of(base_hz.max(1.0) as u64) {
            println!(
                "  t={:6.1}s  hdg={:5.1}°  alt={:5.1}m  pos=({:7.1},{:7.1})m  bank={:4.1}°",
                state.t,
                state.psi.to_degrees(),
                state.up,
                state.east,
                state.north,
                bank.to_degrees()
            );
        }

        // Fixed-rate scheduling that tolerates slow ticks without drifting.
        next += period;
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}

/// Trim attitude (roll, pitch) in radians with a little life on top.
fn attitude(state: &FlightState, bank: f32) -> (f32, f32) {
    let roll = bank + (state.t * 2.0).sin() * 0.02;
    let pitch = 0.05 + (state.t * 1.3).sin() * 0.03;
    (roll, pitch)
}

fn encode_mocap_frame(state: &FlightState, roll: f32, pitch: f32, frame_number: u64) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let (qx, qy, qz, qw) = euler_to_quat(roll, pitch, state.psi);
    let rotation = quaternion_to_rotation_matrix(qw, qx, qy, qz);
    let flags =
        (MocapRawFlags::Valid | MocapRawFlags::ResidualValid | MocapRawFlags::LabelValid).bits();
    // Rigid body 0: the tracked vehicle, ENU position in metres.
    let body = MocapRigidBodyData::new(
        &Vec3f::new(state.east, state.north, state.up),
        &rotation,
        0.0005,
        0,
        flags,
        MocapRawComponent::RigidBody6d,
    );
    let bodies = builder.create_vector(&[body]);
    let message = MocapFrame::create(
        &mut builder,
        &MocapFrameArgs {
            timestamp_us: timestamp_us(),
            frame_number: frame_number as u32,
            flags,
            rigid_bodies: Some(bodies),
            ..Default::default()
        },
    );
    builder.finish(message, None);
    builder.finished_data().to_vec()
}

/// AttitudeEstimate: vehicle attitude quaternion plus body angular velocity.
/// Published as a bare fixed-layout struct (raw bytes), not a root table.
fn encode_attitude_estimate(state: &FlightState, roll: f32, pitch: f32, yaw_rate: f32) -> Vec<u8> {
    let t = state.t;
    let (qx, qy, qz, qw) = euler_to_quat(roll, pitch, state.psi);
    let attitude = Quaternionf::new(qw, qx, qy, qz);
    // Body roll/pitch/yaw rates (FLU, rad/s); yaw tracks the coordinated turn.
    let angular_velocity =
        RateTriplet::new((t * 3.0).sin() * 0.03, (t * 2.2).cos() * 0.03, yaw_rate);
    let flags = (AttitudeEstimateFlags::AttitudeValid | AttitudeEstimateFlags::RatesValid).bits();
    let data = AttitudeEstimateData::new(timestamp_us(), &attitude, &angular_velocity, flags);
    data.0.to_vec()
}

/// PwmSignalOutputs: raw PWM pulse widths. Output 0 tracks a gentle throttle
/// bob; the rest idle at 1000 us. Published as a bare fixed-layout struct.
fn encode_pwm_signal_outputs(state: &FlightState) -> Vec<u8> {
    let throttle = 0.55 + (state.t * 0.3).sin() * 0.05;
    let thr_us = (1000.0 + throttle * 1000.0) as u16;
    // Outputs 0..3 active (mask bits 0..3); the vehicle is armed and flying.
    let data = PwmSignalOutputsData::new(
        timestamp_us(), // timestamp_us
        0b1111,         // active_mask
        0,              // port
        thr_us,         // output0_us
        1000,           // output1_us
        1000,           // output2_us
        1000,           // output3_us
        1000,           // output4_us
        1000,           // output5_us
        1000,           // output6_us
        1000,           // output7_us
        1000,           // output8_us
        1000,           // output9_us
        1000,           // output10_us
        1000,           // output11_us
        1000,           // output12_us
        1000,           // output13_us
        1000,           // output14_us
        1000,           // output15_us
    );
    data.0.to_vec()
}

/// ZYX (yaw-pitch-roll) Euler angles to a {x, y, z, w} quaternion.
fn euler_to_quat(roll: f32, pitch: f32, yaw: f32) -> (f32, f32, f32, f32) {
    let (sr, cr) = (roll * 0.5).sin_cos();
    let (sp, cp) = (pitch * 0.5).sin_cos();
    let (sy, cy) = (yaw * 0.5).sin_cos();
    (
        sr * cp * cy - cr * sp * sy,
        cr * sp * cy + sr * cp * sy,
        cr * cp * sy - sr * sp * cy,
        cr * cp * cy + sr * sp * sy,
    )
}

fn quaternion_to_rotation_matrix(qw: f32, qx: f32, qy: f32, qz: f32) -> RotationMatrix3f {
    let norm = (qw.mul_add(qw, qx.mul_add(qx, qy.mul_add(qy, qz * qz)))).sqrt();
    let scale = if norm.is_finite() && norm > 0.0 {
        1.0 / norm
    } else {
        1.0
    };
    let (w, x, y, z) = (qw * scale, qx * scale, qy * scale, qz * scale);
    RotationMatrix3f::new(
        1.0 - (2.0 * ((y * y) + (z * z))),
        2.0 * ((x * y) - (z * w)),
        2.0 * ((x * z) + (y * w)),
        2.0 * ((x * y) + (z * w)),
        1.0 - (2.0 * ((x * x) + (z * z))),
        2.0 * ((y * z) - (x * w)),
        2.0 * ((x * z) - (y * w)),
        2.0 * ((y * z) + (x * w)),
        1.0 - (2.0 * ((x * x) + (y * y))),
    )
}

fn declare<'a>(
    session: &'a Session,
    key: String,
    topic_name: &str,
) -> anyhow::Result<Publisher<'a>> {
    // Mandatory 0.6.0 value contract, stamped once at declare time.
    let encoding = synapse_fbs::topic_catalog::topic_by_name(topic_name)
        .map(synapse_fbs::value_contract::encoding_for_topic)
        .map(|encoding| zenoh::bytes::Encoding::from(encoding.as_str()))
        .unwrap_or_default();
    session
        .declare_publisher(key)
        .encoding(encoding)
        .wait()
        .map_err(|e| anyhow::anyhow!(e.to_string()))
}

fn put(publisher: &Publisher<'_>, bytes: Vec<u8>) -> anyhow::Result<()> {
    publisher
        .put(bytes)
        .wait()
        .map_err(|e| anyhow::anyhow!(e.to_string()))
}

/// Open a Zenoh session and, in a listening mode, verify the OS actually bound
/// every expected listener (Zenoh logs success even when a bind silently fails,
/// e.g. on rapid restarts / port reuse). Retries a few times, then fails loudly.
fn open_verified(cli: &Cli) -> anyhow::Result<zenoh::Session> {
    let expected: Vec<String> = if cli.mode == "client" {
        Vec::new()
    } else {
        [cli.endpoint.clone(), cli.ws_endpoint.clone()]
            .into_iter()
            .filter(|locator| !locator.trim().is_empty())
            .collect()
    };

    let mut attempt = 0;
    loop {
        attempt += 1;
        let session = zenoh::open(zenoh_config(cli)?)
            .wait()
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        if expected.is_empty() {
            return Ok(session);
        }
        // Let the listeners finish binding, then cross-check the kernel.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let unbound = electrode_web_core::unbound_listeners(&expected);
        if unbound.is_empty() {
            return Ok(session);
        }
        if attempt >= 5 {
            anyhow::bail!(
                "Zenoh listener(s) failed to bind after {attempt} attempts: {unbound:?}. \
                 Another process may hold the port(s) — check `ss -lnp | grep 7447`."
            );
        }
        eprintln!(
            "electrode-fake-sim: listener(s) not bound (attempt {attempt}): {unbound:?}; \
             closing and retrying"
        );
        let _ = session.close().wait();
        std::thread::sleep(std::time::Duration::from_millis(400 * attempt));
    }
}

fn zenoh_config(cli: &Cli) -> anyhow::Result<Config> {
    let mut config = Config::default();
    let set = |config: &mut Config, key: &str, value: &str| -> anyhow::Result<()> {
        config
            .insert_json5(key, value)
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    };
    set(&mut config, "mode", &format!("\"{}\"", cli.mode))?;
    if cli.mode == "client" {
        set(
            &mut config,
            "connect/endpoints",
            &format!("[\"{}\"]", cli.endpoint),
        )?;
    } else {
        // Listen on the network locator (e.g. UDP) plus, unless disabled, a
        // WebSocket locator so browser zenoh-wasm clients can connect directly.
        let mut endpoints = vec![cli.endpoint.clone()];
        let ws = cli.ws_endpoint.trim();
        if !ws.is_empty() && ws != cli.endpoint {
            endpoints.push(ws.to_string());
        }
        let list = endpoints
            .iter()
            .map(|endpoint| format!("\"{endpoint}\""))
            .collect::<Vec<_>>()
            .join(",");
        set(&mut config, "listen/endpoints", &format!("[{list}]"))?;
    }
    set(&mut config, "scouting/multicast/enabled", "false")?;
    Ok(config)
}

fn timestamp_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

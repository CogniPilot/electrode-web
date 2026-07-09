use clap::Parser;
use synapse_fbs::topic::{ManualControlData, ManualControlFlags};
use thiserror::Error;

/// Wire size of a bare `synapse.topic.ManualControlData` struct.
const MANUAL_CONTROL_PAYLOAD_SIZE: usize = 40;
use zenoh::{config::Config, Wait};

#[derive(Debug, Parser)]
#[command(
    name = "electrode-manual-control-dump",
    version,
    about = "Subscribe to Synapse ManualControl over Zenoh and print decoded fields"
)]
struct Cli {
    #[arg(
        long = "zenoh-connect",
        env = "ZENOH_CONNECT",
        value_name = "LOCATOR",
        default_value = "udp/127.0.0.1:7447",
        help = "Zenoh router locator"
    )]
    zenoh_connect: String,

    #[arg(
        long = "topic",
        alias = "zenoh-topic",
        env = "ZENOH_TOPIC",
        value_name = "KEYEXPR",
        default_value = "synapse/v1/topic/manual_control_command",
        help = "Zenoh key expression for synapse.topic.ManualControlData bare structs"
    )]
    topic: String,
}

#[derive(Debug, Error)]
enum DumpError {
    #[error("zenoh error: {0}")]
    Zenoh(String),
    #[error("manual control payload is {actual} bytes, expected {expected}")]
    PayloadSize { expected: usize, actual: usize },
}

type Result<T> = std::result::Result<T, DumpError>;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let session = zenoh::open(zenoh_config(&cli)?)
        .wait()
        .map_err(|error| DumpError::Zenoh(error.to_string()))?;
    let subscriber = session
        .declare_subscriber(cli.topic)
        .wait()
        .map_err(|error| DumpError::Zenoh(error.to_string()))?;

    loop {
        let sample = subscriber
            .recv()
            .map_err(|error| DumpError::Zenoh(error.to_string()))?;
        let payload = sample.payload().to_bytes();
        // synapse_fbs transmits ManualControlData as a bare fixed-layout struct.
        if payload.len() != MANUAL_CONTROL_PAYLOAD_SIZE {
            return Err(DumpError::PayloadSize {
                expected: MANUAL_CONTROL_PAYLOAD_SIZE,
                actual: payload.len(),
            });
        }
        // Safety: fixed-layout structs are repr(transparent) byte arrays with
        // unaligned accessors, and the size check above covers the struct.
        let data = unsafe { <ManualControlData as flatbuffers::Follow>::follow(&payload, 0) };
        let flags = ManualControlFlags::from_bits_retain(data.flags());
        let milli = |value: i16| f32::from(value) / 1000.0;

        println!(
            "roll={:+.3} pitch={:+.3} yaw={:+.3} throttle={:.3} mode={} arm={} kill={} active={} valid={} timestamp_us={}",
            milli(data.roll_milli()),
            milli(data.pitch_milli()),
            milli(data.yaw_milli()),
            milli(data.throttle_milli()),
            data.flight_mode(),
            flags.contains(ManualControlFlags::ArmSwitch),
            flags.contains(ManualControlFlags::KillSwitch),
            flags.contains(ManualControlFlags::Active),
            flags.contains(ManualControlFlags::Valid),
            data.timestamp_us(),
        );
    }
}

fn zenoh_config(cli: &Cli) -> Result<Config> {
    let mut config = Config::default();
    config
        .insert_json5("mode", "\"client\"")
        .map_err(|error| DumpError::Zenoh(error.to_string()))?;
    config
        .insert_json5("connect/endpoints", &format!("[\"{}\"]", cli.zenoh_connect))
        .map_err(|error| DumpError::Zenoh(error.to_string()))?;
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .map_err(|error| DumpError::Zenoh(error.to_string()))?;
    Ok(config)
}

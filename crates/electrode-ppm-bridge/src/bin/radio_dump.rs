use clap::Parser;
use synapse_fbs::topic::RadioControlData;
use thiserror::Error;
use zenoh::{config::Config, Wait};

/// Wire size of a bare `synapse.topic.RadioControlData` struct.
const RADIO_CONTROL_PAYLOAD_SIZE: usize = 48;

#[derive(Debug, Parser)]
#[command(
    name = "electrode-radio-control-dump",
    version,
    about = "Subscribe to Synapse RadioControl over Zenoh and print decoded PPM wire channels"
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
        default_value = "rc",
        help = "Zenoh key expression for synapse.topic.RadioControlData bare structs"
    )]
    topic: String,
}

#[derive(Debug, Error)]
enum DumpError {
    #[error("zenoh error: {0}")]
    Zenoh(String),
    #[error("radio control payload is {actual} bytes, expected {expected}")]
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
        if payload.len() != RADIO_CONTROL_PAYLOAD_SIZE {
            return Err(DumpError::PayloadSize {
                expected: RADIO_CONTROL_PAYLOAD_SIZE,
                actual: payload.len(),
            });
        }
        let data = unsafe { <RadioControlData as flatbuffers::Follow>::follow(&payload, 0) };
        println!(
            "ch1={} ch2={} ch3={} ch4={} ch5={} count={} link={} timestamp_us={}",
            data.chan0_raw_us(),
            data.chan1_raw_us(),
            data.chan2_raw_us(),
            data.chan3_raw_us(),
            data.chan4_raw_us(),
            data.channel_count(),
            data.link_quality_pct(),
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

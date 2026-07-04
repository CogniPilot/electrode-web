use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{ArgAction, Parser};
use electrode_ppm_bridge::{
    ChannelInversions, ChannelMap, FAILSAFE_CHANNELS, PpmChannels, WireOverrides, build_packet,
    channel_inversions_from_slice, channel_map_from_slice, channels_to_pwm_signal_outputs_payload,
    channels_to_wire, manual_control_to_channels, pwm_signal_outputs_to_channels,
};
use synapse_fbs::topic::{ManualControlData, ManualControlFlags, RadioControlData};
use thiserror::Error;
use zenoh::{Wait, config::Config};

#[derive(Debug, Parser)]
#[command(
    name = "electrode-ppm-bridge",
    version,
    about = "Bridge Synapse manual/autopilot outputs on Zenoh to a PPM encoder serial link",
    long_about = "Subscribes to Synapse ManualControlData bare structs and the CUBS2 autopilot's \
PwmSignalOutputsData samples over Zenoh. The ManualControl mode switch selects either \
manual passthrough or autopilot output, while the active/stabilization switch drives \
the final PPM channel. The bridge then sends the same 14-byte serial packet used by \
the ROS ppm_bridge Arduino encoder.",
    next_line_help = true,
    after_help = "\
Examples:
  electrode-ppm-bridge --serial-device /dev/ttyACM0
  electrode-ppm-bridge --manual-topic synapse/v1/topic/manual_control_command --control-output-topic synapse/v1/topic/pwm_signal_outputs --channel-map 1,2,0,3,4
  electrode-ppm-bridge --no-serial --pwm-output-topic synapse/motor_output --radio-output-topic synapse/v1/topic/radio_control

Environment:
  ZENOH_CONNECT, ZENOH_TOPIC, ZENOH_CONTROL_OUTPUT_TOPIC, ZENOH_PWM_OUTPUT_TOPIC, ZENOH_RADIO_OUTPUT_TOPIC
  PPM_SERIAL_DEVICE, PPM_BAUD_RATE, PPM_NO_SERIAL, PPM_CHANNEL_MAP, PPM_CHANNEL_INVERT,
  PPM_FORCE_IDLE_THROTTLE, PPM_FORCE_STABILIZING_MODE"
)]
struct Cli {
    #[command(flatten)]
    zenoh: ZenohArgs,

    #[command(flatten)]
    serial: SerialArgs,

    #[command(flatten)]
    ppm: PpmArgs,
}

#[derive(Debug, Parser)]
#[command(next_help_heading = "Zenoh")]
struct ZenohArgs {
    #[arg(
        long = "zenoh-connect",
        env = "ZENOH_CONNECT",
        value_name = "LOCATOR",
        default_value = "udp/127.0.0.1:7447",
        help = "Zenoh router/peer locator to connect to (matches the ground-bridge peer default)"
    )]
    zenoh_connect: String,

    #[arg(
        long = "manual-topic",
        alias = "zenoh-topic",
        alias = "topic",
        env = "ZENOH_TOPIC",
        value_name = "KEYEXPR",
        default_value = "synapse/v1/topic/manual_control_command",
        help = "Zenoh key expression carrying synapse.topic.ManualControlData bare structs"
    )]
    manual_topic: String,

    #[arg(
        long = "control-output-topic",
        env = "ZENOH_CONTROL_OUTPUT_TOPIC",
        value_name = "KEYEXPR",
        default_value = "synapse/v1/topic/pwm_signal_outputs",
        help = "Zenoh key expression carrying the autopilot's arbitrated synapse.topic.PwmSignalOutputsData bare structs"
    )]
    control_output_topic: String,

    #[arg(
        long = "radio-output-topic",
        env = "ZENOH_RADIO_OUTPUT_TOPIC",
        value_name = "KEYEXPR",
        default_value = "synapse/v1/topic/radio_control",
        help = "Zenoh key expression mirroring the post-arbitration radio wire as synapse.topic.RadioControlData bare structs (must differ from the manual topic: the bridge subscribes there and would re-ingest its own output)"
    )]
    radio_output_topic: String,

    #[arg(
        long = "pwm-output-topic",
        env = "ZENOH_PWM_OUTPUT_TOPIC",
        value_name = "KEYEXPR",
        default_value = "synapse/motor_output",
        help = "Zenoh key expression for the selected post-arbitration PWM command stream (must differ from the subscribed control-output topic)"
    )]
    pwm_output_topic: String,
}

#[derive(Debug, Parser)]
#[command(next_help_heading = "Serial")]
struct SerialArgs {
    #[arg(
        long = "serial-device",
        env = "PPM_SERIAL_DEVICE",
        value_name = "PATH",
        default_value = "/dev/ttyACM0",
        help = "Serial device connected to the PPM encoder"
    )]
    serial_device: String,

    #[arg(
        long = "baud-rate",
        env = "PPM_BAUD_RATE",
        value_name = "BAUD",
        default_value_t = 57_600,
        help = "Serial baud rate"
    )]
    baud_rate: u32,

    #[arg(
        long = "serial-timeout-ms",
        env = "PPM_SERIAL_TIMEOUT_MS",
        value_name = "MS",
        default_value_t = 100,
        help = "Serial write timeout"
    )]
    serial_timeout_ms: u64,

    #[arg(
        long = "no-serial",
        env = "PPM_NO_SERIAL",
        action = ArgAction::SetTrue,
        help = "Run without opening or writing a physical PPM serial encoder"
    )]
    no_serial: bool,
}

#[derive(Debug, Parser)]
#[command(next_help_heading = "PPM")]
struct PpmArgs {
    #[arg(
        long = "channel-map",
        env = "PPM_CHANNEL_MAP",
        value_delimiter = ',',
        value_name = "INDEXES",
        default_value = "0,1,2,3,4",
        help = "Comma-separated output channel map over base order throttle,roll,pitch,yaw,stabilization"
    )]
    channel_map: Vec<usize>,

    #[arg(
        long = "channel-invert",
        env = "PPM_CHANNEL_INVERT",
        value_delimiter = ',',
        value_name = "BOOLS",
        default_value = "false,false,false,false,false",
        help = "Comma-separated inversion flags for each serial output channel"
    )]
    channel_invert: Vec<bool>,

    #[arg(
        long = "force-idle-throttle",
        env = "PPM_FORCE_IDLE_THROTTLE",
        action = ArgAction::Set,
        default_value_t = true,
        help = "Force the throttle source channel to idle before serial output"
    )]
    force_idle_throttle: bool,

    #[arg(
        long = "force-stabilizing-mode",
        env = "PPM_FORCE_STABILIZING_MODE",
        action = ArgAction::Set,
        default_value_t = true,
        help = "Force the stabilization source channel high before serial output"
    )]
    force_stabilizing_mode: bool,
}

#[derive(Debug, Error)]
enum BridgeError {
    #[error("zenoh error: {0}")]
    Zenoh(String),
    #[error("serial error: {0}")]
    Serial(#[from] serialport::Error),
    #[error("serial write error: {0}")]
    SerialWrite(#[from] std::io::Error),
    #[error("channel map error: {0}")]
    ChannelMap(#[from] electrode_ppm_bridge::ChannelMapError),
    #[error("manual control payload is {actual} bytes, expected {expected}")]
    InvalidManualControlSize { expected: usize, actual: usize },
    #[error(
        "{kind} output topic {topic:?} collides with subscribed topic {subscribed:?}: the bridge \
         would re-ingest its own arbitrated output"
    )]
    OutputTopicCollision {
        kind: &'static str,
        topic: String,
        subscribed: String,
    },
}

/// Wire size of a bare `synapse.topic.ManualControlData` struct.
const MANUAL_CONTROL_PAYLOAD_SIZE: usize = 40;

type Result<T> = std::result::Result<T, BridgeError>;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let channel_map = channel_map_from_slice(&cli.ppm.channel_map)?;
    let channel_invert = channel_inversions_from_slice(&cli.ppm.channel_invert)?;
    let overrides = WireOverrides {
        force_idle_throttle: cli.ppm.force_idle_throttle,
        force_stabilizing_mode: cli.ppm.force_stabilizing_mode,
    };
    run(cli, channel_map, channel_invert, overrides)
}

fn run(
    cli: Cli,
    channel_map: ChannelMap,
    channel_invert: ChannelInversions,
    overrides: WireOverrides,
) -> Result<()> {
    let session = zenoh::open(zenoh_config(&cli)?)
        .wait()
        .map_err(|error| BridgeError::Zenoh(error.to_string()))?;
    let manual_subscriber = session
        .declare_subscriber(cli.zenoh.manual_topic.clone())
        .wait()
        .map_err(|error| BridgeError::Zenoh(error.to_string()))?;
    let control_subscriber = session
        .declare_subscriber(cli.zenoh.control_output_topic.clone())
        .wait()
        .map_err(|error| BridgeError::Zenoh(error.to_string()))?;
    let radio_publisher = if cli.zenoh.radio_output_topic.trim().is_empty() {
        None
    } else if cli.zenoh.radio_output_topic == cli.zenoh.manual_topic {
        return Err(BridgeError::OutputTopicCollision {
            kind: "radio",
            topic: cli.zenoh.radio_output_topic.clone(),
            subscribed: cli.zenoh.manual_topic.clone(),
        });
    } else {
        Some(
            session
                .declare_publisher(cli.zenoh.radio_output_topic.clone())
                .wait()
                .map_err(|error| BridgeError::Zenoh(error.to_string()))?,
        )
    };
    let pwm_publisher = if cli.zenoh.pwm_output_topic.trim().is_empty() {
        None
    } else if cli.zenoh.pwm_output_topic == cli.zenoh.manual_topic {
        return Err(BridgeError::OutputTopicCollision {
            kind: "pwm",
            topic: cli.zenoh.pwm_output_topic.clone(),
            subscribed: cli.zenoh.manual_topic.clone(),
        });
    } else if cli.zenoh.pwm_output_topic == cli.zenoh.control_output_topic {
        return Err(BridgeError::OutputTopicCollision {
            kind: "pwm",
            topic: cli.zenoh.pwm_output_topic.clone(),
            subscribed: cli.zenoh.control_output_topic.clone(),
        });
    } else {
        Some(
            session
                .declare_publisher(cli.zenoh.pwm_output_topic.clone())
                .wait()
                .map_err(|error| BridgeError::Zenoh(error.to_string()))?,
        )
    };

    let mut serial = if cli.serial.no_serial {
        None
    } else {
        Some(
            serialport::new(&cli.serial.serial_device, cli.serial.baud_rate)
                .timeout(Duration::from_millis(cli.serial.serial_timeout_ms))
                .open()?,
        )
    };

    println!(
        "listening on manual={} control_output={} pwm_output={} radio_output={} serial={} baud={} channel_map={:?} channel_invert={:?} force_idle_throttle={} force_stabilizing_mode={}",
        cli.zenoh.manual_topic,
        cli.zenoh.control_output_topic,
        cli.zenoh.pwm_output_topic,
        cli.zenoh.radio_output_topic,
        if cli.serial.no_serial {
            "disabled"
        } else {
            cli.serial.serial_device.as_str()
        },
        cli.serial.baud_rate,
        channel_map.0,
        channel_invert.0,
        overrides.force_idle_throttle,
        overrides.force_stabilizing_mode,
    );

    let mut manual_mode = ManualMode::Failsafe;
    let mut manual_channels = PpmChannels(FAILSAFE_CHANNELS);
    let mut control_channels: Option<PpmChannels> = None;

    loop {
        let mut updated = false;

        if let Some(sample) = manual_subscriber
            .recv_timeout(Duration::from_millis(10))
            .map_err(|error| BridgeError::Zenoh(error.to_string()))?
        {
            let payload = sample.payload().to_bytes();
            match manual_from_payload(&payload) {
                Ok(manual) => {
                    manual_mode = manual.mode;
                    manual_channels = manual.channels;
                    updated = true;
                }
                Err(BridgeError::InvalidManualControlSize { expected, actual }) => {
                    eprintln!(
                        "dropping ManualControl payload: {actual} bytes, expected {expected}"
                    );
                }
                Err(error) => return Err(error),
            }
        }

        while let Some(sample) = control_subscriber
            .recv_timeout(Duration::ZERO)
            .map_err(|error| BridgeError::Zenoh(error.to_string()))?
        {
            let payload = sample.payload().to_bytes();
            match pwm_signal_outputs_to_channels(&payload) {
                Some(channels) => {
                    control_channels = Some(channels);
                    updated = true;
                }
                None => {
                    eprintln!("dropping invalid PwmSignalOutputsData control_output payload");
                }
            }
        }

        if !updated {
            continue;
        }

        let channels = select_channels(manual_mode, manual_channels, control_channels);
        if let Some(publisher) = &pwm_publisher {
            publisher
                .put(channels_to_pwm_signal_outputs_payload(channels))
                .wait()
                .map_err(|error| BridgeError::Zenoh(error.to_string()))?;
        }
        let wire_channels = channels_to_wire(channels, channel_map, channel_invert, overrides);
        if let Some(publisher) = &radio_publisher {
            publisher
                .put(encode_radio_control(wire_channels))
                .wait()
                .map_err(|error| BridgeError::Zenoh(error.to_string()))?;
        }
        if let Some(serial) = serial.as_mut() {
            serial.write_all(&build_packet(wire_channels))?;
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ManualMode {
    Manual,
    Auto,
    Failsafe,
}

#[derive(Debug, Clone, Copy)]
struct ManualSelection {
    mode: ManualMode,
    channels: PpmChannels,
}

fn manual_from_payload(payload: &[u8]) -> Result<ManualSelection> {
    // 0.3.0 transmits ManualControlData as a bare fixed-layout struct, not a
    // FlatBuffers root table: verify the exact size then follow at offset 0.
    if payload.len() != MANUAL_CONTROL_PAYLOAD_SIZE {
        return Err(BridgeError::InvalidManualControlSize {
            expected: MANUAL_CONTROL_PAYLOAD_SIZE,
            actual: payload.len(),
        });
    }
    // Safety: fixed-layout structs are repr(transparent) byte arrays with
    // unaligned accessors, and the size check above covers the struct.
    let data = unsafe { <ManualControlData as flatbuffers::Follow>::follow(payload, 0) };
    Ok(manual_from_data(data))
}

fn manual_from_data(data: &ManualControlData) -> ManualSelection {
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

    ManualSelection {
        mode,
        channels: manual_control_to_channels(data),
    }
}

fn select_channels(
    manual_mode: ManualMode,
    manual_channels: PpmChannels,
    control_channels: Option<PpmChannels>,
) -> PpmChannels {
    // Source selection is only the manual mode switch:
    //   manual -> transmitter sticks
    //   auto   -> autopilot/control output
    // Stabilization/active is a separate radio channel and stays operator-owned.
    match manual_mode {
        ManualMode::Manual => manual_channels,
        ManualMode::Auto => control_channels
            .map(|mut channels| {
                channels.0[4] = manual_channels.0[4];
                channels
            })
            .unwrap_or(PpmChannels(FAILSAFE_CHANNELS)),
        ManualMode::Failsafe => PpmChannels(FAILSAFE_CHANNELS),
    }
}

fn encode_radio_control(channels: PpmChannels) -> Vec<u8> {
    // Mirror the exact channel values sent to the PPM encoder as a bare
    // RadioControlData struct: the stream the web app plots as RadioControl.
    let ch = channels.0;
    let data = RadioControlData::new(
        timestamp_us(),
        ch.len() as u8,
        100,
        ch[0],
        ch[1],
        ch[2],
        ch[3],
        ch[4],
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    );

    // Publish the raw bare-struct bytes (inverse of topic_decode's follow).
    data.0.to_vec()
}

fn timestamp_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn zenoh_config(cli: &Cli) -> Result<Config> {
    let mut config = Config::default();
    config
        .insert_json5("mode", "\"client\"")
        .map_err(|error| BridgeError::Zenoh(error.to_string()))?;
    config
        .insert_json5(
            "connect/endpoints",
            &format!("[\"{}\"]", cli.zenoh.zenoh_connect),
        )
        .map_err(|error| BridgeError::Zenoh(error.to_string()))?;
    // Match the other ground-station bridges: explicit endpoint, no multicast
    // discovery, so every component deterministically connects to the same hub.
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .map_err(|error| BridgeError::Zenoh(error.to_string()))?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_mode_uses_control_channels_but_manual_stabilization() {
        let manual_channels = PpmChannels([1000, 1100, 1200, 1300, 2000]);
        let control_channels = PpmChannels([1500, 1600, 1700, 1800, 1000]);

        assert_eq!(
            select_channels(ManualMode::Auto, manual_channels, Some(control_channels)),
            PpmChannels([1500, 1600, 1700, 1800, 2000])
        );
    }

    #[test]
    fn auto_mode_without_control_output_fails_safe_low() {
        assert_eq!(
            select_channels(
                ManualMode::Auto,
                PpmChannels([1000, 1100, 1200, 1300, 2000]),
                None
            ),
            PpmChannels(FAILSAFE_CHANNELS)
        );
    }

    #[test]
    fn manual_mode_uses_manual_channels_directly() {
        let manual_channels = PpmChannels([1000, 1100, 1200, 1300, 2000]);
        let control_channels = PpmChannels([1500, 1600, 1700, 1800, 1000]);

        assert_eq!(
            select_channels(ManualMode::Manual, manual_channels, Some(control_channels)),
            manual_channels
        );
    }
}

use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{ArgAction, Parser};
use synapse_fbs::topic::{ManualControlAxes, ManualControlData, ManualControlFlags};
use thiserror::Error;
use zenoh::{config::Config, Wait};

const JS_EVENT_BUTTON: u8 = 0x01;
const JS_EVENT_AXIS: u8 = 0x02;
const JS_EVENT_INIT: u8 = 0x80;

#[derive(Debug, Parser)]
#[command(
    name = "electrode-manual-control-bridge",
    version,
    about = "Bridge a USB RC transmitter joystick to Synapse ManualControl over Zenoh",
    long_about = "Reads Linux /dev/input/js* events from a USB RC transmitter, converts them into \
synapse.topic.ManualControlData bare structs, and publishes them on a Zenoh key expression.",
    next_line_help = true,
    after_help = "\
Examples:
  electrode-manual-control-bridge --device /dev/input/js0
  electrode-manual-control-bridge --device /dev/input/js0 --zenoh-connect udp/127.0.0.1:7447

Environment:
  JOYSTICK_DEVICE, ZENOH_CONNECT, ZENOH_TOPIC"
)]
struct Cli {
    #[command(flatten)]
    input: InputArgs,

    #[command(flatten)]
    mapping: MappingArgs,

    #[command(flatten)]
    zenoh: ZenohArgs,
}

#[derive(Debug, Parser)]
#[command(next_help_heading = "Input")]
struct InputArgs {
    #[arg(
        long,
        env = "JOYSTICK_DEVICE",
        value_name = "PATH",
        default_value = "/dev/input/js0",
        help = "Linux joystick device for the USB RC transmitter"
    )]
    device: PathBuf,

    #[arg(
        long,
        value_name = "HZ",
        default_value_t = 50.0,
        help = "ManualControl publish rate while the node is running"
    )]
    publish_hz: f64,

    #[arg(
        long,
        value_name = "MS",
        help = "Optional watchdog that marks valid=false after this long without joystick events"
    )]
    stale_ms: Option<u64>,
}

#[derive(Debug, Parser)]
#[command(next_help_heading = "Mapping")]
struct MappingArgs {
    #[arg(long, default_value_t = 1, help = "Joystick axis index for roll")]
    roll_axis: u8,

    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = false,
        help = "Invert roll axis sign"
    )]
    invert_roll: bool,

    #[arg(long, default_value_t = 2, help = "Joystick axis index for pitch")]
    pitch_axis: u8,

    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = false,
        help = "Invert pitch axis sign"
    )]
    invert_pitch: bool,

    #[arg(long, default_value_t = 3, help = "Joystick axis index for yaw")]
    yaw_axis: u8,

    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = false,
        help = "Invert yaw axis sign"
    )]
    invert_yaw: bool,

    #[arg(long, default_value_t = 0, help = "Joystick axis index for throttle")]
    throttle_axis: u8,

    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = false,
        help = "Invert throttle axis before mapping to [0, 1]"
    )]
    invert_throttle: bool,

    #[arg(
        long,
        default_value_t = 4,
        help = "Joystick axis index for flight mode switch"
    )]
    mode_axis: u8,

    #[arg(
        long,
        default_value_t = 5,
        help = "Joystick axis index for active/manual-enable switch"
    )]
    active_axis: u8,

    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = true,
        help = "Invert active switch sign before thresholding"
    )]
    invert_active: bool,

    #[arg(
        long,
        value_name = "INDEX",
        help = "Optional joystick button index for arm_switch"
    )]
    arm_button: Option<u8>,

    #[arg(
        long,
        value_name = "INDEX",
        help = "Optional joystick button index for kill_switch"
    )]
    kill_button: Option<u8>,

    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = false,
        help = "Treat the arm button as a toggle: each press latches high/low (vs momentary while held)"
    )]
    arm_toggle: bool,

    #[arg(
        long,
        action = ArgAction::Set,
        default_value_t = false,
        help = "Treat the kill button as a toggle: each press latches high/low"
    )]
    kill_toggle: bool,
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
enum BridgeError {
    #[error("input error: {0}")]
    Io(#[from] io::Error),
    #[error("zenoh error: {0}")]
    Zenoh(String),
    #[error("publish_hz must be positive")]
    InvalidPublishRate,
}

type Result<T> = std::result::Result<T, BridgeError>;

#[derive(Debug, Clone, Copy)]
struct JoystickEvent {
    kind: EventKind,
    number: u8,
    value: i16,
}

#[derive(Debug, Clone, Copy)]
enum EventKind {
    Axis,
    Button,
}

#[derive(Debug)]
struct ManualState {
    axes: [f32; 32],
    buttons: [bool; 64],
    last_event: Option<Instant>,
    // Latched outputs for buttons configured as toggles (flip on each press).
    arm_latched: bool,
    kill_latched: bool,
}

impl Default for ManualState {
    fn default() -> Self {
        Self {
            axes: [0.0; 32],
            buttons: [false; 64],
            last_event: None,
            arm_latched: false,
            kill_latched: false,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let publish_period = publish_period(cli.input.publish_hz)?;
    let stale_after = cli.input.stale_ms.map(Duration::from_millis);

    let (tx, rx) = mpsc::channel();
    let device = cli.input.device.clone();
    thread::spawn(move || {
        if let Err(error) = read_joystick_events(device, tx) {
            eprintln!("joystick reader stopped: {error}");
        }
    });

    let session = zenoh::open(zenoh_config(&cli)?)
        .wait()
        .map_err(|error| BridgeError::Zenoh(error.to_string()))?;
    let publisher = session
        .declare_publisher(cli.zenoh.topic.clone())
        .wait()
        .map_err(|error| BridgeError::Zenoh(error.to_string()))?;

    let mut state = ManualState::default();
    loop {
        match rx.recv_timeout(publish_period) {
            Ok(event) => {
                apply_event(&mut state, event, &cli.mapping);
                while let Ok(event) = rx.try_recv() {
                    apply_event(&mut state, event, &cli.mapping);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(BridgeError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "joystick reader disconnected",
                )));
            }
        }

        let valid = state.last_event.is_some_and(|last_event| {
            stale_after.is_none_or(|stale_after| last_event.elapsed() <= stale_after)
        });
        let payload = encode_manual_control(&state, &cli.mapping, valid);
        publisher
            .put(payload)
            .wait()
            .map_err(|error| BridgeError::Zenoh(error.to_string()))?;
    }
}

fn publish_period(publish_hz: f64) -> Result<Duration> {
    if publish_hz <= 0.0 {
        return Err(BridgeError::InvalidPublishRate);
    }
    Ok(Duration::from_secs_f64(1.0 / publish_hz))
}

fn read_joystick_events(device: PathBuf, tx: mpsc::Sender<JoystickEvent>) -> io::Result<()> {
    let mut file = File::open(device)?;
    let mut buf = [0_u8; 8];

    loop {
        file.read_exact(&mut buf)?;
        let value = i16::from_ne_bytes([buf[4], buf[5]]);
        let event_type = buf[6] & !JS_EVENT_INIT;
        let number = buf[7];
        let kind = match event_type {
            JS_EVENT_AXIS => EventKind::Axis,
            JS_EVENT_BUTTON => EventKind::Button,
            _ => continue,
        };

        if tx
            .send(JoystickEvent {
                kind,
                number,
                value,
            })
            .is_err()
        {
            return Ok(());
        }
    }
}

fn apply_event(state: &mut ManualState, event: JoystickEvent, mapping: &MappingArgs) {
    state.last_event = Some(Instant::now());
    match event.kind {
        EventKind::Axis => {
            if let Some(axis) = state.axes.get_mut(usize::from(event.number)) {
                *axis = normalize_axis(event.value);
            }
        }
        EventKind::Button => {
            let idx = usize::from(event.number);
            let pressed = event.value != 0;
            let was_pressed = state.buttons.get(idx).copied().unwrap_or(false);
            if let Some(button) = state.buttons.get_mut(idx) {
                *button = pressed;
            }
            // On a rising edge (press), flip any toggle latched to this button.
            if pressed && !was_pressed {
                if mapping.arm_toggle && mapping.arm_button == Some(event.number) {
                    state.arm_latched = !state.arm_latched;
                }
                if mapping.kill_toggle && mapping.kill_button == Some(event.number) {
                    state.kill_latched = !state.kill_latched;
                }
            }
        }
    }
}

fn normalize_axis(value: i16) -> f32 {
    if value < 0 {
        f32::from(value) / 32768.0
    } else {
        f32::from(value) / 32767.0
    }
    .clamp(-1.0, 1.0)
}

fn encode_manual_control(state: &ManualState, mapping: &MappingArgs, valid: bool) -> Vec<u8> {
    let roll = signed_axis(state, mapping.roll_axis, mapping.invert_roll);
    let pitch = signed_axis(state, mapping.pitch_axis, mapping.invert_pitch);
    let yaw = signed_axis(state, mapping.yaw_axis, mapping.invert_yaw);
    let throttle = throttle_axis(state, mapping.throttle_axis, mapping.invert_throttle);
    let aux = aux_axes(state);
    let mode_axis = signed_axis(state, mapping.mode_axis, false);
    let flight_mode = if mode_axis > 0.0 { 1 } else { 0 };
    let active = signed_axis(state, mapping.active_axis, mapping.invert_active) > 0.0;
    let arm_switch = if mapping.arm_toggle {
        state.arm_latched
    } else {
        mapped_button(state, mapping.arm_button)
    };
    let kill_switch = if mapping.kill_toggle {
        state.kill_latched
    } else {
        mapped_button(state, mapping.kill_button)
    };

    // Scale normalized stick/aux inputs (-1..1) to the wire's -1000..1000 shorts.
    let to_milli = |value: f32| (value * 1000.0).round().clamp(-1000.0, 1000.0) as i16;
    // We drive the four sticks plus aux0..5, so mark those axes valid.
    let active_axes = (ManualControlAxes::Pitch
        | ManualControlAxes::Roll
        | ManualControlAxes::Throttle
        | ManualControlAxes::Yaw
        | ManualControlAxes::Aux0
        | ManualControlAxes::Aux1
        | ManualControlAxes::Aux2
        | ManualControlAxes::Aux3
        | ManualControlAxes::Aux4
        | ManualControlAxes::Aux5)
        .bits();
    let mut flags = ManualControlFlags::empty();
    if arm_switch {
        flags |= ManualControlFlags::ArmSwitch;
    }
    if kill_switch {
        flags |= ManualControlFlags::KillSwitch;
    }
    if active {
        flags |= ManualControlFlags::Active;
    }
    if valid {
        flags |= ManualControlFlags::Valid;
    }

    let data = ManualControlData::new(
        timestamp_us(),
        0,
        active_axes,
        to_milli(pitch),
        to_milli(roll),
        to_milli(throttle),
        to_milli(yaw),
        to_milli(aux[0]),
        to_milli(aux[1]),
        to_milli(aux[2]),
        to_milli(aux[3]),
        to_milli(aux[4]),
        to_milli(aux[5]),
        flight_mode,
        flags.bits(),
    );

    // Publish the raw bare-struct bytes (inverse of topic_decode's follow).
    data.0.to_vec()
}

fn signed_axis(state: &ManualState, axis: u8, invert: bool) -> f32 {
    let value = state
        .axes
        .get(usize::from(axis))
        .copied()
        .unwrap_or_default();
    if invert {
        -value
    } else {
        value
    }
}

fn throttle_axis(state: &ManualState, axis: u8, invert: bool) -> f32 {
    let value = signed_axis(state, axis, invert);
    ((value + 1.0) * 0.5).clamp(0.0, 1.0)
}

fn aux_axes(state: &ManualState) -> [f32; 8] {
    let mut aux = [0.0; 8];
    aux.copy_from_slice(&state.axes[..8]);
    aux
}

fn mapped_button(state: &ManualState, button: Option<u8>) -> bool {
    button
        .and_then(|button| state.buttons.get(usize::from(button)))
        .copied()
        .unwrap_or(false)
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
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .map_err(|error| BridgeError::Zenoh(error.to_string()))?;
    Ok(config)
}

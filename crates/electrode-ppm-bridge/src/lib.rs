use std::time::{SystemTime, UNIX_EPOCH};
use synapse_fbs::topic::{ManualControlData, ManualControlFlags, PwmSignalOutputsData};

pub const NUM_CHANNELS: usize = 5;
pub const PACKET_LEN: usize = 14;
pub const PACKET_HEADER: u16 = 0xffff;
pub const DEFAULT_CHANNEL_MAP: [usize; NUM_CHANNELS] = [0, 1, 2, 3, 4];
pub const DEFAULT_CHANNEL_INVERT: [bool; NUM_CHANNELS] = [false; NUM_CHANNELS];
pub const IDLE_THROTTLE_PWM: u16 = 1000;
pub const STABILIZATION_PWM: u16 = 2000;
pub const LOW_CHANNELS: [u16; NUM_CHANNELS] = [1000; NUM_CHANNELS];
pub const FAILSAFE_CHANNELS: [u16; NUM_CHANNELS] = LOW_CHANNELS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PpmChannels(pub [u16; NUM_CHANNELS]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelMap(pub [usize; NUM_CHANNELS]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelInversions(pub [bool; NUM_CHANNELS]);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WireOverrides {
    pub force_idle_throttle: bool,
    pub force_stabilizing_mode: bool,
}

impl Default for ChannelMap {
    fn default() -> Self {
        Self(DEFAULT_CHANNEL_MAP)
    }
}

impl Default for ChannelInversions {
    fn default() -> Self {
        Self(DEFAULT_CHANNEL_INVERT)
    }
}

impl ChannelMap {
    pub fn new(channels: [usize; NUM_CHANNELS]) -> Result<Self, ChannelMapError> {
        if let Some(channel) = channels.iter().find(|&&channel| channel >= NUM_CHANNELS) {
            return Err(ChannelMapError::OutOfRange(*channel));
        }

        Ok(Self(channels))
    }

    pub fn apply(self, channels: PpmChannels) -> PpmChannels {
        PpmChannels(self.0.map(|index| channels.0[index]))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelMapError {
    WrongLength { expected: usize, actual: usize },
    OutOfRange(usize),
}

impl std::fmt::Display for ChannelMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongLength { expected, actual } => {
                write!(f, "expected {expected} channel-map entries, got {actual}")
            }
            Self::OutOfRange(channel) => {
                write!(
                    f,
                    "channel map entry {channel} is out of range 0..{}",
                    NUM_CHANNELS - 1
                )
            }
        }
    }
}

impl std::error::Error for ChannelMapError {}

pub fn channel_map_from_slice(channels: &[usize]) -> Result<ChannelMap, ChannelMapError> {
    let channels: [usize; NUM_CHANNELS] =
        channels
            .try_into()
            .map_err(|_| ChannelMapError::WrongLength {
                expected: NUM_CHANNELS,
                actual: channels.len(),
            })?;
    ChannelMap::new(channels)
}

pub fn channel_inversions_from_slice(
    channels: &[bool],
) -> Result<ChannelInversions, ChannelMapError> {
    let channels: [bool; NUM_CHANNELS] =
        channels
            .try_into()
            .map_err(|_| ChannelMapError::WrongLength {
                expected: NUM_CHANNELS,
                actual: channels.len(),
            })?;
    Ok(ChannelInversions(channels))
}

pub fn manual_control_to_channels(data: &ManualControlData) -> PpmChannels {
    // 0.3.0 ManualControlData carries switch state in the flags bitmask and the
    // axes as scaled shorts in -1000..1000 (normalized = value / 1000).
    let flags = ManualControlFlags::from_bits_retain(data.flags());
    if !flags.contains(ManualControlFlags::Valid) || flags.contains(ManualControlFlags::KillSwitch)
    {
        return PpmChannels(FAILSAFE_CHANNELS);
    }

    PpmChannels([
        throttle_to_pwm(milli_to_norm(data.throttle_milli())),
        centered_to_pwm(milli_to_norm(data.roll_milli())),
        centered_to_pwm(milli_to_norm(data.pitch_milli())),
        centered_to_pwm(-milli_to_norm(data.yaw_milli())),
        stabilization_to_pwm(flags.contains(ManualControlFlags::Active)),
    ])
}

/// Wire size of a bare `synapse.topic.PwmSignalOutputsData` struct.
pub const PWM_SIGNAL_OUTPUTS_PAYLOAD_SIZE: usize = 48;

pub fn pwm_signal_outputs_to_channels(payload: &[u8]) -> Option<PpmChannels> {
    if payload.len() != PWM_SIGNAL_OUTPUTS_PAYLOAD_SIZE {
        return None;
    }
    // Safety: fixed-layout structs are repr(transparent) byte arrays with
    // unaligned accessors, and the size check above covers the struct.
    let data = unsafe { <PwmSignalOutputsData as flatbuffers::Follow>::follow(payload, 0) };

    // CUBS2 copies its arbitrated RC channels 1:1 into output<N>_us
    // (0=roll, 1=pitch, 2=throttle, 3=yaw, 4=stabilization), so reorder to the
    // base throttle,roll,pitch,yaw,stabilization PPM order and keep pitch/yaw servo
    // inversions carried over from the RcChannels16-era bridge.
    Some(PpmChannels([
        clamp_pwm_u16(data.output2_us()),
        clamp_pwm_u16(data.output0_us()),
        invert_pwm_u16(data.output1_us()),
        invert_pwm_u16(data.output3_us()),
        clamp_pwm_u16(data.output4_us()),
    ]))
}

pub fn channels_to_pwm_signal_outputs_payload(channels: PpmChannels) -> Vec<u8> {
    // Selected command stream is canonical AETR PwmSignalOutputsData:
    // output0=aileron/roll, output1=elevator/pitch, output2=throttle,
    // output3=rudder/yaw, output4=stabilization.
    let [throttle, roll, pitch, yaw, stabilization] = channels.0;
    let data = PwmSignalOutputsData::new(
        timestamp_us(),
        0b1_1111,
        0,
        roll,
        pitch,
        throttle,
        yaw,
        stabilization,
        1000,
        1000,
        1000,
        1000,
        1000,
        1000,
        1000,
        1000,
        1000,
        1000,
        1000,
    );
    data.0.to_vec()
}

pub fn channels_to_wire(
    channels: PpmChannels,
    channel_map: ChannelMap,
    inversions: ChannelInversions,
    overrides: WireOverrides,
) -> PpmChannels {
    let mut wire = [0_u16; NUM_CHANNELS];
    for (output, source) in channel_map.0.into_iter().enumerate() {
        let mut value = channels.0[source];
        let forced = match source {
            0 if overrides.force_idle_throttle => {
                value = IDLE_THROTTLE_PWM;
                true
            }
            4 if overrides.force_stabilizing_mode => {
                value = STABILIZATION_PWM;
                true
            }
            _ => false,
        };
        wire[output] = if inversions.0[output] && !forced {
            invert_pwm_u16(value)
        } else {
            value
        };
    }
    PpmChannels(wire)
}

pub fn build_packet(channels: PpmChannels) -> [u8; PACKET_LEN] {
    let mut packet = [0_u8; PACKET_LEN];
    packet[0..2].copy_from_slice(&PACKET_HEADER.to_le_bytes());

    for (index, channel) in channels.0.iter().enumerate() {
        let start = 2 + index * 2;
        packet[start..start + 2].copy_from_slice(&channel.to_le_bytes());
    }

    let checksum = checksum(channels);
    packet[12..14].copy_from_slice(&checksum.to_le_bytes());
    packet
}

pub fn checksum(channels: PpmChannels) -> u16 {
    channels
        .0
        .iter()
        .fold(0_u16, |sum, channel| sum.wrapping_add(*channel))
}

fn clamp_pwm_u16(value: u16) -> u16 {
    value.clamp(1000, 2000)
}

fn invert_pwm_u16(value: u16) -> u16 {
    (3000 - i32::from(value)).clamp(1000, 2000) as u16
}

fn milli_to_norm(value: i16) -> f32 {
    f32::from(value) / 1000.0
}

fn throttle_to_pwm(value: f32) -> u16 {
    scale_to_pwm(value, 0.0, 1.0, 1000.0, 2000.0)
}

fn centered_to_pwm(value: f32) -> u16 {
    scale_to_pwm(value, -1.0, 1.0, 1000.0, 2000.0)
}

fn stabilization_to_pwm(active: bool) -> u16 {
    if active {
        2000
    } else {
        1000
    }
}

fn scale_to_pwm(value: f32, in_min: f32, in_max: f32, out_min: f32, out_max: f32) -> u16 {
    let value = if value.is_finite() { value } else { in_min };
    let normalized = ((value - in_min) / (in_max - in_min)).clamp(0.0, 1.0);
    (out_min + normalized * (out_max - out_min)).round() as u16
}

fn timestamp_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use synapse_fbs::topic::{ManualControlAxes, ManualControlData, ManualControlFlags};

    /// The four stick axes we always populate on the wire.
    const STICK_AXES: ManualControlAxes = ManualControlAxes::Pitch
        .union(ManualControlAxes::Roll)
        .union(ManualControlAxes::Throttle)
        .union(ManualControlAxes::Yaw);

    fn to_milli(value: f32) -> i16 {
        (value * 1000.0).round().clamp(-1000.0, 1000.0) as i16
    }

    /// Build a 0.3.0 ManualControlData from normalized stick inputs and switch
    /// flags (axes scaled to -1000..1000 shorts).
    fn manual_control_data_with_flags(
        roll: f32,
        pitch: f32,
        yaw: f32,
        throttle: f32,
        flight_mode: u8,
        flags: ManualControlFlags,
    ) -> ManualControlData {
        ManualControlData::new(
            42,
            0,
            STICK_AXES.bits(),
            to_milli(pitch),
            to_milli(roll),
            to_milli(throttle),
            to_milli(yaw),
            0,
            0,
            0,
            0,
            0,
            0,
            flight_mode,
            flags.bits(),
        )
    }

    fn manual_control_data(
        roll: f32,
        pitch: f32,
        yaw: f32,
        throttle: f32,
        flight_mode: u8,
    ) -> ManualControlData {
        manual_control_data_with_flags(
            roll,
            pitch,
            yaw,
            throttle,
            flight_mode,
            ManualControlFlags::Active | ManualControlFlags::Valid,
        )
    }

    #[test]
    fn maps_manual_control_to_base_ppm_channels() {
        let data = manual_control_data(0.5, 0.25, -0.5, 0.75, 1);
        assert_eq!(
            manual_control_to_channels(&data),
            PpmChannels([1750, 1750, 1625, 1750, 2000])
        );
    }

    #[test]
    fn invalid_or_kill_switch_messages_use_failsafe_channels() {
        // valid=false: no Valid flag set.
        let invalid =
            manual_control_data_with_flags(1.0, 1.0, 1.0, 1.0, 1, ManualControlFlags::Active);
        // kill switch engaged even though active + valid.
        let killed = manual_control_data_with_flags(
            1.0,
            1.0,
            1.0,
            1.0,
            1,
            ManualControlFlags::Active | ManualControlFlags::Valid | ManualControlFlags::KillSwitch,
        );

        assert_eq!(
            manual_control_to_channels(&invalid),
            PpmChannels(FAILSAFE_CHANNELS)
        );
        assert_eq!(
            manual_control_to_channels(&killed),
            PpmChannels(FAILSAFE_CHANNELS)
        );
    }

    #[test]
    fn failsafe_channels_are_all_low() {
        assert_eq!(FAILSAFE_CHANNELS, [1000; NUM_CHANNELS]);
    }

    #[test]
    fn maps_inactive_valid_manual_control_to_channels() {
        let data =
            manual_control_data_with_flags(-0.2, 0.4, 0.1, 0.6, 0, ManualControlFlags::Valid);

        assert_eq!(
            manual_control_to_channels(&data),
            PpmChannels([1600, 1400, 1700, 1450, 1000])
        );
    }

    #[test]
    fn applies_channel_map_to_base_channels() {
        let map = ChannelMap::new([1, 2, 0, 3, 4]).unwrap();
        assert_eq!(
            map.apply(PpmChannels([1000, 1500, 1600, 1700, 2000])),
            PpmChannels([1500, 1600, 1000, 1700, 2000])
        );
    }

    #[test]
    fn applies_wire_mapping_inversions_and_safe_overrides() {
        assert_eq!(
            channels_to_wire(
                PpmChannels([1200, 1300, 1400, 1500, 1000]),
                ChannelMap([1, 2, 0, 3, 4]),
                ChannelInversions([true, true, true, false, true]),
                WireOverrides {
                    force_idle_throttle: true,
                    force_stabilizing_mode: true,
                },
            ),
            PpmChannels([1700, 1600, IDLE_THROTTLE_PWM, 1500, STABILIZATION_PWM])
        );
    }

    #[test]
    fn encodes_reference_serial_packet_format() {
        let packet = build_packet(PpmChannels([1000, 1500, 1500, 1500, 2000]));

        assert_eq!(packet[0..2], [0xff, 0xff]);
        assert_eq!(
            packet[2..12],
            [0xe8, 0x03, 0xdc, 0x05, 0xdc, 0x05, 0xdc, 0x05, 0xd0, 0x07]
        );
        assert_eq!(packet[12..14], 7500_u16.to_le_bytes());
    }

    #[test]
    fn rejects_bad_channel_maps() {
        assert_eq!(
            channel_map_from_slice(&[0, 1, 2]).unwrap_err(),
            ChannelMapError::WrongLength {
                expected: 5,
                actual: 3
            }
        );
        assert_eq!(
            channel_map_from_slice(&[0, 1, 2, 3, 5]).unwrap_err(),
            ChannelMapError::OutOfRange(5)
        );
    }

    #[test]
    fn maps_pwm_signal_outputs_payload_to_base_channels() {
        // roll=1100, pitch=1200, throttle=1300, yaw=1400, stabilization=1500.
        let data = PwmSignalOutputsData::new(
            42, 0xffff, 0, 1100, 1200, 1300, 1400, 1500, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        );

        assert_eq!(
            pwm_signal_outputs_to_channels(&data.0),
            Some(PpmChannels([1300, 1100, 1800, 1600, 1500]))
        );
    }

    #[test]
    fn rejects_wrong_size_pwm_signal_outputs_payload() {
        assert_eq!(pwm_signal_outputs_to_channels(&[0_u8; 20]), None);
        assert_eq!(
            pwm_signal_outputs_to_channels(&[0_u8; PWM_SIGNAL_OUTPUTS_PAYLOAD_SIZE - 1]),
            None
        );
        assert_eq!(
            pwm_signal_outputs_to_channels(&[0_u8; PWM_SIGNAL_OUTPUTS_PAYLOAD_SIZE + 1]),
            None
        );
    }
}

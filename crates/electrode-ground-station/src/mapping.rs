//! RC mapping profile: which joystick axis/button drives each control, plus
//! inversions. Mirrors the `electrode-manual-control-bridge` CLI so a profile
//! can be turned directly into the arguments that launch it.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MappingProfile {
    /// Joystick node the bridge reads, e.g. `/dev/input/js0`.
    pub device: String,
    /// Zenoh locator the bridge publishes to.
    pub zenoh_connect: String,

    pub roll_axis: u8,
    pub invert_roll: bool,
    pub pitch_axis: u8,
    pub invert_pitch: bool,
    pub yaw_axis: u8,
    pub invert_yaw: bool,
    pub throttle_axis: u8,
    pub invert_throttle: bool,
    pub mode_axis: u8,
    pub active_axis: u8,
    pub invert_active: bool,
    pub arm_button: Option<u8>,
    pub kill_button: Option<u8>,
    #[serde(default)]
    pub arm_toggle: bool,
    #[serde(default)]
    pub kill_toggle: bool,
    #[serde(default = "default_ppm_channel_map")]
    pub ppm_channel_map: [usize; 5],
    #[serde(default)]
    pub ppm_channel_invert: [bool; 5],
    #[serde(default)]
    pub ppm_force_idle_throttle: bool,
    #[serde(default)]
    pub ppm_force_stabilizing_mode: bool,
}

fn default_ppm_channel_map() -> [usize; 5] {
    [1, 2, 0, 3, 4]
}

fn join_usize(values: &[usize]) -> String {
    values
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn join_bool(values: &[bool]) -> String {
    values
        .iter()
        .map(bool::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

impl Default for MappingProfile {
    fn default() -> Self {
        // Matches the manual-control-bridge defaults (tuned for an RC
        // transmitter); a gamepad like the F310 will need remapping via the UI.
        Self {
            device: "/dev/input/js0".to_string(),
            zenoh_connect: "udp/127.0.0.1:7447".to_string(),
            roll_axis: 1,
            invert_roll: false,
            pitch_axis: 2,
            invert_pitch: false,
            yaw_axis: 3,
            invert_yaw: false,
            throttle_axis: 0,
            invert_throttle: false,
            mode_axis: 4,
            active_axis: 5,
            invert_active: true,
            arm_button: None,
            kill_button: None,
            arm_toggle: false,
            kill_toggle: false,
            ppm_channel_map: default_ppm_channel_map(),
            ppm_channel_invert: [false; 5],
            ppm_force_idle_throttle: false,
            ppm_force_stabilizing_mode: false,
        }
    }
}

impl MappingProfile {
    /// Load a profile from disk, falling back to defaults if absent/unreadable.
    pub(crate) fn load_or_default(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Persist the profile as pretty JSON.
    pub(crate) fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        std::fs::write(path, text)
    }

    /// Build the `electrode-manual-control-bridge` argument vector for this
    /// profile.
    pub(crate) fn bridge_args(&self) -> Vec<String> {
        let mut args = vec![
            "--device".into(),
            self.device.clone(),
            "--zenoh-connect".into(),
            self.zenoh_connect.clone(),
            "--roll-axis".into(),
            self.roll_axis.to_string(),
            "--invert-roll".into(),
            self.invert_roll.to_string(),
            "--pitch-axis".into(),
            self.pitch_axis.to_string(),
            "--invert-pitch".into(),
            self.invert_pitch.to_string(),
            "--yaw-axis".into(),
            self.yaw_axis.to_string(),
            "--invert-yaw".into(),
            self.invert_yaw.to_string(),
            "--throttle-axis".into(),
            self.throttle_axis.to_string(),
            "--invert-throttle".into(),
            self.invert_throttle.to_string(),
            "--mode-axis".into(),
            self.mode_axis.to_string(),
            "--active-axis".into(),
            self.active_axis.to_string(),
            "--invert-active".into(),
            self.invert_active.to_string(),
        ];
        if let Some(arm) = self.arm_button {
            args.push("--arm-button".into());
            args.push(arm.to_string());
        }
        if let Some(kill) = self.kill_button {
            args.push("--kill-button".into());
            args.push(kill.to_string());
        }
        args.push("--arm-toggle".into());
        args.push(self.arm_toggle.to_string());
        args.push("--kill-toggle".into());
        args.push(self.kill_toggle.to_string());
        args
    }

    /// Build the selected-output PPM bridge args. This bridge subscribes to
    /// manual control + autopilot PWM, then publishes the selected PWM stream
    /// for simulation and writes the selected PPM packet to the radio encoder.
    pub(crate) fn ppm_bridge_args(&self) -> Vec<String> {
        vec![
            "--zenoh-connect".into(),
            self.zenoh_connect.clone(),
            "--pwm-output-topic".into(),
            "synapse/motor_output".into(),
            "--radio-output-topic".into(),
            "synapse/v1/topic/radio_control".into(),
            "--channel-map".into(),
            join_usize(&self.ppm_channel_map),
            "--channel-invert".into(),
            join_bool(&self.ppm_channel_invert),
            "--force-idle-throttle".into(),
            self.ppm_force_idle_throttle.to_string(),
            "--force-stabilizing-mode".into(),
            self.ppm_force_stabilizing_mode.to_string(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppm_bridge_args_use_sport_cub_aetrm_serial_order() {
        let args = MappingProfile::default().ppm_bridge_args();

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--channel-map", "1,2,0,3,4"]));
    }

    #[test]
    fn ppm_bridge_args_include_inversions_and_low_defaults() {
        let profile = MappingProfile {
            ppm_channel_invert: [true, false, true, false, true],
            ..Default::default()
        };
        let args = profile.ppm_bridge_args();

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--channel-invert", "true,false,true,false,true"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--force-idle-throttle", "false"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--force-stabilizing-mode", "false"]));
    }
}

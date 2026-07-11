//! Autopilot stack profile.
//!
//! This belongs in the Ground Station daemon, not the static Viewer, because it
//! deals with local files and runtime configuration.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub(crate) const MOCAP_POSE_TOPIC: &str = "synapse/mocap/rigid_body/cub1/pose";
/// Raw CUB1 pose from synapse_qualisys_bridge. Unlike the bridge's filtered
/// pose/odometry outputs, this contains the unfiltered QTM measurement.
pub(crate) const RAW_MOCAP_TOPIC: &str = "qualisys/cub1/pose_raw";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AutopilotProfile {
    pub stack_name: String,
    pub stack_path: String,
    pub firmware_source: FirmwareSource,
    pub firmware_artifact: String,
    pub board_target: String,
    pub flash_method: FlashMethod,
    pub runtime_transport: RuntimeTransport,
    pub runtime_endpoint: String,
    pub mission_protocol: RuntimeProtocol,
    pub parameter_protocol: RuntimeProtocol,
    pub calibration_protocol: RuntimeProtocol,
    /// cubs2 `native_sim` firmware binary run by the dashboard start/stop
    /// control (see `autopilot_link`).
    #[serde(default = "default_native_binary")]
    pub native_binary: String,
    /// Port the firmware's csyn UDP transport listens on (inbound topics).
    #[serde(default = "default_udp_rx_port")]
    pub udp_rx_port: u16,
    /// Port the firmware's csyn UDP transport sends to (outbound topics).
    #[serde(default = "default_udp_tx_port")]
    pub udp_tx_port: u16,
    /// Topic keys forwarded from Zenoh into the firmware. Only what
    /// the autopilot consumes — its own publications must not loop back.
    #[serde(default = "default_inbound_topics")]
    pub inbound_topics: Vec<String>,
    /// Which mocap producer is selected by the ground station.
    #[serde(default)]
    pub mocap_source: MocapSource,
}

fn default_native_binary() -> String {
    std::env::var("ELECTRODE_AUTOPILOT_NATIVE_BINARY")
        .unwrap_or_else(|_| "../cerebri_cubs2/build-native_sim/zephyr/zephyr.exe".to_string())
}

fn default_udp_rx_port() -> u16 {
    4250
}

fn default_udp_tx_port() -> u16 {
    4251
}

fn default_inbound_topics() -> Vec<String> {
    vec![RAW_MOCAP_TOPIC.to_string(), "manual".to_string()]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) enum MocapSource {
    Sim,
    #[default]
    Real,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FirmwareSource {
    LocalBuild,
    ReleaseArtifact,
    CiArtifact,
    CustomFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FlashMethod {
    UsbBootloader,
    Dfu,
    SerialBootloader,
    SdCard,
    ExternalTool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RuntimeTransport {
    Zenoh,
    MavlinkSerial,
    MavlinkUdp,
    MavlinkTcp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RuntimeProtocol {
    SynapseZenoh,
    Mavlink,
}

impl Default for AutopilotProfile {
    fn default() -> Self {
        Self {
            stack_name: "Cerebri CUBS2".to_string(),
            stack_path: "../cerebri_cubs2".to_string(),
            firmware_source: FirmwareSource::LocalBuild,
            firmware_artifact: "build-mr_vmu_tropic/zephyr/zephyr.bin".to_string(),
            board_target: "mr_vmu_tropic".to_string(),
            flash_method: FlashMethod::UsbBootloader,
            runtime_transport: RuntimeTransport::Zenoh,
            runtime_endpoint: "udp/127.0.0.1:7447".to_string(),
            mission_protocol: RuntimeProtocol::SynapseZenoh,
            parameter_protocol: RuntimeProtocol::SynapseZenoh,
            calibration_protocol: RuntimeProtocol::SynapseZenoh,
            native_binary: default_native_binary(),
            udp_rx_port: default_udp_rx_port(),
            udp_tx_port: default_udp_tx_port(),
            inbound_topics: default_inbound_topics(),
            mocap_source: MocapSource::default(),
        }
    }
}

impl AutopilotProfile {
    /// Inbound topic suffixes with whitespace/empties filtered out.
    pub(crate) fn inbound_topics(&self) -> Vec<String> {
        self.inbound_topics
            .iter()
            .map(|suffix| suffix.trim().to_string())
            .map(|suffix| normalize_inbound_topic(&suffix))
            .filter(|suffix| !suffix.is_empty())
            .collect()
    }

    pub(crate) fn normalized(mut self) -> Self {
        // The outer-loop process is co-located with the Ground Station. Keep
        // its control plane on loopback even if an older profile points at a
        // LAN router; external traffic must cross command authority first.
        self.runtime_endpoint = "udp/127.0.0.1:7447".to_string();
        self.inbound_topics = self
            .inbound_topics
            .into_iter()
            .map(|topic| normalize_inbound_topic(&topic))
            .filter(|topic| !topic.is_empty())
            .collect();
        if self.inbound_topics.is_empty() {
            self.inbound_topics = default_inbound_topics();
        }
        self
    }

    /// Firmware console log written by the autopilot link, next to the binary
    /// so it lands in the firmware build tree (`.../zephyr/autopilot.log`).
    pub(crate) fn native_log_path(&self) -> String {
        let binary = Path::new(self.native_binary.trim());
        binary
            .parent()
            .map(|dir| dir.join("autopilot.log"))
            .unwrap_or_else(|| Path::new("autopilot.log").to_path_buf())
            .display()
            .to_string()
    }

    /// Load a profile from disk, falling back to a Cerebri-oriented default.
    pub(crate) fn load_or_default(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(&text).ok())
            .unwrap_or_default()
            .normalized()
    }

    /// Persist the profile as pretty JSON.
    pub(crate) fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        std::fs::write(path, text)
    }
}

fn normalize_inbound_topic(topic: &str) -> String {
    let topic = topic.trim();
    if topic == "cub1/external_pose" || topic == "external_pose/1" {
        return RAW_MOCAP_TOPIC.to_string();
    }
    if topic == "qualisys/cub1/pose" {
        return RAW_MOCAP_TOPIC.to_string();
    }
    if topic.ends_with("mocap/frame") || topic.ends_with("mocap_frame") {
        return RAW_MOCAP_TOPIC.to_string();
    }
    if topic.contains("/mocap/rigid_body/") || topic.contains("/mocap/selected/rigid_body/") {
        return RAW_MOCAP_TOPIC.to_string();
    }
    // Profiles saved before synapse_fbs 0.6.0 name manual control by its old
    // snake suffix; coerce onto the compact catalog key.
    if topic == "manual_control_command" || topic.ends_with("/manual_control_command") {
        return "manual".to_string();
    }
    topic.to_string()
}

#[cfg(test)]
mod tests {
    use super::{AutopilotProfile, MocapSource, RAW_MOCAP_TOPIC};

    #[test]
    fn normalizes_legacy_selected_mocap_inbound_topic_to_external_pose() {
        let profile = AutopilotProfile {
            inbound_topics: vec![
                "synapse/mocap/selected/rigid_body/cub1/pose".to_string(),
                "manual_control_command".to_string(),
            ],
            mocap_source: MocapSource::Sim,
            ..AutopilotProfile::default()
        }
        .normalized();

        assert_eq!(profile.inbound_topics[0], RAW_MOCAP_TOPIC);
        assert_eq!(profile.inbound_topics[1], "manual");
        assert_eq!(profile.mocap_source, MocapSource::Sim);
    }

    #[test]
    fn migrates_raw_mocap_profiles_to_external_pose() {
        let profile = AutopilotProfile {
            inbound_topics: vec!["synapse/mocap/frame".to_string()],
            ..AutopilotProfile::default()
        }
        .normalized();

        assert_eq!(profile.inbound_topics[0], RAW_MOCAP_TOPIC);
    }

    #[test]
    fn forces_outer_loop_runtime_onto_localhost() {
        let profile = AutopilotProfile {
            runtime_endpoint: "udp/192.168.10.2:7447".to_string(),
            ..AutopilotProfile::default()
        }
        .normalized();

        assert_eq!(profile.runtime_endpoint, "udp/127.0.0.1:7447");
    }
}

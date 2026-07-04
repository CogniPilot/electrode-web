//! Autopilot stack profile and firmware-install planning.
//!
//! This belongs in the Ground Station daemon, not the static Viewer, because it
//! deals with local files, plugged-in hardware, and eventually bootloader access.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutopilotProfile {
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
    /// Topic key suffixes forwarded from Zenoh into the firmware. Only what
    /// the autopilot consumes — its own publications must not loop back.
    #[serde(default = "default_inbound_topics")]
    pub inbound_topics: Vec<String>,
}

fn default_native_binary() -> String {
    "/home/micah/autopilot/cerebri_cubs2/build-native_sim/zephyr/zephyr.exe".to_string()
}

fn default_udp_rx_port() -> u16 {
    4250
}

fn default_udp_tx_port() -> u16 {
    4251
}

fn default_inbound_topics() -> Vec<String> {
    vec![
        "synapse/mocap/rigid_body/cub1/pose".to_string(),
        "manual_control_command".to_string(),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FirmwareSource {
    LocalBuild,
    ReleaseArtifact,
    CiArtifact,
    CustomFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FlashMethod {
    UsbBootloader,
    Dfu,
    SerialBootloader,
    SdCard,
    ExternalTool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeTransport {
    Zenoh,
    MavlinkSerial,
    MavlinkUdp,
    MavlinkTcp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeProtocol {
    SynapseZenoh,
    Mavlink,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareInstallRequest {
    pub device: String,
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareInstallStatus {
    pub job_id: String,
    pub status: InstallState,
    pub message: String,
    pub profile: AutopilotProfile,
    pub device: String,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallState {
    Planned,
    Rejected,
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
        }
    }
}

impl AutopilotProfile {
    /// Inbound topic suffixes with whitespace/empties filtered out.
    pub fn inbound_topics(&self) -> Vec<String> {
        self.inbound_topics
            .iter()
            .map(|suffix| suffix.trim().to_string())
            .filter(|suffix| !suffix.is_empty())
            .collect()
    }

    /// Firmware console log written by the autopilot link, next to the binary
    /// so it lands in the firmware build tree (`.../zephyr/autopilot.log`).
    pub fn native_log_path(&self) -> String {
        let binary = Path::new(self.native_binary.trim());
        binary
            .parent()
            .map(|dir| dir.join("autopilot.log"))
            .unwrap_or_else(|| Path::new("autopilot.log").to_path_buf())
            .display()
            .to_string()
    }

    /// Load a profile from disk, falling back to a Cerebri-oriented default.
    pub fn load_or_default(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Persist the profile as pretty JSON.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        std::fs::write(path, text)
    }

    pub fn install_plan(&self, request: FirmwareInstallRequest) -> FirmwareInstallStatus {
        let artifact = self.firmware_artifact.trim();
        let device = request.device.trim();
        let mut steps = vec![
            format!("Use {} autopilot profile", self.stack_name),
            format!("Resolve firmware artifact: {}", artifact),
            format!("Target board: {}", self.board_target),
            format!("Prepare flash method: {:?}", self.flash_method),
            format!("Open hardware device: {}", device),
            "Reboot target into bootloader mode".to_string(),
            "Erase, program, verify, then reboot into runtime firmware".to_string(),
        ];

        if matches!(self.runtime_transport, RuntimeTransport::Zenoh) {
            steps.push(format!(
                "After install, reconnect runtime transport over Zenoh at {}",
                self.runtime_endpoint
            ));
        } else {
            steps.push(format!(
                "After install, reconnect runtime transport over MAVLink at {}",
                self.runtime_endpoint
            ));
        }

        let rejected = if artifact.is_empty() {
            Some("firmware artifact is required")
        } else if device.is_empty() {
            Some("hardware device is required")
        } else if !request.confirmed {
            Some("install request requires explicit operator confirmation")
        } else {
            None
        };

        if let Some(reason) = rejected {
            return FirmwareInstallStatus {
                job_id: new_job_id(),
                status: InstallState::Rejected,
                message: reason.to_string(),
                profile: self.clone(),
                device: device.to_string(),
                steps,
            };
        }

        FirmwareInstallStatus {
            job_id: new_job_id(),
            status: InstallState::Planned,
            message: "install plan created; flashing backend is not enabled yet".to_string(),
            profile: self.clone(),
            device: device.to_string(),
            steps,
        }
    }
}

fn new_job_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("fw-{millis}")
}

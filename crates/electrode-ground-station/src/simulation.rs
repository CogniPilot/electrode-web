//! Browser Rumoca simulation profile and model storage.
//!
//! Ground Station owns the editable Modelica source file. The simulator runtime
//! itself lives in the web app via the `@cognipilot/rumoca` npm package.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct SimulationProfile {
    pub backend: SimulationBackend,
    pub mode: SimulationMode,
    pub vehicle_kind: SimulationVehicleKind,
    /// Directory inside Electrode that contains the editable Modelica source.
    pub project_path: String,
    /// Deprecated native-Rumoca config path. Kept for JSON compatibility only.
    pub generated_config_path: String,
    pub model_path: String,
    pub model_editable: bool,
    pub modelica_lsp_command: String,
    pub timing_mode: String,
    pub simulation_dt: f64,
    pub lockstep_send_rate_hz: f64,
    pub lockstep_receive_rate_hz: f64,
    pub lockstep_max_step_dt: f64,
    pub zenoh_connect: String,
    pub command_input_topic: String,
    pub actuator_output_topic: String,
    pub sensor_output_topic: String,
    pub telemetry_output_topic: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SimulationBackend {
    Rumoca,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SimulationMode {
    WithAutopilot,
    DirectCommands,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SimulationVehicleKind {
    FixedWing,
    Quadrotor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelicaFile {
    pub path: String,
    pub text: String,
    pub editable: bool,
    pub lsp_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelicaFileSave {
    pub path: String,
    pub text: String,
}

impl Default for SimulationProfile {
    fn default() -> Self {
        let project_path = "models".to_string();
        Self {
            backend: SimulationBackend::Rumoca,
            mode: SimulationMode::WithAutopilot,
            vehicle_kind: SimulationVehicleKind::FixedWing,
            project_path,
            generated_config_path: String::new(),
            model_path: "FixedWingTrueSILFull.mo".to_string(),
            model_editable: true,
            modelica_lsp_command: "modelica-language-server".to_string(),
            timing_mode: "realtime".to_string(),
            simulation_dt: 0.002,
            lockstep_send_rate_hz: 240.0,
            lockstep_receive_rate_hz: 50.0,
            lockstep_max_step_dt: 0.002,
            zenoh_connect: "udp/127.0.0.1:7447".to_string(),
            command_input_topic: "synapse/motor_output".to_string(),
            actuator_output_topic: "synapse/motor_output".to_string(),
            sensor_output_topic: "synapse/v1/sim/sensors".to_string(),
            telemetry_output_topic: crate::sim_bridge::PRIVATE_MOCAP_TOPIC.to_string(),
        }
    }
}

impl SimulationProfile {
    pub(crate) fn normalized(mut self) -> Self {
        self.normalize();
        self
    }

    /// Load a profile from disk, falling back to the Electrode-owned model.
    pub(crate) fn load_or_default(path: &Path) -> Self {
        let mut profile: Self = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        profile.normalize();
        profile
    }

    /// Persist the profile as pretty JSON.
    pub(crate) fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut profile = self.clone();
        profile.normalize();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&profile).unwrap_or_default();
        std::fs::write(path, text)
    }

    pub(crate) fn read_model_file(&self) -> std::io::Result<ModelicaFile> {
        let profile = self.clone().normalized();
        let path = profile.model_file_path()?;
        Ok(ModelicaFile {
            path: path.display().to_string(),
            text: std::fs::read_to_string(path)?,
            editable: profile.model_editable,
            lsp_command: profile.modelica_lsp_command,
        })
    }

    pub(crate) fn save_model_file(&self, file: ModelicaFileSave) -> std::io::Result<ModelicaFile> {
        let profile = self.clone().normalized();
        if !profile.model_editable {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "model editing is disabled for this simulation profile",
            ));
        }
        let path = profile.allowed_model_path(&file.path)?;
        std::fs::write(&path, file.text)?;
        Ok(ModelicaFile {
            path: path.display().to_string(),
            text: std::fs::read_to_string(path)?,
            editable: true,
            lsp_command: profile.modelica_lsp_command,
        })
    }

    fn normalize(&mut self) {
        let defaults = Self::default();
        if self.project_path.trim().is_empty() || is_old_external_rumoca_path(&self.project_path) {
            self.project_path = defaults.project_path;
        }
        self.generated_config_path.clear();
        let default_model_path = "FixedWingTrueSILFull.mo".to_string();
        if self.model_path.trim().is_empty()
            || model_path_needs_reset(&self.model_path)
            || is_old_external_rumoca_path(&self.model_path)
            || path_repeats_project_dir(&self.project_path, &self.model_path)
        {
            self.model_path = default_model_path;
        }
        if self.modelica_lsp_command.trim().is_empty() {
            self.modelica_lsp_command = defaults.modelica_lsp_command;
        }
        if self.timing_mode.trim().is_empty() {
            self.timing_mode = defaults.timing_mode;
        }
        if self.simulation_dt <= 0.0 {
            self.simulation_dt = defaults.simulation_dt;
        }
        if self.lockstep_send_rate_hz <= 0.0 {
            self.lockstep_send_rate_hz = defaults.lockstep_send_rate_hz;
        }
        if self.lockstep_receive_rate_hz <= 0.0 {
            self.lockstep_receive_rate_hz = defaults.lockstep_receive_rate_hz;
        }
        if self.lockstep_max_step_dt <= 0.0 {
            self.lockstep_max_step_dt = defaults.lockstep_max_step_dt;
        }
        if self.zenoh_connect.trim().is_empty() {
            self.zenoh_connect = defaults.zenoh_connect;
        }
        if self.command_input_topic.trim().is_empty()
            || self.command_input_topic == "synapse/flight_snapshot"
            || self.command_input_topic == "synapse/manual_control"
            || self.command_input_topic == "synapse/radio_control"
            || self.command_input_topic == "synapse/control_output"
            || self.command_input_topic == "synapse/v1/topic/radio_control"
            || self.command_input_topic == "synapse/v1/topic/manual_control_command"
            || self.command_input_topic == "synapse/v1/topic/pwm_signal_outputs"
            || self.command_input_topic == "rc"
            || self.command_input_topic == "manual"
            || self.command_input_topic == "pwm"
        {
            self.command_input_topic = defaults.command_input_topic;
        }
        if self.actuator_output_topic.trim().is_empty()
            || self.actuator_output_topic == "synapse/control_output"
            || self.actuator_output_topic == "synapse/v1/topic/pwm_signal_outputs"
            || self.actuator_output_topic == "pwm"
        {
            self.actuator_output_topic = defaults.actuator_output_topic;
        }
        if self.sensor_output_topic.trim().is_empty()
            || self.sensor_output_topic == "synapse/sim/sensors"
        {
            self.sensor_output_topic = defaults.sensor_output_topic;
        }
        if self.telemetry_output_topic.trim().is_empty()
            || self.telemetry_output_topic == "synapse/sim/telemetry"
            || self.telemetry_output_topic == "synapse/sim_input"
            || self.telemetry_output_topic == "synapse/mocap/frame"
            || self.telemetry_output_topic == "synapse/mocap/rigid_body/cub1/pose"
            || self.telemetry_output_topic == "synapse/v1/topic/mocap_frame"
            || self.telemetry_output_topic == "synapse/v1/sil/sim_input"
            || self.telemetry_output_topic == "qualisys/mocap"
            || self.telemetry_output_topic == "mocap"
        {
            self.telemetry_output_topic = defaults.telemetry_output_topic;
        }
    }

    fn model_file_path(&self) -> std::io::Result<PathBuf> {
        self.allowed_model_path(&self.model_path)
    }

    fn allowed_model_path(&self, path: &str) -> std::io::Result<PathBuf> {
        let path = PathBuf::from(path);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if !(file_name.ends_with(".mo") || file_name.ends_with(".mo.in")) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "simulation model must be a .mo or .mo.in file",
            ));
        }

        let project = PathBuf::from(&self.project_path).canonicalize()?;
        let candidate = if path.is_absolute() {
            path
        } else {
            project.join(path)
        }
        .canonicalize()?;

        if !candidate.starts_with(&project) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "model file must live inside the Electrode models directory",
            ));
        }
        Ok(candidate)
    }
}

fn model_path_needs_reset(path: &str) -> bool {
    let path = Path::new(path);
    path.components().any(|component| {
        component.as_os_str() == ".electrode" || component.as_os_str() == ".rumoca"
    })
}

fn is_old_external_rumoca_path(path: &str) -> bool {
    path.contains("/rumoca/")
}

fn path_repeats_project_dir(project_path: &str, model_path: &str) -> bool {
    let project = Path::new(project_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    !project.is_empty() && Path::new(model_path).starts_with(project)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_uses_electrode_owned_model() {
        let profile = SimulationProfile::default();
        assert_eq!(profile.project_path, "models");
        assert_eq!(profile.model_path, "FixedWingTrueSILFull.mo");
        assert_eq!(profile.generated_config_path, "");
    }

    #[test]
    fn normalize_migrates_old_external_rumoca_paths() {
        let mut profile = SimulationProfile {
            project_path: "/tmp/external/rumoca/project".to_string(),
            model_path: "/tmp/external/rumoca/project/FixedWingTrueSILFull.mo".to_string(),
            generated_config_path: "/tmp/old.toml".to_string(),
            ..Default::default()
        };
        profile.normalize();
        assert_eq!(profile.project_path, "models");
        assert_eq!(profile.model_path, "FixedWingTrueSILFull.mo");
        assert_eq!(profile.generated_config_path, "");
    }

    #[test]
    fn normalize_removes_repeated_models_prefix() {
        let mut profile = SimulationProfile {
            project_path: "models".to_string(),
            model_path: "models/FixedWingTrueSILFull.mo".to_string(),
            ..Default::default()
        };
        profile.normalize();
        assert_eq!(profile.model_path, "FixedWingTrueSILFull.mo");
    }
}

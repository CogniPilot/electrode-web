//! Rumoca simulation add-on profile and process supervision.
//!
//! The Ground Station owns this because launching a simulator touches local
//! executables, model files, logs, and Zenoh endpoints. The browser only edits
//! the profile and requests lifecycle changes.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::sim_bridge::SimBridgeCounts;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct SimulationProfile {
    pub backend: SimulationBackend,
    pub mode: SimulationMode,
    pub vehicle_kind: SimulationVehicleKind,
    pub project_path: String,
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
    pub executable: String,
    /// Directory that receives the Synapse `.bfbs` reflection schemas. The
    /// schemas themselves are embedded in the `synapse_fbs` crate and written
    /// here on demand; an empty value materializes them under the project's
    /// `.electrode/bfbs` directory.
    pub schema_bfbs_dir: String,
    /// Rumoca viewer asset directory (skybox / ground / generic models). A
    /// rumoca-distribution concern; overridable per profile / via env.
    pub asset_dir: String,
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SimulationStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub started_at_ms: Option<u128>,
    pub message: String,
    pub command_line: Vec<String>,
    pub sim_bridge: SimBridgeCounts,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SimulationCheckResult {
    pub ok: bool,
    pub status: Option<i32>,
    pub command_line: Vec<String>,
    pub stdout: String,
    pub stderr: String,
}

pub(crate) struct SimulationSupervisor {
    child: Mutex<Option<SimChild>>,
}

struct SimChild {
    child: Child,
    started_at_ms: u128,
    command_line: Vec<String>,
}

impl Default for SimulationProfile {
    fn default() -> Self {
        let project_path =
            "/home/micah/Research/development/rumoca/fixedwing_fullaero_truesil".to_string();
        let generated_config_path = format!("{project_path}/.electrode/rum.sil.generated.toml");
        let model_path = format!("{project_path}/FixedWingTrueSILFull.mo");
        Self {
            backend: SimulationBackend::Rumoca,
            mode: SimulationMode::WithAutopilot,
            vehicle_kind: SimulationVehicleKind::FixedWing,
            project_path,
            generated_config_path,
            model_path,
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
            telemetry_output_topic: "synapse/mocap/rigid_body/cub1/pose".to_string(),
            executable: std::env::var("ELECTRODE_RUMOCA_SIM_BIN")
                .unwrap_or_else(|_| "rumoca".to_string()),
            schema_bfbs_dir: default_schema_bfbs_dir(),
            asset_dir: default_asset_dir(),
        }
    }
}

/// Output directory for the Synapse `.bfbs` schemas materialized from the
/// `synapse_fbs` crate. Overridable via `ELECTRODE_SYNAPSE_BFBS_DIR`; empty by
/// default so they land under the project's `.electrode/bfbs` directory.
fn default_schema_bfbs_dir() -> String {
    std::env::var("ELECTRODE_SYNAPSE_BFBS_DIR").unwrap_or_default()
}

/// Rumoca's generic viewer assets. Overridable via `RUMOCA_ASSET_DIR`.
fn default_asset_dir() -> String {
    std::env::var("RUMOCA_ASSET_DIR").unwrap_or_else(|_| {
        "/home/micah/Research/development/rumoca/rumoca/examples/assets".to_string()
    })
}

impl SimulationProfile {
    pub(crate) fn normalized(mut self) -> Self {
        self.normalize();
        self
    }

    /// Load a profile from disk, falling back to a Rumoca-oriented default.
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

    pub(crate) fn check_config(&self) -> std::io::Result<SimulationCheckResult> {
        let config_path = self.write_generated_config()?;
        let command_line = vec![
            self.executable.clone(),
            "sim".to_string(),
            "check".to_string(),
            "-c".to_string(),
            config_path.display().to_string(),
        ];
        let output = Command::new(&self.executable)
            .args(["sim", "check", "-c"])
            .arg(&config_path)
            .current_dir(&self.project_path)
            .output()?;
        Ok(SimulationCheckResult {
            ok: output.status.success(),
            status: output.status.code(),
            command_line,
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    pub(crate) fn write_generated_config(&self) -> std::io::Result<PathBuf> {
        let mut profile = self.clone();
        profile.normalize();
        let path = PathBuf::from(&profile.generated_config_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, profile.generated_config_text()?)?;
        Ok(path)
    }

    fn normalize(&mut self) {
        let defaults = Self::default();
        if self.project_path.trim().is_empty() {
            self.project_path = defaults.project_path;
        }
        if self.generated_config_path.trim().is_empty() {
            self.generated_config_path =
                format!("{}/.electrode/rum.sil.generated.toml", self.project_path);
        }
        let default_model_path = format!("{}/FixedWingTrueSILFull.mo", self.project_path);
        if self.model_path.trim().is_empty() || model_path_needs_reset(&self.model_path) {
            self.model_path = default_model_path;
        } else if !PathBuf::from(&self.model_path).exists() {
            let fallback = Path::new(&self.project_path).join(
                Path::new(&self.model_path)
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new("FixedWingTrueSILFull.mo")),
            );
            if fallback.exists() {
                self.model_path = fallback.display().to_string();
            }
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
        // Legacy keys and wrong-message keys migrate to the selected post-
        // arbitration PWM stream consumed by Rumoca.
        if self.command_input_topic.trim().is_empty()
            || self.command_input_topic == "synapse/flight_snapshot"
            || self.command_input_topic == "synapse/manual_control"
            || self.command_input_topic == "synapse/radio_control"
            || self.command_input_topic == "synapse/control_output"
            || self.command_input_topic == "synapse/v1/topic/radio_control"
            || self.command_input_topic == "synapse/v1/topic/manual_control_command"
            || self.command_input_topic == "synapse/v1/topic/pwm_signal_outputs"
        {
            self.command_input_topic = defaults.command_input_topic;
        }
        if self.actuator_output_topic.trim().is_empty()
            || self.actuator_output_topic == "synapse/control_output"
            || self.actuator_output_topic == "synapse/v1/topic/pwm_signal_outputs"
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
            || self.telemetry_output_topic == "synapse/v1/topic/mocap_frame"
            || self.telemetry_output_topic == "synapse/v1/sil/sim_input"
        {
            self.telemetry_output_topic = defaults.telemetry_output_topic;
        }
        if self.executable.trim().is_empty() {
            self.executable = defaults.executable;
        }
        if self.schema_bfbs_dir.trim().is_empty() {
            self.schema_bfbs_dir = defaults.schema_bfbs_dir;
        }
        if self.asset_dir.trim().is_empty() {
            self.asset_dir = defaults.asset_dir;
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
                "model file must live inside the Rumoca project directory",
            ));
        }
        Ok(candidate)
    }

    /// Directory that receives the embedded Synapse `.bfbs` schemas. Honors an
    /// explicit profile override, otherwise nests them under the project's
    /// `.electrode/bfbs` directory alongside the generated config.
    fn schema_bfbs_output_dir(&self) -> PathBuf {
        if self.schema_bfbs_dir.trim().is_empty() {
            Path::new(&self.project_path).join(".electrode/bfbs")
        } else {
            PathBuf::from(&self.schema_bfbs_dir)
        }
    }

    /// Write the embedded Synapse binary schemas to disk and return their
    /// paths. The `synapse_fbs` crate ships one self-contained `.bfbs` per
    /// schema file, so the simulator loads the exact wire contract of this
    /// release without depending on a local synapse_fbs checkout.
    fn materialize_schema_bfbs(&self) -> std::io::Result<Vec<PathBuf>> {
        let dir = self.schema_bfbs_output_dir();
        std::fs::create_dir_all(&dir)?;
        let mut paths = Vec::new();
        for schema in synapse_fbs::schemas::SCHEMAS {
            let path = dir.join(format!("{}.bfbs", schema.name));
            std::fs::write(&path, schema.bfbs)?;
            paths.push(path);
        }
        Ok(paths)
    }

    fn generated_config_text(&self) -> std::io::Result<String> {
        // Emit an absolute model path. The generated config lives in
        // `<project>/.electrode/`, and Rumoca resolves `[model] file` relative
        // to the config file's own directory — so a project-relative path (e.g.
        // `FixedWingTrueSILFull.mo`) would be looked up under `.electrode/` and
        // fail. An absolute path resolves unambiguously wherever the config sits.
        let model_file = self.model_file_path()?.display().to_string();
        let asset_dir = self.asset_dir.clone();
        // Synapse schemas are electrode-owned: materialize the crate's embedded
        // binary schemas to disk and reference the written paths, rather than a
        // local synapse_fbs checkout.
        let bfbs_array = self
            .materialize_schema_bfbs()?
            .iter()
            .map(|path| format!("    \"{}\",", toml_escape(&path.display().to_string())))
            .collect::<Vec<_>>()
            .join("\n");
        // Absolute, for the same reason as the model file: the config lives in
        // `.electrode/`, and a relative `scene` would be resolved there instead
        // of at the project root where `fixedwing_scene.js` actually lives.
        let scene_file = Path::new(&self.project_path)
            .join("fixedwing_scene.js")
            .display()
            .to_string();

        Ok(GENERATED_RUMOCA_CONFIG
            .replace("__MODEL_FILE__", &toml_escape(&model_file))
            .replace("__SCENE_FILE__", &toml_escape(&scene_file))
            .replace("__TIMING_MODE__", &toml_escape(&self.timing_mode))
            .replace("__SIMULATION_DT__", &format_float(self.simulation_dt))
            .replace(
                "__LOCKSTEP_SEND_RATE_HZ__",
                &format_float(self.lockstep_send_rate_hz),
            )
            .replace(
                "__LOCKSTEP_RECEIVE_RATE_HZ__",
                &format_float(self.lockstep_receive_rate_hz),
            )
            .replace(
                "__LOCKSTEP_MAX_STEP_DT__",
                &format_float(self.lockstep_max_step_dt),
            )
            .replace(
                "__ZENOH_ENDPOINT_LINE__",
                &self.generated_zenoh_endpoint_line(),
            )
            .replace(
                "__SIM_INPUT_TOPIC__",
                &toml_escape(&self.telemetry_output_topic),
            )
            .replace(
                "__MANUAL_CONTROL_TOPIC__",
                &toml_escape(&self.command_input_topic),
            )
            .replace("__ASSET_DIR__", &toml_escape(&asset_dir))
            .replace("__SCHEMA_BFBS__", &bfbs_array))
    }

    fn generated_zenoh_endpoint_line(&self) -> String {
        let endpoint = self.zenoh_connect.trim();
        if endpoint.is_empty() {
            String::new()
        } else {
            format!("endpoint = \"{}\"", toml_escape(endpoint))
        }
    }
}

impl SimulationSupervisor {
    pub(crate) fn new() -> Self {
        Self {
            child: Mutex::new(None),
        }
    }

    pub(crate) fn status(&self) -> SimulationStatus {
        let mut guard = self.child.lock().expect("simulation lock poisoned");
        match guard.as_mut() {
            Some(sim) => match sim.child.try_wait() {
                Ok(Some(status)) => {
                    let command_line = sim.command_line.clone();
                    *guard = None;
                    SimulationStatus {
                        running: false,
                        pid: None,
                        started_at_ms: None,
                        message: format!("simulator exited with {status}"),
                        command_line,
                        sim_bridge: SimBridgeCounts::default(),
                    }
                }
                Ok(None) => SimulationStatus {
                    running: true,
                    pid: Some(sim.child.id()),
                    started_at_ms: Some(sim.started_at_ms),
                    message: "simulator running".to_string(),
                    command_line: sim.command_line.clone(),
                    sim_bridge: SimBridgeCounts::default(),
                },
                Err(err) => SimulationStatus {
                    running: true,
                    pid: Some(sim.child.id()),
                    started_at_ms: Some(sim.started_at_ms),
                    message: format!("simulator status unavailable: {err}"),
                    command_line: sim.command_line.clone(),
                    sim_bridge: SimBridgeCounts::default(),
                },
            },
            None => SimulationStatus {
                running: false,
                pid: None,
                started_at_ms: None,
                message: "simulator stopped".to_string(),
                command_line: Vec::new(),
                sim_bridge: SimBridgeCounts::default(),
            },
        }
    }

    pub(crate) fn start(&self, profile: &SimulationProfile) -> std::io::Result<SimulationStatus> {
        self.stop();
        let config_path = profile.write_generated_config()?;
        let mut command = Command::new(&profile.executable);
        command.args(["sim", "-c"]);
        command.arg(&config_path);
        command.env("ELECTRODE_SIM_BACKEND", format!("{:?}", profile.backend));
        command.env("ELECTRODE_SIM_MODE", format!("{:?}", profile.mode));
        command.env(
            "ELECTRODE_SIM_VEHICLE",
            format!("{:?}", profile.vehicle_kind),
        );
        command.env("ELECTRODE_SIM_MODEL", &profile.model_path);
        command.env("ELECTRODE_SIM_ZENOH_CONNECT", &profile.zenoh_connect);
        command.env("ELECTRODE_SIM_COMMAND_TOPIC", &profile.command_input_topic);
        command.env(
            "ELECTRODE_SIM_ACTUATOR_TOPIC",
            &profile.actuator_output_topic,
        );
        command.env("ELECTRODE_SIM_SENSOR_TOPIC", &profile.sensor_output_topic);
        command.env(
            "ELECTRODE_SIM_TELEMETRY_TOPIC",
            &profile.telemetry_output_topic,
        );

        let child = command.spawn()?;
        let sim = SimChild {
            child,
            started_at_ms: now_ms(),
            command_line: {
                let mut command_line = vec![profile.executable.clone()];
                command_line.extend(["sim".to_string(), "-c".to_string()]);
                command_line.push(config_path.display().to_string());
                command_line
            },
        };
        *self.child.lock().expect("simulation lock poisoned") = Some(sim);
        Ok(self.status())
    }

    pub(crate) fn stop(&self) -> SimulationStatus {
        if let Some(mut sim) = self.child.lock().expect("simulation lock poisoned").take() {
            let command_line = sim.command_line.clone();
            let _ = sim.child.kill();
            let _ = sim.child.wait();
            return SimulationStatus {
                running: false,
                pid: None,
                started_at_ms: None,
                message: "simulator stopped".to_string(),
                command_line,
                sim_bridge: SimBridgeCounts::default(),
            };
        }
        self.status()
    }

    pub(crate) fn restart(&self, profile: &SimulationProfile) -> std::io::Result<SimulationStatus> {
        self.stop();
        self.start(profile)
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn format_float(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn model_path_needs_reset(path: &str) -> bool {
    let path = Path::new(path);
    path.components().any(|component| {
        component.as_os_str() == ".electrode" || component.as_os_str() == ".rumoca"
    })
}

const GENERATED_RUMOCA_CONFIG: &str = r#"# Generated by Electrode Ground Station.
# Edit the SIM page, not this file. It is regenerated before Check/Start.

[rumoca]
version = "1"
task = "simulate"

[model]
file = "__MODEL_FILE__"
name = "FixedWingTrueSILFull"

[sim]
dt = __SIMULATION_DT__
mode = "__TIMING_MODE__"
solver = "rk-like"

[lockstep]
send_rate_hz = __LOCKSTEP_SEND_RATE_HZ__
receive_rate_hz = __LOCKSTEP_RECEIVE_RATE_HZ__
max_step_dt = __LOCKSTEP_MAX_STEP_DT__

# No external-interface section: the autopilot is real cubs2 hardware coupled
# purely over Zenoh. The sim never spawns a firmware process.

[transport.zenoh]
mode = "peer"
__ZENOH_ENDPOINT_LINE__

[publish]
send = "__SIM_INPUT_TOPIC__"

[subscribe]
receive = "__MANUAL_CONTROL_TOPIC__"

[transport.http]
port = 8080
scene = "__SCENE_FILE__"
asset_dir = "__ASSET_DIR__"

[transport.websocket]
port = 8081

[schema]
bfbs = [
__SCHEMA_BFBS__
]

# Cerebri publishes synapse_fbs 0.3.0 PwmSignalOutputs as the bare fixed
# struct (PwmSignalOutputsData, 48 bytes). Channel order is AETR:
# output0=aileron, output1=elevator, output2=throttle, output3=rudder.
# 0.3.0 dropped the actuator message's `armed` flag; the `armed` local keeps
# its default (false) until an armed source (e.g. vehicle_health) is routed.
[receive]
root_type = "synapse.topic.PwmSignalOutputsData"

[receive.route]
"output0_us" = { to = "local:ail_pwm" }
"output1_us" = { to = "local:elev_pwm" }
"output2_us" = { to = "local:thr_pwm" }
"output3_us" = { to = "local:rud_pwm" }

[send]
root_type = "synapse.topic.MocapFrame"

[send.route]
"frame_number" = { key = "frame" }
"rigid_bodies.0.id" = { key = "m_body_id" }
"rigid_bodies.0.position_enu_m.x" = { key = "m_px" }
"rigid_bodies.0.position_enu_m.y" = { key = "m_py" }
"rigid_bodies.0.position_enu_m.z" = { key = "m_pz" }
"rigid_bodies.0.attitude.x" = { key = "m_qx" }
"rigid_bodies.0.attitude.y" = { key = "m_qy" }
"rigid_bodies.0.attitude.z" = { key = "m_qz" }
"rigid_bodies.0.attitude.w" = { key = "m_qw" }
"rigid_bodies.0.residual" = { key = "m_residual" }
"rigid_bodies.0.tracking_valid" = { key = "m_valid" }

[locals]
armed = { type = "bool", default = false }
fmode = { type = "float", default = 0.0 }
thr_us = { type = "float", default = 0.0 }
armsw = { type = "float", default = 0.0 }
ail_pwm = { type = "float", default = 1500.0 }
elev_pwm = { type = "float", default = 1500.0 }
thr_pwm = { type = "float", default = 1000.0 }
rud_pwm = { type = "float", default = 1500.0 }
att_r = { type = "float", default = 0.0 }
att_p = { type = "float", default = 0.0 }
att_y = { type = "float", default = 0.0 }

[signals.stepper_inputs]
ail_pwm = "local:ail_pwm"
elev_pwm = "local:elev_pwm"
thr_pwm = "local:thr_pwm"
rud_pwm = "local:rud_pwm"

[signals.send]
m_px = "stepper:cer_x"
m_py = "stepper:cer_y"
m_pz = "stepper:cer_z"
m_qx = "stepper:mq_x"
m_qy = "stepper:mq_y"
m_qz = "stepper:mq_z"
m_qw = "stepper:mq_w"
m_body_id = { const = 0.0 }
m_residual = { const = 0.0 }
m_valid = { const = 1.0 }

[signals.viewer]
px = "stepper:position[1]"
py = "stepper:position[2]"
pz = "stepper:position[3]"
ail_rad = "stepper:ail_rad"
elev_rad = "stepper:elev_rad"
rud_rad = "stepper:rud_rad"
throttle = "stepper:thr_out"
airspeed = "stepper:airspeed"
alpha = "stepper:alpha_deg"
armed = "local:armed"
stick_roll = "stepper:cmd_aileron"
stick_pitch = "stepper:cmd_elevator"
stick_yaw = "stepper:cmd_rudder"
stick_throttle = "stepper:thr_out"
t = "stepper:time"
frame = "runtime:frame_num"
input_mode = "runtime:input_mode"
wall_ms = "runtime:wall_ms"
fmode = "local:fmode"
thr_us = "local:thr_us"
armsw = "local:armsw"
att_r = "local:att_r"
att_p = "local:att_p"
att_y = "local:att_y"
log_ail_pwm = "stepper:log_ail_pwm"
log_elev_pwm = "stepper:log_elev_pwm"
log_thr_pwm = "stepper:log_thr_pwm"
log_rud_pwm = "stepper:log_rud_pwm"
log_safe_ail = "stepper:log_safe_ail"
log_safe_elev = "stepper:log_safe_elev"
log_safe_rud = "stepper:log_safe_rud"
log_p = "stepper:log_p"
log_q = "stepper:log_q"
log_r = "stepper:log_r"
log_u = "stepper:log_u"
log_v = "stepper:log_v"
log_w = "stepper:log_w"
log_phi_deg = "stepper:log_phi_deg"
log_theta_deg = "stepper:log_theta_deg"
log_psi_deg = "stepper:log_psi_deg"
cer_x = "stepper:cer_x"
cer_y = "stepper:cer_y"
cer_z = "stepper:cer_z"
cer_yaw = "stepper:cer_yaw"

[signals.viewer.q0]
default = 1.0
from = "stepper:quat[1]"

[signals.viewer.q1]
default = 0.0
from = "stepper:quat[2]"

[signals.viewer.q2]
default = 0.0
from = "stepper:quat[3]"

[signals.viewer.q3]
default = 0.0
from = "stepper:quat[4]"

[reset]
on_signal = "reset"
reset_locals = true
rebuild_stepper = true

[input]
mode = "auto"

[input.keyboard.keys.r]
action = "signal"
signal = "reset"

[input.keyboard.keys.q]
action = "signal"
signal = "quit"

[viewer]
mode = "external_web"
show_armed = true
status_title = "Fixed-Wing TRUE SIL (full aero, cerebri cubs2)"

[[viewer.frame]]
name = "body"
position = ["px", "py", "pz"]
quaternion = ["q0", "q1", "q2", "q3"]

[viewer.hud]
mode = "flight"
frame = "body"
altitude = "pz"
speed = ["airspeed"]
sticks = { roll = "stick_roll", pitch = "stick_pitch", yaw = "stick_yaw", throttle = "stick_throttle" }

[[viewer.controls.keyboard]]
keys = "R"
action = "Reset (restarts cerebri too)"

[[viewer.controls.keyboard]]
keys = "C"
action = "Camera"

[[viewer.controls.keyboard]]
keys = "H"
action = "HUD"

[[viewer.controls.keyboard]]
keys = "Q"
action = "Quit"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The generated rumoca config must speak synapse_fbs 0.3.0: bare-struct
    /// PwmSignalOutputsData receive (AETR channel order), renamed mocap
    /// position field, and the selected post-arbitration PWM input topic.
    #[test]
    fn generated_config_targets_synapse_0_3_0() {
        let profile = SimulationProfile::default();
        assert_eq!(profile.command_input_topic, "synapse/motor_output");
        assert_eq!(
            profile.telemetry_output_topic,
            "synapse/mocap/rigid_body/cub1/pose"
        );

        let text = GENERATED_RUMOCA_CONFIG;
        assert!(text.contains("root_type = \"synapse.topic.PwmSignalOutputsData\""));
        // AETR: output0=aileron, output1=elevator, output2=throttle, output3=rudder.
        assert!(text.contains("\"output0_us\" = { to = \"local:ail_pwm\" }"));
        assert!(text.contains("\"output1_us\" = { to = \"local:elev_pwm\" }"));
        assert!(text.contains("\"output2_us\" = { to = \"local:thr_pwm\" }"));
        assert!(text.contains("\"output3_us\" = { to = \"local:rud_pwm\" }"));
        // 0.3.0 dropped the actuator `armed` flag; no receive route for it.
        assert!(!text.contains("\"armed\" = { to"));
        // 0.3.0 renamed MocapRigidBodySample.position -> position_enu_m.
        assert!(text.contains("\"rigid_bodies.0.position_enu_m.x\""));
        assert!(!text.contains("\"rigid_bodies.0.position.x\""));
        assert!(!text.contains("MotorOutput"));
        // The autopilot is real cubs2 hardware over Zenoh: the sim never
        // spawns a firmware process, and it paces on the wall clock.
        assert!(!text.contains("[external_interface]"));
        assert!(!text.contains("restart_external_interface"));
        assert!(!text.contains("cerebri_wrap"));
        assert_eq!(profile.timing_mode, "realtime");
    }

    /// Legacy or wrong-side topic keys in persisted profiles migrate to the
    /// selected post-arbitration PWM stream.
    #[test]
    fn normalize_migrates_legacy_topics() {
        let mut profile = SimulationProfile {
            command_input_topic: "synapse/v1/topic/pwm_signal_outputs".to_string(),
            telemetry_output_topic: "synapse/mocap/frame".to_string(),
            ..Default::default()
        };
        profile.normalize();
        assert_eq!(profile.command_input_topic, "synapse/motor_output");
        assert_eq!(
            profile.telemetry_output_topic,
            "synapse/mocap/rigid_body/cub1/pose"
        );
    }

    /// Manual helper, not part of the suite: writes the generated config (and
    /// the embedded .bfbs schemas) into the local rumoca project so it can be
    /// validated with `rumoca sim check -c <path>`. Run with:
    ///   cargo test -p electrode-ground-station -- --ignored materialize
    #[test]
    #[ignore]
    fn materialize_generated_config_for_local_rumoca() {
        let profile = SimulationProfile::default();
        let path = profile
            .write_generated_config()
            .expect("write generated rumoca config");
        eprintln!("wrote {}", path.display());
    }
}

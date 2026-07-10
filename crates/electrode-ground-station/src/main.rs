//! Electrode Ground Station daemon.
//!
//! Serves the same static Viewer bundle that GitHub Pages hosts, but on the
//! local machine and alongside a `gcs/*` HTTP API. Because the app probes
//! `gcs/health` on its own origin, being served by this daemon is exactly what
//! flips it from Viewer into Ground Station mode and unlocks the hardware
//! panels.
//!
//! Capabilities: device discovery, a live raw joystick inspector, and an
//! editable RC mapping profile applied by supervising the manual-control
//! bridge.

mod autopilot;
mod autopilot_link;
mod devices;
mod joystick;
mod mapping;
mod sim_bridge;
mod simulation;
mod supervisor;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use electrode_command_authority::{CommandAuthority, CommandAuthorityConfig};
use serde::Serialize;
use tower::{ServiceExt, service_fn};

use autopilot::AutopilotProfile;
use autopilot_link::{AutopilotLink, AutopilotRunStatus};
use mapping::MappingProfile;
use simulation::{ModelicaFile, ModelicaFileSave, SimulationProfile};
use supervisor::Supervisor;

#[derive(Parser, Debug)]
#[command(
    name = "electrode-ground-station",
    about = "Serve the electrode Viewer locally with Ground Station (hardware) capabilities"
)]
struct Cli {
    /// Address to listen on.
    #[arg(long, env = "ELECTRODE_GCS_ADDR", default_value = "127.0.0.1:8790")]
    addr: SocketAddr,

    /// Directory containing the built Viewer app (adapter-static output).
    #[arg(long, env = "ELECTRODE_GCS_WEB_DIR", default_value = "apps/web/build")]
    web_dir: PathBuf,

    /// Where the RC mapping profile is stored.
    #[arg(
        long,
        env = "ELECTRODE_GCS_MAPPING_FILE",
        default_value = "electrode-mapping.json"
    )]
    mapping_file: PathBuf,

    /// Where the autopilot stack profile is stored.
    #[arg(
        long,
        env = "ELECTRODE_GCS_AUTOPILOT_FILE",
        default_value = "electrode-autopilot.json"
    )]
    autopilot_file: PathBuf,

    /// Where the Rumoca simulation profile is stored.
    #[arg(
        long,
        env = "ELECTRODE_GCS_SIMULATION_FILE",
        default_value = "electrode-simulation.json"
    )]
    simulation_file: PathBuf,
}

struct AppState {
    mapping: RwLock<MappingProfile>,
    mapping_file: PathBuf,
    autopilot: RwLock<AutopilotProfile>,
    autopilot_file: PathBuf,
    simulation: RwLock<SimulationProfile>,
    simulation_file: PathBuf,
    sim_bridge: sim_bridge::SimBridge,
    supervisor: Supervisor,
    ppm_supervisor: Supervisor,
    autopilot_link: AutopilotLink,
    _command_authority: CommandAuthority,
}

fn should_serve_spa_fallback(path: &str) -> bool {
    let last_segment = path.rsplit('/').next().unwrap_or_default();

    !path.starts_with("/_app/") && !path.starts_with("/assets/") && !last_segment.contains('.')
}

#[cfg(test)]
mod tests {
    use super::should_serve_spa_fallback;

    #[test]
    fn spa_fallback_accepts_client_routes() {
        assert!(should_serve_spa_fallback("/"));
        assert!(should_serve_spa_fallback("/manual-control"));
        assert!(should_serve_spa_fallback("/nested/route"));
    }

    #[test]
    fn spa_fallback_rejects_asset_paths() {
        assert!(!should_serve_spa_fallback(
            "/_app/immutable/entry/start.missing.js"
        ));
        assert!(!should_serve_spa_fallback("/assets/models/missing.glb"));
        assert!(!should_serve_spa_fallback("/favicon.ico"));
    }
}

type Shared = Arc<AppState>;

#[derive(Serialize)]
struct Health {
    service: &'static str,
    version: &'static str,
    host: String,
}

async fn health() -> Json<Health> {
    Json(Health {
        service: "electrode-ground-station",
        version: env!("CARGO_PKG_VERSION"),
        host: hostname(),
    })
}

async fn devices() -> Json<devices::Devices> {
    Json(devices::list())
}

async fn get_mapping(State(state): State<Shared>) -> Json<MappingProfile> {
    Json(state.mapping.read().expect("mapping lock poisoned").clone())
}

async fn put_mapping(
    State(state): State<Shared>,
    Json(profile): Json<MappingProfile>,
) -> Result<Json<MappingProfile>, (StatusCode, String)> {
    // Persist, then relaunch the bridge if it's already running so mapping
    // edits take effect live.
    profile
        .save(&state.mapping_file)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let args = profile.bridge_args();
    let ppm_args = profile.ppm_bridge_args();
    *state.mapping.write().expect("mapping lock poisoned") = profile.clone();
    state
        .supervisor
        .restart_if_running(&args)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    state
        .ppm_supervisor
        .restart_if_running(&ppm_args)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(Json(profile))
}

async fn get_autopilot(State(state): State<Shared>) -> Json<AutopilotProfile> {
    Json(
        state
            .autopilot
            .read()
            .expect("autopilot lock poisoned")
            .clone(),
    )
}

async fn put_autopilot(
    State(state): State<Shared>,
    Json(profile): Json<AutopilotProfile>,
) -> Result<Json<AutopilotProfile>, (StatusCode, String)> {
    let profile = profile.normalized();
    profile
        .save(&state.autopilot_file)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    state.sim_bridge.set_mocap_source(profile.mocap_source);
    *state.autopilot.write().expect("autopilot lock poisoned") = profile.clone();
    Ok(Json(profile))
}

async fn autopilot_run_status(State(state): State<Shared>) -> Json<AutopilotRunStatus> {
    Json(state.autopilot_link.status())
}

async fn autopilot_start(
    State(state): State<Shared>,
) -> Result<Json<AutopilotRunStatus>, (StatusCode, String)> {
    let profile = state
        .autopilot
        .read()
        .expect("autopilot lock poisoned")
        .clone();
    // The link blocks briefly on zenoh open; keep the async runtime free.
    let link = tokio::task::block_in_place(|| {
        state
            .autopilot_link
            .start(&profile, Some(state._command_authority.vehicle_session()))
    });
    link.map(Json)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
}

async fn autopilot_stop(State(state): State<Shared>) -> Json<AutopilotRunStatus> {
    Json(tokio::task::block_in_place(|| state.autopilot_link.stop()))
}

async fn get_simulation(State(state): State<Shared>) -> Json<SimulationProfile> {
    Json(
        state
            .simulation
            .read()
            .expect("simulation lock poisoned")
            .clone(),
    )
}

async fn put_simulation(
    State(state): State<Shared>,
    Json(profile): Json<SimulationProfile>,
) -> Result<Json<SimulationProfile>, (StatusCode, String)> {
    let profile = profile.normalized();
    profile
        .save(&state.simulation_file)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    *state.simulation.write().expect("simulation lock poisoned") = profile.clone();
    Ok(Json(profile))
}

async fn simulation_model(
    State(state): State<Shared>,
) -> Result<Json<ModelicaFile>, (StatusCode, String)> {
    let profile = state
        .simulation
        .read()
        .expect("simulation lock poisoned")
        .clone();
    profile
        .read_model_file()
        .map(Json)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
}

async fn put_simulation_model(
    State(state): State<Shared>,
    Json(file): Json<ModelicaFileSave>,
) -> Result<Json<ModelicaFile>, (StatusCode, String)> {
    let profile = state
        .simulation
        .read()
        .expect("simulation lock poisoned")
        .clone();
    profile
        .save_model_file(file)
        .map(Json)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BridgeStatus {
    running: bool,
    bin: String,
    ppm_running: bool,
    ppm_bin: String,
}

async fn bridge_status(State(state): State<Shared>) -> Json<BridgeStatus> {
    let manual_running = state.supervisor.running();
    let ppm_running = state.ppm_supervisor.running();
    Json(BridgeStatus {
        running: manual_running,
        bin: state.supervisor.bin_display(),
        ppm_running,
        ppm_bin: state.ppm_supervisor.bin_display(),
    })
}

async fn bridge_start(
    State(state): State<Shared>,
) -> Result<Json<BridgeStatus>, (StatusCode, String)> {
    let args = state
        .mapping
        .read()
        .expect("mapping lock poisoned")
        .bridge_args();
    state
        .supervisor
        .start(&args)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let manual_running = state.supervisor.running();
    let ppm_running = state.ppm_supervisor.running();
    Ok(Json(BridgeStatus {
        running: manual_running,
        bin: state.supervisor.bin_display(),
        ppm_running,
        ppm_bin: state.ppm_supervisor.bin_display(),
    }))
}

async fn bridge_stop(State(state): State<Shared>) -> Json<BridgeStatus> {
    state.supervisor.stop();
    let ppm_running = state.ppm_supervisor.running();
    Json(BridgeStatus {
        running: false,
        bin: state.supervisor.bin_display(),
        ppm_running,
        ppm_bin: state.ppm_supervisor.bin_display(),
    })
}

async fn ppm_bridge_start(
    State(state): State<Shared>,
) -> Result<Json<BridgeStatus>, (StatusCode, String)> {
    let ppm_args = state
        .mapping
        .read()
        .expect("mapping lock poisoned")
        .ppm_bridge_args();
    state
        .ppm_supervisor
        .start(&ppm_args)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(Json(BridgeStatus {
        running: state.supervisor.running(),
        bin: state.supervisor.bin_display(),
        ppm_running: state.ppm_supervisor.running(),
        ppm_bin: state.ppm_supervisor.bin_display(),
    }))
}

async fn ppm_bridge_stop(State(state): State<Shared>) -> Json<BridgeStatus> {
    state.ppm_supervisor.stop();
    Json(BridgeStatus {
        running: state.supervisor.running(),
        bin: state.supervisor.bin_display(),
        ppm_running: false,
        ppm_bin: state.ppm_supervisor.bin_display(),
    })
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "electrode_ground_station=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let command_authority = CommandAuthority::start(CommandAuthorityConfig::from_env())?;
    let zenoh_listeners = command_authority.listeners().to_vec();
    let autopilot_profile = AutopilotProfile::load_or_default(&cli.autopilot_file);
    let sim_bridge = sim_bridge::SimBridge::start(
        command_authority.vehicle_session(),
        autopilot_profile.mocap_source,
    )?;

    let state: Shared = Arc::new(AppState {
        mapping: RwLock::new(MappingProfile::load_or_default(&cli.mapping_file)),
        mapping_file: cli.mapping_file.clone(),
        autopilot: RwLock::new(autopilot_profile),
        autopilot_file: cli.autopilot_file.clone(),
        simulation: RwLock::new(SimulationProfile::load_or_default(&cli.simulation_file)),
        simulation_file: cli.simulation_file.clone(),
        sim_bridge,
        supervisor: Supervisor::manual_control(),
        ppm_supervisor: Supervisor::ppm_bridge(),
        autopilot_link: AutopilotLink::new(),
        _command_authority: command_authority,
    });

    // The gcs/* API is same-origin in production; permissive CORS lets the Vite
    // dev server (a different port) probe it during development.
    let gcs = Router::new()
        .route("/health", get(health))
        .route("/devices", get(devices))
        .route("/joystick", get(joystick::joystick_ws))
        .route("/mapping", get(get_mapping).put(put_mapping))
        .route("/autopilot", get(get_autopilot).put(put_autopilot))
        .route("/autopilot/status", get(autopilot_run_status))
        .route("/autopilot/start", post(autopilot_start))
        .route("/autopilot/stop", post(autopilot_stop))
        .route("/simulation", get(get_simulation).put(put_simulation))
        .route(
            "/simulation/model",
            get(simulation_model).put(put_simulation_model),
        )
        .route("/bridge", get(bridge_status))
        .route("/bridge/start", post(bridge_start))
        .route("/bridge/stop", post(bridge_stop))
        .route("/ppm", get(bridge_status))
        .route("/ppm/start", post(ppm_bridge_start))
        .route("/ppm/stop", post(ppm_bridge_stop))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state);

    // SPA static hosting: serve built assets and fall back to index.html for
    // client-rendered routes without masking missing JS/CSS/model assets.
    let index = cli.web_dir.join("index.html");
    let static_service = tower_http::services::ServeDir::new(&cli.web_dir).fallback(service_fn(
        move |req: Request<Body>| {
            let index = index.clone();
            async move {
                if !should_serve_spa_fallback(req.uri().path()) {
                    return Ok::<_, std::convert::Infallible>(
                        StatusCode::NOT_FOUND.into_response(),
                    );
                }

                let response = match tower_http::services::ServeFile::new(index)
                    .oneshot(req)
                    .await
                {
                    Ok(response) => response.map(Body::new),
                    Err(err) => match err {},
                };
                Ok(response)
            }
        },
    ));

    let app = Router::new()
        .nest("/gcs", gcs)
        .fallback_service(static_service);

    let listener = tokio::net::TcpListener::bind(cli.addr).await?;
    tracing::info!(addr = %cli.addr, web_dir = %cli.web_dir.display(), "electrode-ground-station listening");
    println!("\n  electrode Ground Station up:");
    println!(
        "    Ground Station (viewer + hardware):  http://{}/",
        cli.addr
    );
    if !zenoh_listeners.is_empty() {
        println!(
            "    Zenoh command authority: {}\n",
            zenoh_listeners.join(", ")
        );
    }
    axum::serve(listener, app).await?;
    Ok(())
}

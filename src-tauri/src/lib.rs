mod chat;
pub use lanlm_mesh::dl;
mod cluster;
mod engine;
mod hw;
mod mesh;
mod models;
mod server;

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tauri::{AppHandle, Manager, RunEvent};

/// Shared state for every command.
pub struct AppState {
    pub http: reqwest::Client,
    /// The running llama-server child process, if a model is loaded.
    pub server: tokio::sync::Mutex<Option<server::ServerProc>>,
    /// The RPC worker (this PC lending itself to a master), if running.
    pub worker: tokio::sync::Mutex<Option<tokio::process::Child>>,
    /// Last lines printed by llama-server, for the log panel.
    pub logs: Mutex<VecDeque<String>>,
    /// Ids of downloads the user asked to cancel.
    pub cancelled: Mutex<HashSet<String>>,
    /// Set by `chat_cancel`, read by the streaming loop.
    pub chat_cancel: AtomicBool,
    /// The cluster service (device discovery, shared state, scheduling, gateway).
    pub ctl: OnceLock<lanlm_mesh::control::Controller>,
}

pub fn data_dir(app: &AppHandle) -> PathBuf {
    app.path().app_data_dir().expect("no app data directory")
}

pub fn models_dir(app: &AppHandle) -> PathBuf {
    data_dir(app).join("models")
}

pub fn engine_dir(app: &AppHandle) -> PathBuf {
    data_dir(app).join("engine")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let http = reqwest::Client::builder()
        .user_agent("LanLM/0.1")
        .connect_timeout(Duration::from_secs(15))
        .build()
        .expect("http client");

    let state = AppState {
        http,
        server: tokio::sync::Mutex::new(None),
        worker: tokio::sync::Mutex::new(None),
        logs: Mutex::new(VecDeque::new()),
        cancelled: Mutex::new(HashSet::new()),
        chat_cancel: AtomicBool::new(false),
        ctl: OnceLock::new(),
    };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            hw::hardware_info,
            engine::engine_status,
            engine::engine_install,
            models::catalog,
            models::list_installed,
            models::model_download,
            models::model_cancel,
            models::model_delete,
            models::open_models_folder,
            server::server_start,
            server::server_stop,
            server::server_status,
            server::recent_logs,
            chat::chat_stream,
            chat::chat_cancel,
            cluster::worker_start,
            cluster::worker_stop,
            cluster::worker_status,
            cluster::worker_probe,
            cluster::cluster_preview,
            mesh::mesh_status,
            mesh::mesh_create,
            mesh::mesh_join,
            mesh::mesh_leave,
            mesh::mesh_key,
            mesh::mesh_set_name,
            mesh::mesh_add_peer,
            mesh::mesh_pair_open,
            mesh::mesh_pair_close,
            mesh::mesh_pair_decide,
            mesh::mesh_pair_start,
            mesh::mesh_pair_reset,
            mesh::mesh_deploy,
            mesh::mesh_undeploy,
            mesh::mesh_preview,
            mesh::mesh_fetch,
            mesh::mesh_cancel_fetch,
            mesh::mesh_delete_model,
            mesh::mesh_bench,
            mesh::mesh_drain,
            mesh::mesh_lend,
            mesh::mesh_gateway,
            mesh::mesh_logs,
        ])
        .setup(|app| {
            // The cluster service runs for as long as the app does.
            let cfg = lanlm_mesh::control::ControllerConfig {
                data_dir: data_dir(app.handle()).join("mesh"),
                engine_dir: engine_dir(app.handle()),
                models_dir: models_dir(app.handle()),
                name: None,
                port: lanlm_mesh::mesh::DEFAULT_PORT,
                discovery: true,
                version: env!("CARGO_PKG_VERSION").to_string(),
            };
            match tauri::async_runtime::block_on(lanlm_mesh::control::Controller::start(cfg)) {
                Ok(c) => {
                    if let Some(state) = app.try_state::<AppState>() {
                        let _ = state.ctl.set(c);
                    }
                }
                // The app still works for local chat without the cluster service.
                Err(e) => eprintln!("cluster service could not start: {e}"),
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building LanLM")
        .run(|app, event| {
            // Never leave llama-server running after the window closes.
            if let RunEvent::Exit = event {
                if let Some(state) = app.try_state::<AppState>() {
                    if let Some(ctl) = state.ctl.get() {
                        let ctl = ctl.clone();
                        tauri::async_runtime::block_on(async move {
                            let _ =
                                tokio::time::timeout(Duration::from_secs(6), ctl.shutdown()).await;
                        });
                    }
                    if let Ok(mut guard) = state.server.try_lock() {
                        if let Some(mut proc_) = guard.take() {
                            let _ = proc_.child.start_kill();
                        }
                    }
                    if let Ok(mut guard) = state.worker.try_lock() {
                        if let Some(mut child) = guard.take() {
                            let _ = child.start_kill();
                        }
                    }
                }
            }
        });
}

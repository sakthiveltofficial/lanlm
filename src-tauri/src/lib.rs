mod chat;
mod cluster;
mod dl;
mod engine;
mod hw;
mod models;
mod server;

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
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
        ])
        .build(tauri::generate_context!())
        .expect("error while building LanLM")
        .run(|app, event| {
            // Never leave llama-server running after the window closes.
            if let RunEvent::Exit = event {
                if let Some(state) = app.try_state::<AppState>() {
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

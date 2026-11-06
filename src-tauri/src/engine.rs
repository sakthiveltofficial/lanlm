use std::path::PathBuf;

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::{engine_dir, AppState};
use lanlm_mesh::engine as shared;

/// Progress messages streamed to the UI while something installs or downloads.
#[derive(Clone, Serialize)]
#[serde(
    tag = "event",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Progress {
    Status { text: String },
    Bytes { done: u64, total: u64 },
    Done,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    installed: bool,
    version: Option<String>,
    path: Option<String>,
    /// Which build is installed: `cpu`, `vulkan`, `cuda-13.4`, ...
    variant: Option<String>,
    /// The build can lend this PC to a cluster (it has the RPC worker).
    has_rpc: bool,
}

/// Finds a program anywhere under `dir` by file name, ignoring case (release zips differ in layout).
pub fn find_exe(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    shared::find_exe(dir, name.trim_end_matches(".exe"))
}

pub fn find_exe_pub(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    find_exe(dir, name)
}

pub fn server_exe(app: &AppHandle) -> Option<PathBuf> {
    shared::find_exe(&engine_dir(app), "llama-server")
}

#[tauri::command]
pub fn engine_status(app: AppHandle) -> EngineStatus {
    let dir = engine_dir(&app);
    let exe = server_exe(&app);
    EngineStatus {
        installed: exe.is_some(),
        version: shared::installed_version(&dir),
        variant: shared::installed_variant(&dir),
        has_rpc: shared::find_rpc_server(&dir).is_some(),
        path: exe.map(|p| p.display().to_string()),
    }
}

/// Installs the llama.cpp build that suits this PC (its GPU, if any), or the one asked for:
/// `cpu`, `vulkan`, `cuda-13.4`, ... An empty or `auto` value chooses by hardware.
#[tauri::command]
pub async fn engine_install(
    app: AppHandle,
    state: State<'_, AppState>,
    variant: Option<String>,
    on_event: Channel<Progress>,
) -> Result<String, String> {
    // The model server and any worker must not be running while their files are replaced.
    if let Some(ctl) = state.ctl.get() {
        ctl.node().shutdown().await;
    }
    if let Some(mut p) = state.server.lock().await.take() {
        let _ = p.child.kill().await;
    }
    if let Some(mut c) = state.worker.lock().await.take() {
        let _ = c.kill().await;
    }
    let dir = engine_dir(&app);
    let done = shared::install(&state.http, &dir, variant.as_deref(), |s| {
        let _ = on_event.send(match s {
            shared::Step::Status(text) => Progress::Status { text },
            shared::Step::Bytes { done, total } => Progress::Bytes { done, total },
            shared::Step::Done => Progress::Done,
        });
    })
    .await?;
    Ok(format!("{} ({})", done.tag, done.variant))
}

#[cfg(test)]
mod tests;

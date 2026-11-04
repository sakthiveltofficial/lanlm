use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::{dl, engine_dir, AppState};

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
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Finds llama-server.exe anywhere under `dir` (release zips differ in layout).
fn find_exe(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_exe(&p, name) {
                return Some(found);
            }
        } else if p
            .file_name()
            .map(|n| n.eq_ignore_ascii_case(name))
            .unwrap_or(false)
        {
            return Some(p);
        }
    }
    None
}

pub fn find_exe_pub(dir: &Path, name: &str) -> Option<PathBuf> {
    find_exe(dir, name)
}

pub fn server_exe(app: &AppHandle) -> Option<PathBuf> {
    find_exe(&engine_dir(app), "llama-server.exe")
}

#[tauri::command]
pub fn engine_status(app: AppHandle) -> EngineStatus {
    let exe = server_exe(&app);
    let version = std::fs::read_to_string(engine_dir(&app).join("VERSION"))
        .ok()
        .map(|s| s.trim().to_string());
    EngineStatus {
        installed: exe.is_some(),
        version,
        path: exe.map(|p| p.display().to_string()),
    }
}

/// The plain-CPU 64-bit Windows build. Newer llama.cpp releases call it
/// `win-cpu-x64`; older ones `win-avx2-x64`.
fn pick_asset(assets: &[Asset]) -> Option<&Asset> {
    let candidates: Vec<&Asset> = assets
        .iter()
        .filter(|a| {
            a.name.ends_with(".zip")
                && a.name.contains("win")
                && !a.name.contains("cudart")
                && !a.name.contains("arm64")
        })
        .collect();
    for key in ["win-cpu-x64", "win-avx2-x64"] {
        if let Some(a) = candidates.iter().find(|a| a.name.contains(key)) {
            return Some(*a);
        }
    }
    None
}

#[tauri::command]
pub async fn engine_install(
    app: AppHandle,
    state: State<'_, AppState>,
    on_event: Channel<Progress>,
) -> Result<String, String> {
    let status = |t: &str| {
        let _ = on_event.send(Progress::Status { text: t.into() });
    };

    status("Looking up the latest llama.cpp release…");
    // `releases/latest` can point at a placeholder tag with no binaries, so scan
    // the most recent releases for the first one that ships a Windows CPU build.
    let releases: Vec<Release> = state
        .http
        .get("https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=15")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("Could not reach GitHub: {e}"))?
        .error_for_status()
        .map_err(|e| format!("GitHub answered with an error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Unexpected release data: {e}"))?;

    let (release, asset) = releases
        .iter()
        .find_map(|r| pick_asset(&r.assets).map(|a| (r, a)))
        .ok_or("No recent llama.cpp release has a Windows CPU build (looked for win-cpu-x64 / win-avx2-x64)")?;

    let dir = engine_dir(&app);
    let zip_path = dir.join("engine-download.zip");
    status(&format!("Downloading {} …", asset.name));
    dl::download(
        &state.http,
        &asset.browser_download_url,
        &zip_path,
        || false,
        |done, total| {
            let _ = on_event.send(Progress::Bytes { done, total });
        },
    )
    .await?;

    status("Unpacking…");
    let zip_for_task = zip_path.clone();
    let dir_for_task = dir.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let file = std::fs::File::open(&zip_for_task).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        archive.extract(&dir_for_task).map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())??;
    let _ = std::fs::remove_file(&zip_path);

    if find_exe(&dir, "llama-server.exe").is_none() {
        return Err("The download did not contain llama-server.exe".into());
    }
    let _ = std::fs::write(dir.join("VERSION"), &release.tag_name);
    let _ = on_event.send(Progress::Done);
    Ok(release.tag_name.clone())
}

#[cfg(test)]
mod tests;

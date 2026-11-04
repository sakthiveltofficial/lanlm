use std::path::PathBuf;

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};

use crate::engine::Progress;
use crate::{dl, models_dir, AppState};

struct Entry {
    id: &'static str,
    name: &'static str,
    repo: &'static str,
    file: &'static str,
    size_mb: u32,
    desc: &'static str,
    /// Vision projector file in the same repo ("" for text-only models) and its size.
    proj_file: &'static str,
    proj_mb: u32,
}

/// A starter list for ordinary laptops. Any other GGUF can be added by URL
/// or by dropping the file into the models folder.
const CATALOG: &[Entry] = &[
    Entry {
        id: "llama32-1b",
        name: "Llama 3.2 1B Instruct",
        repo: "bartowski/Llama-3.2-1B-Instruct-GGUF",
        file: "Llama-3.2-1B-Instruct-Q4_K_M.gguf",
        size_mb: 810,
        desc: "Tiny and fast. Good first test on any PC.",
        proj_file: "",
        proj_mb: 0,
    },
    Entry {
        id: "gemma3-1b",
        name: "Gemma 3 1B Instruct",
        repo: "unsloth/gemma-3-1b-it-GGUF",
        file: "gemma-3-1b-it-Q4_0.gguf",
        size_mb: 722,
        desc: "Google's compact chat model.",
        proj_file: "",
        proj_mb: 0,
    },
    Entry {
        id: "lfm25-12b",
        name: "LFM2.5 1.2B Instruct",
        repo: "LiquidAI/LFM2.5-1.2B-Instruct-GGUF",
        file: "LFM2.5-1.2B-Instruct-Q4_K_M.gguf",
        size_mb: 731,
        desc: "Liquid AI's fast on-device model.",
        proj_file: "",
        proj_mb: 0,
    },
    Entry {
        id: "llama32-3b",
        name: "Llama 3.2 3B Instruct",
        repo: "bartowski/Llama-3.2-3B-Instruct-GGUF",
        file: "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
        size_mb: 2020,
        desc: "Smarter, still comfortable on 8 GB of RAM.",
        proj_file: "",
        proj_mb: 0,
    },
    Entry {
        id: "qwen25-3b",
        name: "Qwen2.5 3B Instruct",
        repo: "bartowski/Qwen2.5-3B-Instruct-GGUF",
        file: "Qwen2.5-3B-Instruct-Q4_K_M.gguf",
        size_mb: 2100,
        desc: "Best balance of speed and quality for small PCs.",
        proj_file: "",
        proj_mb: 0,
    },
    Entry {
        id: "phi35-mini",
        name: "Phi-3.5 Mini Instruct",
        repo: "bartowski/Phi-3.5-mini-instruct-GGUF",
        file: "Phi-3.5-mini-instruct-Q4_K_M.gguf",
        size_mb: 2200,
        desc: "Microsoft's compact reasoning model.",
        proj_file: "",
        proj_mb: 0,
    },
    Entry {
        id: "qwen25-7b",
        name: "Qwen2.5 7B Instruct",
        repo: "bartowski/Qwen2.5-7B-Instruct-GGUF",
        file: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
        size_mb: 4680,
        desc: "Stronger answers. Wants 16 GB of RAM.",
        proj_file: "",
        proj_mb: 0,
    },
    // Vision models: the model plus a "projector" file that lets it read images.
    Entry {
        id: "lfm25-vl",
        name: "LFM2.5-VL 1.6B (vision)",
        repo: "LiquidAI/LFM2.5-VL-1.6B-GGUF",
        file: "LFM2.5-VL-1.6B-Q4_K_M.gguf",
        size_mb: 731,
        desc: "Small vision model. Describes photos and reads text in images.",
        proj_file: "mmproj-LFM2.5-VL-1.6b-Q8_0.gguf",
        proj_mb: 583,
    },
    Entry {
        id: "qwen2vl-2b",
        name: "Qwen2-VL 2B Instruct (vision)",
        repo: "ggml-org/Qwen2-VL-2B-Instruct-GGUF",
        file: "Qwen2-VL-2B-Instruct-Q4_K_M.gguf",
        size_mb: 986,
        desc: "Describe photos, read text and screenshots.",
        proj_file: "mmproj-Qwen2-VL-2B-Instruct-Q8_0.gguf",
        proj_mb: 710,
    },
    Entry {
        id: "gemma3-4b-vl",
        name: "Gemma 3 4B Instruct (vision)",
        repo: "ggml-org/gemma-3-4b-it-GGUF",
        file: "gemma-3-4b-it-Q4_K_M.gguf",
        size_mb: 2500,
        desc: "Stronger image understanding. Wants about 8 GB of RAM.",
        proj_file: "mmproj-model-f16.gguf",
        proj_mb: 851,
    },
];

/// The local file name a model's projector is saved under: `<model>.mmproj.gguf`.
pub fn proj_name(model_file: &str) -> String {
    format!(
        "{}.mmproj.gguf",
        model_file
            .trim_end_matches(".gguf")
            .trim_end_matches(".GGUF")
    )
}

/// The projector sitting next to `model_file`, if it was downloaded.
pub fn sibling_proj(app: &AppHandle, model_file: &str) -> Option<PathBuf> {
    let p = models_dir(app).join(proj_name(model_file));
    p.exists().then_some(p)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogItem {
    id: &'static str,
    name: &'static str,
    file: &'static str,
    url: String,
    size_mb: u32,
    desc: &'static str,
    vision: bool,
    proj_url: Option<String>,
    /// Name the projector is saved as locally.
    proj_file: Option<String>,
    proj_mb: u32,
}

#[tauri::command]
pub fn catalog() -> Vec<CatalogItem> {
    CATALOG
        .iter()
        .map(|e| {
            let vision = !e.proj_file.is_empty();
            CatalogItem {
                id: e.id,
                name: e.name,
                file: e.file,
                url: format!("https://huggingface.co/{}/resolve/main/{}", e.repo, e.file),
                size_mb: e.size_mb,
                desc: e.desc,
                vision,
                proj_url: vision.then(|| {
                    format!(
                        "https://huggingface.co/{}/resolve/main/{}",
                        e.repo, e.proj_file
                    )
                }),
                proj_file: vision.then(|| proj_name(e.file)),
                proj_mb: e.proj_mb,
            }
        })
        .collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    pub file: String,
    pub size_mb: u64,
    /// A projector is installed next to it, so it can read images.
    pub vision: bool,
}

#[tauri::command]
pub fn list_installed(app: AppHandle) -> Vec<Installed> {
    let dir = models_dir(&app);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let lower = name.to_lowercase();
            // Projector files belong to their model and are not listed on their own.
            if lower.ends_with(".gguf") && !lower.ends_with(".mmproj.gguf") {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                out.push(Installed {
                    vision: dir.join(proj_name(&name)).exists(),
                    file: name,
                    size_mb: size / (1024 * 1024),
                });
            }
        }
    }
    out.sort_by_key(|m| m.file.to_lowercase());
    out
}

/// A file name that cannot escape the models folder.
pub fn safe_name(name: &str) -> Result<String, String> {
    let ok = !name.is_empty()
        && name.to_lowercase().ends_with(".gguf")
        && !name.contains(['/', '\\', ':', '\0'])
        && !name.contains("..");
    if ok {
        Ok(name.to_string())
    } else {
        Err("The file name must end in .gguf and contain no folders".into())
    }
}

#[tauri::command]
pub async fn model_download(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    url: String,
    file: String,
    on_event: Channel<Progress>,
) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("Only https:// links can be downloaded".into());
    }
    let file = safe_name(&file)?;
    let dest = models_dir(&app).join(&file);
    state.cancelled.lock().unwrap().remove(&id);

    let result = dl::download(
        &state.http,
        &url,
        &dest,
        || state.cancelled.lock().unwrap().contains(&id),
        |done, total| {
            let _ = on_event.send(Progress::Bytes { done, total });
        },
    )
    .await;

    state.cancelled.lock().unwrap().remove(&id);
    result?;
    let _ = on_event.send(Progress::Done);
    Ok(())
}

#[tauri::command]
pub fn model_cancel(state: State<'_, AppState>, id: String) {
    state.cancelled.lock().unwrap().insert(id);
}

#[tauri::command]
pub fn model_delete(app: AppHandle, file: String) -> Result<(), String> {
    let file = safe_name(&file)?;
    let dir = models_dir(&app);
    for name in [file.clone(), proj_name(&file)] {
        let path = dir.join(&name);
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
        }
        // Also drop half-finished downloads of the same files.
        let _ = std::fs::remove_file(dir.join(format!("{name}.part")));
    }
    Ok(())
}

#[tauri::command]
pub fn open_models_folder(app: AppHandle) -> Result<(), String> {
    let dir = models_dir(&app);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer")
        .arg(dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests;

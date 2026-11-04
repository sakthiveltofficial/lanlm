use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};

use crate::cluster::{self, WorkerCfg};
use crate::models::safe_name;
use crate::{engine, models_dir, AppState};

pub struct ServerProc {
    pub child: Child,
    pub port: u16,
    pub model: String,
    /// API key llama-server requires, if one is set.
    pub key: Option<String>,
    /// How the model was split across PCs, and the API address, kept for status calls.
    pub note: Option<String>,
    pub api_url: Option<String>,
    /// The model was loaded with a vision projector and accepts images.
    pub vision: bool,
}

/// Sharing the loaded model as an OpenAI-compatible API.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiCfg {
    port: u16,
    /// Listen on the whole network instead of this PC only.
    network: bool,
    key: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    running: bool,
    port: Option<u16>,
    model: Option<String>,
    /// How the model was split across PCs, when a cluster was used.
    note: Option<String>,
    /// Base URL other apps can use (OpenAI style), when the API is shared.
    api_url: Option<String>,
    /// Images can be attached (a vision projector is loaded).
    vision: bool,
}

fn pump_lines<R: AsyncRead + Unpin + Send + 'static>(app: AppHandle, reader: R) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(state) = app.try_state::<AppState>() {
                let mut logs = state.logs.lock().unwrap();
                logs.push_back(line.clone());
                while logs.len() > 500 {
                    logs.pop_front();
                }
            }
            let _ = app.emit("engine-log", line);
        }
    });
}

fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    Ok(listener.local_addr().map_err(|e| e.to_string())?.port())
}

async fn stop_inner(state: &AppState) {
    let proc_ = state.server.lock().await.take();
    if let Some(mut p) = proc_ {
        let _ = p.child.kill().await;
    }
}

// A Tauri command receives its arguments flat from the UI, so the list is long on purpose.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn server_start(
    app: AppHandle,
    state: State<'_, AppState>,
    model: String,
    ctx: u32,
    threads: u32,
    parallel: u32,
    workers: Vec<WorkerCfg>,
    force_share: bool,
    api: Option<ApiCfg>,
) -> Result<ServerStatus, String> {
    let model = safe_name(&model)?;
    let model_path = models_dir(&app).join(&model);
    if !model_path.exists() {
        return Err("That model file is not downloaded".into());
    }
    let exe = engine::server_exe(&app).ok_or("The engine is not installed yet")?;

    stop_inner(&state).await;
    state.logs.lock().unwrap().clear();

    // Cluster: llama.cpp aborts the whole process if a worker is unreachable,
    // so check each one first and explain which is missing.
    let mut plan = cluster::Plan {
        args: vec![],
        note: String::new(),
    };
    if !workers.is_empty() {
        for w in &workers {
            let ep = w.endpoint.clone();
            let ok = tokio::task::spawn_blocking(move || cluster::probe(&ep))
                .await
                .unwrap_or(false);
            if !ok {
                return Err(format!("Can't reach the worker at {}. Is it running, on the same network, and not blocked by a firewall?", w.endpoint));
            }
        }
        plan = cluster::plan(
            &model_path,
            &workers,
            force_share,
            cluster::master_capacity_mb(),
        )?;
    }

    // Normally a private random port on this PC. When the API is shared it is the
    // chosen port, and a network listener must have a key (anyone could reach it).
    let (host, port, key) = match &api {
        Some(a) => {
            let key = a.key.trim().to_string();
            if a.network && key.is_empty() {
                return Err(
                    "Sharing on the network needs an API key. Create one in Settings.".into(),
                );
            }
            let ep = format!("127.0.0.1:{}", a.port);
            if tokio::task::spawn_blocking(move || cluster::probe(&ep))
                .await
                .unwrap_or(false)
            {
                return Err(format!(
                    "Port {} is already in use. Pick another API port in Settings.",
                    a.port
                ));
            }
            (
                if a.network { "0.0.0.0" } else { "127.0.0.1" },
                a.port,
                Some(key).filter(|k| !k.is_empty()),
            )
        }
        None => ("127.0.0.1", free_port()?, None),
    };
    let proj = crate::models::sibling_proj(&app, &model);
    let mut cmd = Command::new(&exe);
    cmd.current_dir(exe.parent().unwrap_or(std::path::Path::new(".")))
        .arg("-m")
        .arg(&model_path)
        .arg("--host")
        .arg(host)
        .arg("--port")
        .arg(port.to_string())
        .arg("-c")
        .arg(ctx.max(512).to_string())
        .arg("-t")
        .arg(threads.max(1).to_string())
        .arg("-np")
        .arg(parallel.max(1).to_string())
        .args(&plan.args)
        .args(
            proj.iter()
                .flat_map(|p| ["--mmproj".to_string(), p.display().to_string()]),
        )
        .args(
            key.iter()
                .flat_map(|k| ["--api-key".to_string(), k.clone()]),
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Could not start llama-server: {e}"))?;
    if let Some(out) = child.stdout.take() {
        pump_lines(app.clone(), out);
    }
    if let Some(err) = child.stderr.take() {
        pump_lines(app.clone(), err);
    }

    // Wait until /health says the model is loaded (503 while loading).
    let health = format!("http://127.0.0.1:{port}/health");
    for _ in 0..600 {
        if let Ok(Some(status)) = child.try_wait() {
            let tail: Vec<String> = state
                .logs
                .lock()
                .unwrap()
                .iter()
                .rev()
                .take(12)
                .cloned()
                .collect();
            let tail: Vec<String> = tail.into_iter().rev().collect();
            return Err(format!(
                "llama-server exited ({status}) while loading.\n{}",
                tail.join("\n")
            ));
        }
        if let Ok(r) = state
            .http
            .get(&health)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            if r.status().is_success() {
                let api_url = api.as_ref().map(|a| {
                    let addr = if a.network {
                        cluster::lan_ip().unwrap_or_else(|| "127.0.0.1".into())
                    } else {
                        "127.0.0.1".into()
                    };
                    format!("http://{addr}:{port}/v1")
                });
                let note = if plan.note.is_empty() {
                    None
                } else {
                    Some(plan.note.clone())
                };
                let vision = proj.is_some();
                *state.server.lock().await = Some(ServerProc {
                    child,
                    port,
                    model: model.clone(),
                    key,
                    note: note.clone(),
                    api_url: api_url.clone(),
                    vision,
                });
                return Ok(ServerStatus {
                    running: true,
                    port: Some(port),
                    model: Some(model),
                    note,
                    api_url,
                    vision,
                });
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = child.kill().await;
    Err("Timed out waiting for the model to load (5 minutes)".into())
}

#[tauri::command]
pub async fn server_stop(state: State<'_, AppState>) -> Result<(), String> {
    stop_inner(&state).await;
    Ok(())
}

#[tauri::command]
pub async fn server_status(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    let guard = state.server.lock().await;
    Ok(match guard.as_ref() {
        Some(p) => ServerStatus {
            running: true,
            port: Some(p.port),
            model: Some(p.model.clone()),
            note: p.note.clone(),
            api_url: p.api_url.clone(),
            vision: p.vision,
        },
        None => ServerStatus {
            running: false,
            port: None,
            model: None,
            note: None,
            api_url: None,
            vision: false,
        },
    })
}

#[tauri::command]
pub fn recent_logs(state: State<'_, AppState>) -> Vec<String> {
    state.logs.lock().unwrap().iter().cloned().collect()
}

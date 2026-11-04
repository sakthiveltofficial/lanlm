//! Cluster: other PCs lend memory and CPU through llama.cpp's RPC backend.
//!
//! A *worker* runs `ggml-rpc-server.exe`. The *master* starts llama-server with
//! `--rpc host:port,...`; llama.cpp then offloads the model's last layers to the
//! workers (they appear as remote devices). The planner below decides how many.

use std::io::{BufReader, Read, Seek, SeekFrom};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tokio::process::Command;

use crate::models::safe_name;
use crate::{engine_dir, models_dir, AppState};

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WorkerCfg {
    /// "192.168.1.20:50052"
    pub endpoint: String,
    /// Free RAM the worker PC can lend, in MB.
    pub ram_mb: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerStatus {
    running: bool,
    port: Option<u16>,
    /// Addresses other PCs can type to reach this worker.
    addresses: Vec<String>,
    lan: bool,
    /// What another PC pastes to add this one: "192.168.1.5:50052@5120".
    pairing: Option<String>,
    ram_mb: u64,
}

pub struct Plan {
    pub args: Vec<String>,
    pub note: String,
}

// ───────────────────────── GGUF header: layer count ─────────────────────────

fn rd<const N: usize, R: Read>(r: &mut R) -> std::io::Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}
fn rd_u32<R: Read>(r: &mut R) -> std::io::Result<u32> {
    Ok(u32::from_le_bytes(rd(r)?))
}
fn rd_u64<R: Read>(r: &mut R) -> std::io::Result<u64> {
    Ok(u64::from_le_bytes(rd(r)?))
}

fn scalar_size(t: u32) -> Option<i64> {
    Some(match t {
        0 | 1 | 7 => 1,
        2 | 3 => 2,
        4..=6 => 4,
        10..=12 => 8,
        _ => return None,
    })
}

/// Skips a value of GGUF type `t` (strings and arrays included).
fn skip_value<R: Read + Seek>(r: &mut R, t: u32) -> std::io::Result<()> {
    if let Some(n) = scalar_size(t) {
        r.seek(SeekFrom::Current(n))?;
    } else if t == 8 {
        let len = rd_u64(r)?;
        r.seek(SeekFrom::Current(len as i64))?;
    } else if t == 9 {
        let et = rd_u32(r)?;
        let n = rd_u64(r)?;
        if let Some(sz) = scalar_size(et) {
            r.seek(SeekFrom::Current(sz * n as i64))?;
        } else {
            for _ in 0..n {
                skip_value(r, et)?;
            }
        }
    } else {
        return Err(std::io::Error::other("unknown GGUF value type"));
    }
    Ok(())
}

/// The model's transformer block count, read from the GGUF metadata.
pub fn block_count(path: &Path) -> Result<u32, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut r = BufReader::with_capacity(1 << 16, f);
    let io = |e: std::io::Error| format!("Could not read the model header: {e}");
    if &rd::<4, _>(&mut r).map_err(io)? != b"GGUF" {
        return Err("Not a GGUF file".into());
    }
    let _version = rd_u32(&mut r).map_err(io)?;
    let _tensors = rd_u64(&mut r).map_err(io)?;
    let kv = rd_u64(&mut r).map_err(io)?;

    let mut arch: Option<String> = None;
    let mut blocks: Vec<(String, u32)> = Vec::new();
    for _ in 0..kv {
        let klen = rd_u64(&mut r).map_err(io)? as usize;
        let mut key = vec![0u8; klen];
        r.read_exact(&mut key).map_err(io)?;
        let key = String::from_utf8_lossy(&key).to_string();
        let t = rd_u32(&mut r).map_err(io)?;
        if key == "general.architecture" && t == 8 {
            let l = rd_u64(&mut r).map_err(io)? as usize;
            let mut s = vec![0u8; l];
            r.read_exact(&mut s).map_err(io)?;
            arch = Some(String::from_utf8_lossy(&s).to_string());
        } else if key.ends_with(".block_count") && (t == 4 || t == 5) {
            blocks.push((key, rd_u32(&mut r).map_err(io)?));
        } else {
            skip_value(&mut r, t).map_err(io)?;
        }
        if let Some(a) = &arch {
            if let Some((_, n)) = blocks
                .iter()
                .find(|(k, _)| *k == format!("{a}.block_count"))
            {
                return Ok(*n);
            }
        }
    }
    Err("The model header has no block_count".into())
}

// ───────────────────────── planning ─────────────────────────

pub fn probe(endpoint: &str) -> bool {
    endpoint
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
        .map(|addr: SocketAddr| TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok())
        .unwrap_or(false)
}

/// How the model's layers are divided between this PC and the workers.
struct Split {
    n_layer: u32,
    file_mb: f64,
    need_mb: f64,
    local_layers: u32,
    /// Layers per worker, in the same order as the workers.
    remote_layers: Vec<u32>,
    caps: Vec<f64>,
    uses_workers: bool,
    note: String,
    error: Option<String>,
}

/// Spreads `total` whole layers over `weights` (largest remainder).
fn spread(total: u32, weights: &[f64]) -> Vec<u32> {
    let sum: f64 = weights.iter().sum();
    if weights.is_empty() || sum <= 0.0 {
        return vec![0; weights.len()];
    }
    let exact: Vec<f64> = weights.iter().map(|w| total as f64 * w / sum).collect();
    let mut out: Vec<u32> = exact.iter().map(|x| x.floor() as u32).collect();
    let mut left = total - out.iter().sum::<u32>();
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by(|&a, &b| {
        (exact[b] - exact[b].floor())
            .partial_cmp(&(exact[a] - exact[a].floor()))
            .unwrap()
    });
    for i in order {
        if left == 0 {
            break;
        }
        out[i] += 1;
        left -= 1;
    }
    out
}

/// The master fills first (those layers need no network hop); the rest is spread
/// over the workers by free memory. With `force_share` the model is spread over
/// every PC by memory instead.
fn split(
    model: &Path,
    workers: &[WorkerCfg],
    force_share: bool,
    master_cap_mb: f64,
) -> Result<Split, String> {
    let file_mb = std::fs::metadata(model).map_err(|e| e.to_string())?.len() as f64 / 1048576.0;
    let n_layer = block_count(model)?;
    Ok(split_sizes(
        file_mb,
        n_layer,
        workers,
        force_share,
        master_cap_mb,
    ))
}

/// The planning itself, a pure function of sizes (so it can be tested without model files).
fn split_sizes(
    file_mb: f64,
    n_layer: u32,
    workers: &[WorkerCfg],
    force_share: bool,
    master_cap_mb: f64,
) -> Split {
    let need = file_mb * 1.1; // weights + KV cache + working buffers
    let caps: Vec<f64> = workers.iter().map(|w| w.ram_mb as f64 * 0.75).collect();
    let remote_cap: f64 = caps.iter().sum();
    let total = master_cap_mb + remote_cap;
    let all_local = |note: &str, error: Option<String>| Split {
        n_layer,
        file_mb,
        need_mb: need,
        local_layers: n_layer,
        remote_layers: vec![0; workers.len()],
        caps: caps.clone(),
        uses_workers: false,
        note: note.to_string(),
        error,
    };

    if need > total {
        return all_local("", Some(format!(
            "This model needs about {:.1} GB, but this PC and its {} worker(s) can lend about {:.1} GB together. Add a worker, close other programs, or pick a smaller model.",
            need / 1024.0, workers.len(), total / 1024.0
        )));
    }
    if workers.is_empty() {
        return all_local("", None);
    }
    if !force_share && need <= master_cap_mb {
        return all_local("The model fits on this PC, so the workers are not used (faster). Turn on “Use workers even when it fits” to share it anyway.", None);
    }

    let local_mb = if force_share {
        need * master_cap_mb / total
    } else {
        master_cap_mb.min(need)
    };
    let local_layers = ((n_layer as f64) * local_mb / need)
        .round()
        .clamp(0.0, n_layer as f64) as u32;
    if local_layers >= n_layer {
        return all_local("All layers stay on this PC.", None);
    }
    let remote_layers = spread(n_layer - local_layers, &caps);
    let remote_mb = file_mb * (n_layer - local_layers) as f64 / n_layer as f64;
    Split {
        n_layer,
        file_mb,
        need_mb: need,
        local_layers,
        remote_layers,
        caps,
        uses_workers: true,
        note: format!(
            "{local_layers} of {n_layer} layers on this PC, the rest ({:.1} GB) on {} worker(s).",
            remote_mb / 1024.0,
            workers.len()
        ),
        error: None,
    }
}

/// llama-server arguments for the plan, or none when nothing is offloaded.
pub fn plan(
    model: &Path,
    workers: &[WorkerCfg],
    force_share: bool,
    master_cap_mb: f64,
) -> Result<Plan, String> {
    if workers.is_empty() {
        return Ok(Plan {
            args: vec![],
            note: String::new(),
        });
    }
    plan_from_split(split(model, workers, force_share, master_cap_mb)?, workers)
}

/// Turns a split into llama-server arguments (an error when the model cannot fit).
fn plan_from_split(s: Split, workers: &[WorkerCfg]) -> Result<Plan, String> {
    if let Some(e) = s.error {
        return Err(e);
    }
    if !s.uses_workers {
        return Ok(Plan {
            args: vec![],
            note: s.note,
        });
    }
    // llama.cpp offloads the *last* n layers; +1 counts the output layer, which
    // is pinned back to this PC below.
    let n_gpu = s.n_layer + 1 - s.local_layers;
    let remote_cap: f64 = s.caps.iter().sum();
    let ts: Vec<String> = s
        .caps
        .iter()
        .map(|c| format!("{:.3}", c / remote_cap))
        .collect();
    let args = vec![
        "--rpc".into(),
        workers
            .iter()
            .map(|w| w.endpoint.clone())
            .collect::<Vec<_>>()
            .join(","),
        "-ngl".into(),
        n_gpu.to_string(),
        "-ts".into(),
        ts.join(","),
        // The output matrix is huge and its result (a score per vocabulary entry)
        // would cross the network on every token, so keep it on this PC.
        "-ot".into(),
        r"^(output|token_embd)\.weight$=CPU".into(),
    ];
    Ok(Plan { args, note: s.note })
}

// ───────────────────────── preview for the UI ─────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevicePlan {
    name: String,
    kind: &'static str,
    cap_mb: f64,
    layers: u32,
    mb: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    n_layer: u32,
    file_mb: f64,
    need_mb: f64,
    uses_workers: bool,
    devices: Vec<DevicePlan>,
    note: String,
    error: Option<String>,
}

pub fn master_capacity_mb() -> f64 {
    // The master also runs the app and the KV cache, so it offers less of its own memory.
    crate::hw::memory_mb().1 as f64 * 0.7
}

/// What loading `model` would do with these workers, without loading it.
#[tauri::command]
pub fn cluster_preview(
    app: AppHandle,
    model: String,
    workers: Vec<WorkerCfg>,
    force_share: bool,
) -> Result<Preview, String> {
    let model = safe_name(&model)?;
    let path = models_dir(&app).join(&model);
    if !path.exists() {
        return Err("That model is not downloaded".into());
    }
    let master_cap = master_capacity_mb();
    let s = split(&path, &workers, force_share, master_cap)?;
    let per_layer = s.file_mb / s.n_layer.max(1) as f64;
    let mut devices = vec![DevicePlan {
        name: "This PC".into(),
        kind: "master",
        cap_mb: master_cap,
        layers: s.local_layers,
        mb: per_layer * s.local_layers as f64,
    }];
    for (i, w) in workers.iter().enumerate() {
        devices.push(DevicePlan {
            name: w.endpoint.clone(),
            kind: "worker",
            cap_mb: s.caps[i],
            layers: s.remote_layers[i],
            mb: per_layer * s.remote_layers[i] as f64,
        });
    }
    Ok(Preview {
        n_layer: s.n_layer,
        file_mb: s.file_mb,
        need_mb: s.need_mb,
        uses_workers: s.uses_workers,
        devices,
        note: s.note,
        error: s.error,
    })
}

// ───────────────────────── worker role ─────────────────────────

/// This PC's address on the local network, if it has one.
pub fn lan_ip() -> Option<String> {
    // Connecting a UDP socket sends nothing; it only asks the OS which local
    // address would be used to reach the network.
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect("192.0.2.1:9").map(|_| s))
        .and_then(|s| s.local_addr())
        .map(|a| a.ip().to_string())
        .ok()
}

fn lan_addresses(port: u16) -> Vec<String> {
    lan_ip()
        .into_iter()
        .map(|ip| format!("{ip}:{port}"))
        .collect()
}

fn lendable_ram_mb() -> u64 {
    (crate::hw::memory_mb().1 as f64 * 0.8) as u64
}

#[tauri::command]
pub async fn worker_start(
    app: AppHandle,
    state: State<'_, AppState>,
    port: u16,
    threads: u32,
    lan: bool,
) -> Result<WorkerStatus, String> {
    worker_stop_inner(&state).await;
    let exe = engine_dir(&app);
    let exe = crate::engine::find_exe_pub(&exe, "ggml-rpc-server.exe")
        .or_else(|| crate::engine::find_exe_pub(&exe, "rpc-server.exe"))
        .ok_or("This engine build has no RPC server. Update the engine in Settings.")?;
    let mut cmd = Command::new(&exe);
    cmd.current_dir(exe.parent().unwrap_or(Path::new(".")))
        .arg("-H")
        .arg(if lan { "0.0.0.0" } else { "127.0.0.1" })
        .arg("-p")
        .arg(port.to_string())
        .arg("-t")
        .arg(threads.max(1).to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Could not start the worker: {e}"))?;

    // Give it a moment to bind, then make sure it is still alive and listening.
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "The worker exited right away ({status}). Is port {port} already in use?"
            ));
        }
        if probe(&format!("127.0.0.1:{port}")) {
            *state.worker.lock().await = Some(child);
            let ram = lendable_ram_mb();
            let addresses = lan_addresses(port);
            let pairing = if lan {
                addresses.first().map(|a| format!("{a}@{ram}"))
            } else {
                Some(format!("127.0.0.1:{port}@{ram}"))
            };
            return Ok(WorkerStatus {
                running: true,
                port: Some(port),
                addresses,
                lan,
                pairing,
                ram_mb: ram,
            });
        }
    }
    let _ = child.kill().await;
    Err("The worker did not start listening in time".into())
}

async fn worker_stop_inner(state: &AppState) {
    if let Some(mut c) = state.worker.lock().await.take() {
        let _ = c.kill().await;
    }
}

#[tauri::command]
pub async fn worker_stop(state: State<'_, AppState>) -> Result<(), String> {
    worker_stop_inner(&state).await;
    Ok(())
}

#[tauri::command]
pub async fn worker_status(state: State<'_, AppState>) -> Result<WorkerStatus, String> {
    let running = state.worker.lock().await.is_some();
    Ok(WorkerStatus {
        running,
        port: None,
        addresses: vec![],
        lan: false,
        pairing: None,
        ram_mb: lendable_ram_mb(),
    })
}

#[tauri::command]
pub async fn worker_probe(endpoint: String) -> bool {
    tokio::task::spawn_blocking(move || probe(&endpoint))
        .await
        .unwrap_or(false)
}

#[cfg(test)]
mod tests;

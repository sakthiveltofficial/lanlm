//! What this machine can do: CPU, memory, GPUs and how fast its memory is.
//!
//! GPUs are read from the engine itself (`llama-server --list-devices`), because that is the
//! list of devices llama.cpp can actually use. Before an engine is installed, `nvidia-smi` and
//! the operating system's adapter list say what to download.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sysinfo::System;

use crate::proto::{GpuInfo, HwSpec};

const MB: u64 = 1024 * 1024;

/// Runs a program without flashing a console window, and gives up after `wait`.
/// Returns stdout and stderr joined, or `None` if it could not run or timed out.
pub fn run_capture(program: &Path, args: &[&str], wait: Duration) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    if let Some(dir) = program.parent().filter(|d| d.is_dir()) {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > wait => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => return None,
        }
    }
    let out = child.wait_with_output().ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(text)
}

fn backend_of(device: &str) -> Option<&'static str> {
    let d = device.to_ascii_lowercase();
    if d.starts_with("cuda") {
        Some("cuda")
    } else if d.starts_with("vulkan") {
        Some("vulkan")
    } else if d.starts_with("rocm") || d.starts_with("hip") {
        Some("rocm")
    } else if d.starts_with("metal") || d.starts_with("mtl") {
        Some("metal")
    } else if d.starts_with("sycl") || d.starts_with("level") {
        Some("sycl")
    } else if d.starts_with("opencl") {
        Some("opencl")
    } else {
        None // CPU, RPC and anything unknown are not local accelerators
    }
}

/// Reads the device list that `llama-server --list-devices` prints, for example
/// `  CUDA0: NVIDIA GeForce RTX 5060 Ti (16311 MiB, 15111 MiB free)`.
pub fn parse_list_devices(text: &str) -> Vec<GpuInfo> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((device, rest)) = line.split_once(':') else {
            continue;
        };
        let device = device.trim();
        let Some(backend) = backend_of(device) else {
            continue;
        };
        if device.contains(' ') {
            continue;
        }
        let rest = rest.trim();
        let (name, mem) = match rest.rfind('(') {
            Some(i) => (rest[..i].trim(), &rest[i + 1..]),
            None => (rest, ""),
        };
        let mut nums = mem
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse::<u64>().ok());
        let total = nums.next().unwrap_or(0);
        let free = nums.next().unwrap_or(total);
        out.push(GpuInfo {
            device: device.to_string(),
            name: name.to_string(),
            backend: backend.to_string(),
            vram_total_mb: total,
            vram_free_mb: free.min(total.max(free)),
        });
    }
    out
}

/// Reads `nvidia-smi --query-gpu=name,memory.total,memory.free --format=csv,noheader,nounits`.
pub fn parse_nvidia_smi(text: &str) -> Vec<GpuInfo> {
    let mut out = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() < 3 {
            continue;
        }
        let (Ok(total), Ok(free)) = (parts[1].parse::<u64>(), parts[2].parse::<u64>()) else {
            continue;
        };
        out.push(GpuInfo {
            device: format!("CUDA{}", out.len()),
            name: parts[0].to_string(),
            backend: "cuda".into(),
            vram_total_mb: total,
            vram_free_mb: free,
        });
    }
    out
}

fn nvidia_smi() -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) {
        "nvidia-smi.exe"
    } else {
        "nvidia-smi"
    };
    let mut candidates = vec![std::path::PathBuf::from(name)];
    if cfg!(windows) {
        candidates.push(r"C:\Windows\System32\nvidia-smi.exe".into());
        candidates.push(r"C:\Program Files\NVIDIA Corporation\NVSMI\nvidia-smi.exe".into());
    }
    candidates.into_iter().find(|p| {
        run_capture(p, &["-L"], Duration::from_secs(8)).is_some_and(|t| t.contains("GPU"))
    })
}

/// NVIDIA GPUs and the driver version, from `nvidia-smi`.
pub fn nvidia_gpus() -> (Vec<GpuInfo>, Option<String>) {
    let Some(exe) = nvidia_smi() else {
        return (vec![], None);
    };
    let gpus = run_capture(
        &exe,
        &[
            "--query-gpu=name,memory.total,memory.free",
            "--format=csv,noheader,nounits",
        ],
        Duration::from_secs(8),
    )
    .map(|t| parse_nvidia_smi(&t))
    .unwrap_or_default();
    let driver = run_capture(
        &exe,
        &["--query-gpu=driver_version", "--format=csv,noheader"],
        Duration::from_secs(8),
    )
    .and_then(|t| t.lines().next().map(|l| l.trim().to_string()))
    .filter(|s| !s.is_empty());
    (gpus, driver)
}

/// Total and available memory in MB; `(0, 0)` when the operating system will not say.
pub fn memory_mb() -> (u64, u64) {
    let Ok(mut sys) = System::new() else {
        return (0, 0);
    };
    sys.refresh_memory();
    (sys.total_memory() / MB, sys.available_memory() / MB)
}

/// Measures how fast this machine can stream memory, in GB/s. Generating a token reads every
/// weight once, so this is what limits token speed on a CPU.
pub fn mem_bandwidth_gbps() -> f64 {
    const SIZE: usize = 96 * 1024 * 1024;
    let src = vec![1u8; SIZE];
    let mut dst = vec![0u8; SIZE];
    dst.copy_from_slice(&src); // fault the pages in before timing
    let mut best = 0.0f64;
    for _ in 0..4 {
        let t = Instant::now();
        dst.copy_from_slice(&src);
        std::hint::black_box(&dst);
        let secs = t.elapsed().as_secs_f64().max(1e-6);
        best = best.max(SIZE as f64 / secs / 1e9);
    }
    // A copy both reads and writes; only the read side limits decoding.
    (best * 10.0).round() / 10.0
}

/// A full hardware reading. Slow (it starts the engine): call it from a blocking thread, and
/// not more often than every few seconds. `engine_exe` is llama-server, if installed.
pub fn sample(engine_exe: Option<&Path>, measure_bw: bool) -> HwSpec {
    let (total, free) = memory_mb();
    let mut spec = HwSpec {
        ram_total_mb: total,
        ram_free_mb: free,
        ..Default::default()
    };
    if let Ok(mut sys) = System::new() {
        sys.refresh_cpu_all();
        spec.cores = System::physical_core_count()
            .map(|n| n as u32)
            .unwrap_or(sys.cpus().len() as u32)
            .max(1);
        spec.cpu = sys
            .cpus()
            .first()
            .map(|c| c.brand().trim().to_string())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "Unknown CPU".into());
    }
    if measure_bw {
        spec.mem_bw_gbps = mem_bandwidth_gbps();
    }
    spec.gpus = gpus(engine_exe);
    spec
}

/// Usable GPUs: what the installed engine reports, or NVIDIA's own tool before an engine exists.
pub fn gpus(engine_exe: Option<&Path>) -> Vec<GpuInfo> {
    if let Some(exe) = engine_exe {
        if let Some(text) = run_capture(exe, &["--list-devices"], Duration::from_secs(20)) {
            let found = parse_list_devices(&text);
            if !found.is_empty() {
                return found;
            }
            // The engine ran and found no accelerator: that is an honest "no GPU" for this build.
            if text.to_ascii_lowercase().contains("available devices") {
                return vec![];
            }
        }
    }
    nvidia_gpus().0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_engines_device_list() {
        let text = "load_backend: loaded CUDA backend from C:\\x\\ggml-cuda.dll\n\
                    Available devices:\n  CUDA0: NVIDIA GeForce RTX 5060 Ti (16311 MiB, 15111 MiB free)\n  \
                    Vulkan0: AMD Radeon RX 7600 (8176 MiB, 7900 MiB free)\n  CPU: AMD Ryzen 7 (32768 MiB, 20000 MiB free)\n";
        let g = parse_list_devices(text);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].device, "CUDA0");
        assert_eq!(g[0].backend, "cuda");
        assert_eq!(g[0].name, "NVIDIA GeForce RTX 5060 Ti");
        assert_eq!((g[0].vram_total_mb, g[0].vram_free_mb), (16311, 15111));
        assert_eq!(g[1].backend, "vulkan");
    }

    #[test]
    fn an_engine_with_no_accelerators_lists_nothing() {
        assert!(parse_list_devices("Available devices:\n").is_empty());
        assert!(parse_list_devices("  CPU: Intel (1000 MiB, 900 MiB free)\n").is_empty());
        assert!(parse_list_devices("RPC0: 127.0.0.1:50052 (1 MiB, 1 MiB free)").is_empty());
    }

    #[test]
    fn parses_nvidia_smi_output() {
        let g =
            parse_nvidia_smi("NVIDIA GeForce RTX 5060 Ti, 16311, 15111\nNVIDIA T4, 15360, 15000\n");
        assert_eq!(g.len(), 2);
        assert_eq!(g[1].device, "CUDA1");
        assert_eq!(g[0].vram_free_mb, 15111);
        assert!(parse_nvidia_smi("garbage\nNo devices were found").is_empty());
    }

    #[test]
    fn memory_bandwidth_is_a_sensible_number() {
        let bw = mem_bandwidth_gbps();
        assert!(bw > 0.5 && bw < 5000.0, "{bw}");
    }
}

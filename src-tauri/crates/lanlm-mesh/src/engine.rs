//! Installing the llama.cpp engine: which build fits this machine's GPU, downloading it, and
//! checking that it really sees the GPU before keeping it.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::dl;
use crate::hw;

const RELEASES: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=15";

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

/// What the machine has, as far as choosing a build goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Accel {
    pub nvidia: bool,
    /// Major version of the NVIDIA driver (580 for 580.97).
    pub nvidia_driver: Option<u32>,
    /// RTX 50-series and newer need CUDA 12.8 or later.
    pub blackwell: bool,
    pub amd: bool,
    pub intel_gpu: bool,
}

/// One installable build: its name and the files to download (the engine, plus the CUDA
/// runtime when the build needs it).
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    /// `cuda-13.4`, `vulkan`, `cpu`, ...
    pub variant: String,
    pub assets: Vec<Asset>,
}

pub fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// Finds a program anywhere under `dir` (release archives differ in layout).
pub fn find_exe(dir: &Path, base: &str) -> Option<PathBuf> {
    let want = exe_name(base);
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_exe(&p, base) {
                return Some(found);
            }
        } else if p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&want))
        {
            return Some(p);
        }
    }
    None
}

/// The RPC worker program: named `rpc-server` in current releases, `ggml-rpc-server` before.
pub fn find_rpc_server(dir: &Path) -> Option<PathBuf> {
    find_exe(dir, "rpc-server").or_else(|| find_exe(dir, "ggml-rpc-server"))
}

// ───────────────────────────── choosing a build ─────────────────────────────

fn cuda_version(name: &str) -> Option<(u32, u32)> {
    let rest = name.split("cuda-").nth(1)?;
    let ver = rest.split("-x64").next()?;
    let mut it = ver.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

/// Whether a CUDA build of this version can run on this machine.
fn cuda_ok(ver: (u32, u32), a: &Accel) -> bool {
    let Some(driver) = a.nvidia_driver else {
        return a.nvidia; // unknown driver: try it, the check after installing will catch a mismatch
    };
    let min_driver = match ver.0 {
        13.. => 580,
        12 if ver.1 >= 8 => 570,
        12 => 525,
        _ => 450,
    };
    if driver < min_driver {
        return false;
    }
    // Blackwell GPUs (RTX 50-series) have no kernels in CUDA builds older than 12.8.
    !a.blackwell || ver >= (12, 8)
}

fn is_archive(name: &str) -> bool {
    name.ends_with(".zip") || name.ends_with(".tar.gz")
}

/// The builds that suit this machine, best first, that this release actually ships.
/// `os` is `windows` or `linux`.
pub fn rank(os: &str, a: &Accel, assets: &[Asset]) -> Vec<Choice> {
    let platform = match os {
        "windows" => "-bin-win-",
        "linux" => "-bin-ubuntu-",
        _ => return vec![],
    };
    // Plain engine archives for 64-bit x86 on this OS (not the CUDA runtime bundles).
    let engines: Vec<&Asset> = assets
        .iter()
        .filter(|x| {
            is_archive(&x.name)
                && x.name.contains(platform)
                && x.name.contains("x64")
                && !x.name.contains("arm64")
                && !x.name.starts_with("cudart")
        })
        .collect();
    let runtimes: Vec<&Asset> = assets
        .iter()
        .filter(|x| {
            is_archive(&x.name) && x.name.starts_with("cudart") && x.name.contains(platform)
        })
        .collect();

    let mut out: Vec<Choice> = Vec::new();

    if a.nvidia {
        let mut cuda: Vec<((u32, u32), &Asset)> = engines
            .iter()
            .filter_map(|x| cuda_version(&x.name).map(|v| (v, *x)))
            .filter(|(v, _)| cuda_ok(*v, a))
            .collect();
        cuda.sort_by_key(|x| std::cmp::Reverse(x.0));
        for (ver, asset) in cuda {
            let tag = format!("cuda-{}.{}", ver.0, ver.1);
            let mut files = vec![(*asset).clone()];
            if let Some(rt) = runtimes.iter().find(|r| cuda_version(&r.name) == Some(ver)) {
                files.push((*rt).clone());
            }
            out.push(Choice {
                variant: tag,
                assets: files,
            });
        }
    }
    if a.nvidia || a.amd || a.intel_gpu {
        if let Some(v) = engines.iter().find(|x| x.name.contains("vulkan")) {
            out.push(Choice {
                variant: "vulkan".into(),
                assets: vec![(*v).clone()],
            });
        }
    }
    // The plain CPU build: `win-cpu-x64` / `ubuntu-x64`, or `win-avx2-x64` in older releases.
    let cpu = engines.iter().find(|x| {
        let n = &x.name;
        n.contains("cpu")
            || n.contains("avx2")
            || (os == "linux" && n.ends_with("-ubuntu-x64.tar.gz"))
    });
    if let Some(c) = cpu {
        out.push(Choice {
            variant: "cpu".into(),
            assets: vec![(*c).clone()],
        });
    }
    out
}

fn parse_driver_major(text: &str) -> Option<u32> {
    text.trim().split('.').next()?.parse().ok()
}

fn looks_blackwell(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    [
        "RTX 50",
        "RTX PRO 6000 BLACKWELL",
        "RTX PRO 5000",
        "RTX PRO 4000",
        "B100",
        "B200",
        "GB10",
        "GB200",
    ]
    .iter()
    .any(|k| n.contains(k))
}

/// Looks at the machine's graphics adapters.
pub fn detect_accel() -> Accel {
    let (nv, driver) = hw::nvidia_gpus();
    let mut a = Accel {
        nvidia: !nv.is_empty(),
        nvidia_driver: driver.as_deref().and_then(parse_driver_major),
        blackwell: nv.iter().any(|g| looks_blackwell(&g.name)),
        ..Default::default()
    };
    for name in adapter_names() {
        let n = name.to_ascii_lowercase();
        if n.contains("nvidia") {
            a.nvidia = true;
        } else if n.contains("amd") || n.contains("radeon") {
            a.amd = true;
        } else if n.contains("intel") && (n.contains("arc") || n.contains("iris xe")) {
            a.intel_gpu = true;
        }
    }
    a
}

/// Names of the graphics adapters the operating system lists.
fn adapter_names() -> Vec<String> {
    if cfg!(windows) {
        let ps = Path::new("powershell.exe");
        hw::run_capture(
            ps,
            &[
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_VideoController | ForEach-Object { $_.Name }",
            ],
            std::time::Duration::from_secs(15),
        )
        .map(|t| {
            t.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
    } else {
        // Vendor ids from the kernel: 0x10de NVIDIA, 0x1002 AMD, 0x8086 Intel.
        let mut v = Vec::new();
        if let Ok(rd) = std::fs::read_dir("/sys/class/drm") {
            for e in rd.flatten() {
                let p = e.path().join("device/vendor");
                if let Ok(id) = std::fs::read_to_string(p) {
                    match id.trim() {
                        "0x10de" => v.push("NVIDIA".to_string()),
                        "0x1002" => v.push("AMD Radeon".to_string()),
                        _ => {}
                    }
                }
            }
        }
        v
    }
}

// ───────────────────────────── installing ─────────────────────────────

#[derive(Clone, Debug)]
pub enum Step {
    Status(String),
    Bytes { done: u64, total: u64 },
    Done,
}

#[derive(Clone, Debug)]
pub struct Installed {
    pub tag: String,
    pub variant: String,
}

pub fn installed_variant(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("VARIANT"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn installed_version(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("VERSION"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn unpack(archive: &Path, dir: &Path) -> Result<(), String> {
    let name = archive.to_string_lossy().to_ascii_lowercase();
    if name.ends_with(".zip") {
        let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
        let mut z = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        // The zip crate refuses entries that would escape `dir`.
        z.extract(dir).map_err(|e| e.to_string())
    } else {
        let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
        let mut t = tar::Archive::new(flate2::read::GzDecoder::new(file));
        // `unpack` rejects entries that would escape `dir`.
        t.unpack(dir).map_err(|e| e.to_string())
    }
}

/// Downloads and installs the best engine for this machine into `dir`.
///
/// `want` is `None` or `Some("auto")` to choose by GPU, or a variant such as `cpu`, `vulkan`,
/// `cuda-13.4` to force one. After installing a GPU build the engine is asked which devices it
/// sees; if it sees none, the next choice is tried, so a machine never ends up with a GPU build
/// that silently runs on the CPU.
pub async fn install(
    http: &reqwest::Client,
    dir: &Path,
    want: Option<&str>,
    mut on: impl FnMut(Step),
) -> Result<Installed, String> {
    let os = if cfg!(windows) { "windows" } else { "linux" };
    on(Step::Status(
        "Looking at this PC's graphics hardware…".into(),
    ));
    let accel = tokio::task::spawn_blocking(detect_accel)
        .await
        .map_err(|e| e.to_string())?;

    on(Step::Status(
        "Looking up the latest llama.cpp release…".into(),
    ));
    let releases: Vec<Release> = http
        .get(RELEASES)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("Could not reach GitHub: {e}"))?
        .error_for_status()
        .map_err(|e| format!("GitHub answered with an error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Unexpected release data: {e}"))?;

    let forced = want.filter(|w| !w.is_empty() && *w != "auto");
    let pick = |r: &Release| -> Vec<Choice> {
        let mut c = rank(os, &accel, &r.assets);
        if let Some(f) = forced {
            // A forced choice may need hardware `detect_accel` did not see; rank with everything on.
            let all = Accel {
                nvidia: true,
                nvidia_driver: None,
                blackwell: false,
                amd: true,
                intel_gpu: true,
            };
            c = rank(os, &all, &r.assets)
                .into_iter()
                .filter(|x| x.variant == f || x.variant.starts_with(&format!("{f}.")))
                .collect();
        }
        c
    };
    let (release, choices) = releases
        .iter()
        .find_map(|r| {
            let c = pick(r);
            (!c.is_empty()).then_some((r, c))
        })
        .ok_or("No recent llama.cpp release has a build for this PC")?;

    let mut last_error = String::new();
    for choice in choices {
        match try_install(http, dir, release, &choice, &mut on).await {
            Ok(done) => {
                if choice.variant != "cpu" && forced.is_none() {
                    // Make sure the GPU build can see a GPU, or fall through to the next build.
                    let Some(exe) = find_exe(dir, "llama-server") else {
                        continue;
                    };
                    let seen = tokio::task::spawn_blocking(move || hw::gpus(Some(&exe)))
                        .await
                        .unwrap_or_default();
                    if seen.is_empty() {
                        last_error = format!(
                            "The {} build does not see a GPU on this PC. Trying the next build.",
                            choice.variant
                        );
                        on(Step::Status(last_error.clone()));
                        continue;
                    }
                }
                on(Step::Done);
                return Ok(done);
            }
            Err(e) => last_error = e,
        }
    }
    Err(if last_error.is_empty() {
        "No engine build could be installed".into()
    } else {
        last_error
    })
}

async fn try_install(
    http: &reqwest::Client,
    dir: &Path,
    release: &Release,
    choice: &Choice,
    on: &mut impl FnMut(Step),
) -> Result<Installed, String> {
    // Start clean so files of two different builds never mix.
    let dl_dir = dir.join(".download");
    let _ = tokio::fs::remove_dir_all(dir).await;
    tokio::fs::create_dir_all(&dl_dir)
        .await
        .map_err(|e| e.to_string())?;

    let mut files = Vec::new();
    for a in &choice.assets {
        on(Step::Status(format!("Downloading {} …", a.name)));
        let dest = dl_dir.join(&a.name);
        dl::download(
            http,
            &a.browser_download_url,
            &dest,
            || false,
            |done, total| {
                on(Step::Bytes { done, total });
            },
        )
        .await?;
        files.push(dest);
    }
    on(Step::Status("Unpacking…".into()));
    let dir2 = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        for f in files {
            unpack(&f, &dir2)?;
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| e.to_string())??;
    let _ = tokio::fs::remove_dir_all(&dl_dir).await;

    if find_exe(dir, "llama-server").is_none() {
        return Err("The download did not contain llama-server".into());
    }
    let _ = std::fs::write(dir.join("VERSION"), &release.tag_name);
    let _ = std::fs::write(dir.join("VARIANT"), &choice.variant);
    Ok(Installed {
        tag: release.tag_name.clone(),
        variant: choice.variant.clone(),
    })
}

#[cfg(test)]
mod tests;

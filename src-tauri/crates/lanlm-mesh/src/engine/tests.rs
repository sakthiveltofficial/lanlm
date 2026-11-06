use super::*;

/// The asset names of a real llama.cpp release (b12167).
fn assets() -> Vec<Asset> {
    [
        "cudart-llama-b12167-bin-ubuntu-cuda-12.8-x64.tar.gz",
        "cudart-llama-b12167-bin-ubuntu-cuda-13.4-arm64.tar.gz",
        "cudart-llama-b12167-bin-ubuntu-cuda-13.4-x64.tar.gz",
        "cudart-llama-bin-win-cuda-12.4-x64.zip",
        "cudart-llama-bin-win-cuda-13.4-arm64.zip",
        "cudart-llama-bin-win-cuda-13.4-x64.zip",
        "llama-b12167-bin-macos-arm64.tar.gz",
        "llama-b12167-bin-ubuntu-arm64.tar.gz",
        "llama-b12167-bin-ubuntu-cuda-12.8-x64.tar.gz",
        "llama-b12167-bin-ubuntu-cuda-13.4-arm64.tar.gz",
        "llama-b12167-bin-ubuntu-cuda-13.4-x64.tar.gz",
        "llama-b12167-bin-ubuntu-rocm-10.0-x64.tar.gz",
        "llama-b12167-bin-ubuntu-vulkan-arm64.tar.gz",
        "llama-b12167-bin-ubuntu-vulkan-x64.tar.gz",
        "llama-b12167-bin-ubuntu-x64.tar.gz",
        "llama-b12167-bin-win-cpu-arm64.zip",
        "llama-b12167-bin-win-cpu-x64.zip",
        "llama-b12167-bin-win-cuda-12.4-x64.zip",
        "llama-b12167-bin-win-cuda-13.4-arm64.zip",
        "llama-b12167-bin-win-cuda-13.4-x64.zip",
        "llama-b12167-bin-win-opencl-adreno-arm64.zip",
        "llama-b12167-bin-win-rocm-10.0-x64.zip",
        "llama-b12167-bin-win-sycl-x64.zip",
        "llama-b12167-bin-win-vulkan-arm64.zip",
        "llama-b12167-bin-win-vulkan-x64.zip",
    ]
    .iter()
    .map(|n| Asset {
        name: n.to_string(),
        browser_download_url: format!("https://example.invalid/{n}"),
    })
    .collect()
}

fn variants(os: &str, a: &Accel) -> Vec<String> {
    rank(os, a, &assets())
        .into_iter()
        .map(|c| c.variant)
        .collect()
}

fn nvidia(driver: u32, blackwell: bool) -> Accel {
    Accel {
        nvidia: true,
        nvidia_driver: Some(driver),
        blackwell,
        ..Default::default()
    }
}

#[test]
fn a_pc_with_no_gpu_gets_the_cpu_build() {
    assert_eq!(variants("windows", &Accel::default()), ["cpu"]);
    assert_eq!(variants("linux", &Accel::default()), ["cpu"]);
}

#[test]
fn a_new_nvidia_card_with_a_new_driver_gets_cuda_13_then_falls_back() {
    // An RTX 5060 Ti on a 580+ driver.
    assert_eq!(
        variants("windows", &nvidia(581, true)),
        ["cuda-13.4", "vulkan", "cpu"]
    );
}

#[test]
fn a_blackwell_card_never_gets_the_old_cuda_12_4_build() {
    // Driver too old for CUDA 13, and the only Windows CUDA 12 build is 12.4: Vulkan instead.
    assert_eq!(variants("windows", &nvidia(560, true)), ["vulkan", "cpu"]);
}

#[test]
fn an_older_nvidia_card_gets_the_cuda_12_build_it_can_run() {
    assert_eq!(
        variants("windows", &nvidia(552, false)),
        ["cuda-12.4", "vulkan", "cpu"]
    );
    // A very old driver cannot run either CUDA build.
    assert_eq!(variants("windows", &nvidia(450, false)), ["vulkan", "cpu"]);
}

#[test]
fn cuda_builds_come_with_their_runtime_files() {
    let c = &rank("windows", &nvidia(581, true), &assets())[0];
    assert_eq!(c.assets.len(), 2);
    assert!(c.assets[0]
        .name
        .starts_with("llama-b12167-bin-win-cuda-13.4"));
    assert!(c.assets[1].name.starts_with("cudart"));
    assert!(c.assets[1].name.contains("13.4"));
    // CPU and Vulkan builds are a single archive.
    let v = rank("windows", &nvidia(581, true), &assets());
    assert_eq!(v.last().unwrap().assets.len(), 1);
}

#[test]
fn amd_and_intel_gpus_use_vulkan() {
    let amd = Accel {
        amd: true,
        ..Default::default()
    };
    assert_eq!(variants("windows", &amd), ["vulkan", "cpu"]);
    let arc = Accel {
        intel_gpu: true,
        ..Default::default()
    };
    assert_eq!(variants("linux", &arc), ["vulkan", "cpu"]);
}

#[test]
fn linux_picks_its_own_archives() {
    let c = rank("linux", &nvidia(581, true), &assets());
    assert_eq!(c[0].variant, "cuda-13.4");
    assert!(c[0].assets[0].name.ends_with("ubuntu-cuda-13.4-x64.tar.gz"));
    assert!(c[0].assets[1].name.contains("cudart"));
    assert!(c
        .iter()
        .all(|x| x.assets.iter().all(|a| !a.name.contains("arm64"))));
    // Blackwell on an older Linux driver can use the 12.8 build.
    assert_eq!(
        variants("linux", &nvidia(572, true)),
        ["cuda-12.8", "vulkan", "cpu"]
    );
}

#[test]
fn an_older_release_called_the_cpu_build_avx2() {
    let old = vec![Asset {
        name: "llama-b3000-bin-win-avx2-x64.zip".into(),
        browser_download_url: "https://example.invalid/x".into(),
    }];
    let c = rank("windows", &Accel::default(), &old);
    assert_eq!(c[0].variant, "cpu");
}

#[test]
fn unknown_systems_get_nothing() {
    assert!(rank("plan9", &Accel::default(), &assets()).is_empty());
}

#[test]
fn blackwell_and_driver_detection() {
    assert!(looks_blackwell("NVIDIA GeForce RTX 5060 Ti"));
    assert!(looks_blackwell("NVIDIA GeForce RTX 5090"));
    assert!(!looks_blackwell("NVIDIA GeForce RTX 4090"));
    assert_eq!(parse_driver_major("581.15"), Some(581));
    assert_eq!(parse_driver_major("nonsense"), None);
}

#[test]
fn finds_programs_in_nested_folders() {
    let dir = std::env::temp_dir().join(format!("lanlm-eng-{}-{}", std::process::id(), rand_id()));
    let nested = dir.join("llama-b1").join("bin");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join(exe_name("llama-server")), b"x").unwrap();
    std::fs::write(nested.join(exe_name("rpc-server")), b"x").unwrap();
    assert!(find_exe(&dir, "llama-server").is_some());
    assert!(find_rpc_server(&dir).is_some());
    assert!(find_exe(&dir, "missing").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

fn rand_id() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lanlm-eng-{name}-{}-{}",
        std::process::id(),
        rand_id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for (name, data) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn unpack_extracts_zip_files_including_nested_folders() {
    let dir = temp_dir("unpack");
    let zip_path = dir.join("engine.zip");
    make_zip(
        &zip_path,
        &[
            ("llama-server.exe", b"server"),
            ("ggml.dll", b"dll"),
            ("bin/tools/rpc.exe", b"rpc"),
        ],
    );
    let out = dir.join("engine");
    unpack(&zip_path, &out).unwrap();
    assert_eq!(
        std::fs::read(out.join("llama-server.exe")).unwrap(),
        b"server"
    );
    assert_eq!(
        std::fs::read(out.join("bin/tools/rpc.exe")).unwrap(),
        b"rpc"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn unpack_extracts_tar_gz_archives() {
    let dir = temp_dir("targz");
    let path = dir.join("engine.tar.gz");
    {
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut tarball = tar::Builder::new(gz);
        let data = b"server";
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        tarball
            .append_data(&mut h, "llama-b1/llama-server", &data[..])
            .unwrap();
        tarball.into_inner().unwrap().finish().unwrap();
    }
    let out = dir.join("engine");
    unpack(&path, &out).unwrap();
    assert_eq!(
        std::fs::read(out.join("llama-b1/llama-server")).unwrap(),
        b"server"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn unpack_rejects_files_that_are_not_archives() {
    let dir = temp_dir("notzip");
    let bad = dir.join("engine.zip");
    std::fs::write(&bad, b"this is not a zip archive").unwrap();
    assert!(unpack(&bad, &dir.join("engine")).is_err());
    assert!(unpack(&dir.join("missing.zip"), &dir.join("engine")).is_err());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn unpack_never_writes_outside_the_target_folder() {
    // A malicious archive could name an entry "../evil.txt" to escape (zip slip).
    let dir = temp_dir("zipslip");
    let zip_path = dir.join("evil.zip");
    make_zip(
        &zip_path,
        &[("../evil.txt", b"gotcha"), ("ok.txt", b"fine")],
    );
    let out = dir.join("engine");
    let _ = unpack(&zip_path, &out);
    assert!(
        !dir.join("evil.txt").exists(),
        "an entry escaped the target folder"
    );
    std::fs::remove_dir_all(dir).ok();
}

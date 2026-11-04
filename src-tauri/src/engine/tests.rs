use super::*;

fn asset(name: &str) -> Asset {
    Asset {
        name: name.into(),
        browser_download_url: format!("https://example.test/{name}"),
    }
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("lanlm-test-{name}-{}-{nanos}", std::process::id()));
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
fn unpack_extracts_files_including_nested_folders() {
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
    assert_eq!(std::fs::read(out.join("ggml.dll")).unwrap(), b"dll");
    assert_eq!(
        std::fs::read(out.join("bin/tools/rpc.exe")).unwrap(),
        b"rpc"
    );
    assert!(find_exe(&out, "rpc.exe").is_some());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn unpack_rejects_a_file_that_is_not_a_zip() {
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

#[test]
fn picks_the_plain_cpu_build_for_64_bit_windows() {
    let assets = vec![
        asset("cudart-llama-bin-win-cuda-12.4-x64.zip"),
        asset("llama-b1-bin-win-cuda-12.4-x64.zip"),
        asset("llama-b1-bin-win-cpu-arm64.zip"),
        asset("llama-b1-bin-win-cpu-x64.zip"),
        asset("llama-b1-bin-ubuntu-x64.zip"),
    ];
    assert_eq!(
        pick_asset(&assets).unwrap().name,
        "llama-b1-bin-win-cpu-x64.zip"
    );
}

#[test]
fn falls_back_to_the_older_avx2_name() {
    let assets = vec![
        asset("llama-b1-bin-win-avx2-x64.zip"),
        asset("llama-b1-bin-win-avx512-x64.zip"),
    ];
    assert_eq!(
        pick_asset(&assets).unwrap().name,
        "llama-b1-bin-win-avx2-x64.zip"
    );
}

#[test]
fn finds_nothing_in_a_release_without_windows_cpu_builds() {
    // GitHub's `releases/latest` once pointed at a placeholder with a single text asset.
    assert!(pick_asset(&[asset("nightly-tag.txt")]).is_none());
    assert!(pick_asset(&[
        asset("llama-b1-bin-macos-arm64.zip"),
        asset("llama-b1-bin-win-cuda-12.4-x64.zip")
    ])
    .is_none());
    assert!(pick_asset(&[]).is_none());
}

#[test]
fn find_exe_searches_subfolders_and_ignores_case() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("lanlm-test-engine-{}-{nanos}", std::process::id()));
    let nested = dir.join("llama-b1").join("bin");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("Llama-Server.EXE"), b"x").unwrap();
    let found = find_exe(&dir, "llama-server.exe").expect("should find the nested exe");
    assert!(found.ends_with("Llama-Server.EXE"));
    assert!(find_exe(&dir, "missing.exe").is_none());
    std::fs::remove_dir_all(dir).ok();
}

use super::*;

fn asset(name: &str) -> Asset {
    Asset {
        name: name.into(),
        browser_download_url: format!("https://example.test/{name}"),
    }
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

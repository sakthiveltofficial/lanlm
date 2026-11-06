use super::*;

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
    let name = if cfg!(windows) {
        "Llama-Server.EXE"
    } else {
        "llama-server"
    };
    std::fs::write(nested.join(name), b"x").unwrap();
    let found = find_exe(&dir, "llama-server.exe").expect("should find the nested exe");
    assert!(found.ends_with(name));
    assert!(find_exe(&dir, "missing.exe").is_none());
    std::fs::remove_dir_all(dir).ok();
}

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn tmp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("lanlm-test-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn body(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

/// A tiny HTTP server for one file. It honours `Range: bytes=N-`, answers 416 once
/// the range starts past the end, and answers 404 for `/missing`.
async fn serve(data: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let data = data.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let start: usize = req
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("range: bytes=")
                            .map(|r| r.trim_end_matches('-').trim().parse().unwrap_or(0))
                    })
                    .unwrap_or(0);
                let total = data.len();
                let missing = req.contains("/missing");
                let past_end = start >= total && start > 0;
                let head = if missing {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                } else if past_end {
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                } else if start > 0 {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{}/{total}\r\nConnection: close\r\n\r\n",
                        total - start,
                        total - 1
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"
                    )
                };
                let _ = sock.write_all(head.as_bytes()).await;
                if !missing && !past_end {
                    let _ = sock.write_all(&data[start.min(total)..]).await;
                }
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}/file.bin")
}

#[tokio::test]
async fn downloads_a_whole_file_and_reports_progress() {
    let dir = tmp_dir("dl-full");
    let data = body(300_000);
    let url = serve(data.clone()).await;
    let dest = dir.join("out.bin");
    let last = AtomicU64::new(0);
    download(
        &reqwest::Client::new(),
        &url,
        &dest,
        || false,
        |done, _| last.store(done, Ordering::SeqCst),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert!(
        !dir.join("out.bin.part").exists(),
        "the .part file must be renamed away"
    );
    assert_eq!(last.load(Ordering::SeqCst), data.len() as u64);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn resumes_from_a_partial_file() {
    let dir = tmp_dir("dl-resume");
    let data = body(50_000);
    let url = serve(data.clone()).await;
    let dest = dir.join("out.bin");
    std::fs::write(dir.join("out.bin.part"), &data[..12_345]).unwrap();
    download(&reqwest::Client::new(), &url, &dest, || false, |_, _| {})
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        data,
        "a resumed file must match byte for byte"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn a_part_file_that_is_already_complete_is_just_finished() {
    let dir = tmp_dir("dl-416");
    let data = body(8_000);
    let url = serve(data.clone()).await;
    let dest = dir.join("out.bin");
    std::fs::write(dir.join("out.bin.part"), &data).unwrap();
    download(&reqwest::Client::new(), &url, &dest, || false, |_, _| {})
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn cancelling_keeps_the_part_file_and_creates_no_final_file() {
    let dir = tmp_dir("dl-cancel");
    let url = serve(body(2_000_000)).await;
    let dest = dir.join("out.bin");
    let err = download(&reqwest::Client::new(), &url, &dest, || true, |_, _| {})
        .await
        .unwrap_err();
    assert_eq!(err, "cancelled");
    assert!(!dest.exists());
    assert!(
        dir.join("out.bin.part").exists(),
        "the partial file is kept so the download can resume"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn http_errors_are_reported() {
    let dir = tmp_dir("dl-404");
    let base = serve(body(10)).await;
    let url = base.replace("file.bin", "missing");
    let err = download(
        &reqwest::Client::new(),
        &url,
        &dir.join("x.bin"),
        || false,
        |_, _| {},
    )
    .await
    .unwrap_err();
    assert!(err.contains("404"), "{err}");
    assert!(!dir.join("x.bin").exists());
    std::fs::remove_dir_all(dir).ok();
}

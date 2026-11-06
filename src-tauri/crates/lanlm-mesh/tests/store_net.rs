//! Model files moving between nodes, and from a download server, over real sockets.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lanlm_mesh::store::{sha256_file, Store, CHUNK};
use lanlm_mesh::{Mesh, MeshConfig};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "lanlm-store-{tag}-{}-{}",
        std::process::id(),
        rand::random::<u32>()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Dev {
    mesh: Mesh,
    store: Store,
}

async fn dev(name: &str) -> Dev {
    let mut cfg = MeshConfig::new(temp_dir(name));
    cfg.name = Some(name.to_string());
    cfg.port = 0;
    cfg.discovery = false;
    let mesh = Mesh::start(cfg).await.unwrap();
    let store = Store::new(
        mesh.clone(),
        temp_dir(&format!("{name}-models")),
        reqwest::Client::new(),
    );
    store.start_service();
    Dev { mesh, store }
}

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn alive(m: &Mesh) -> usize {
    m.snapshot().nodes.iter().filter(|n| n.alive).count()
}

async fn cluster(names: &[&str]) -> Vec<Dev> {
    let mut devs = Vec::new();
    for n in names {
        devs.push(dev(n).await);
    }
    let key = devs[0].mesh.create_cluster("t");
    for d in &devs[1..] {
        d.mesh.join_cluster(&key, "t").unwrap();
    }
    for a in &devs {
        for b in &devs {
            if a.mesh.id() != b.mesh.id() {
                a.mesh.add_peer(&format!("127.0.0.1:{}", b.mesh.port()));
            }
        }
    }
    for d in &devs {
        wait_for("cluster", 15, || alive(&d.mesh) == names.len()).await;
    }
    devs
}

fn content(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 31 + i / 7) % 251) as u8).collect()
}

fn hex_sha(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Gives a device a model file and tells the cluster about it.
async fn give(d: &Dev, name: &str, data: &[u8]) {
    std::fs::write(d.store.path(name), data).unwrap();
    while d.store.background_tick().await {}
    let held = d.store.scan();
    d.mesh.update_self(|r| {
        r.models = held;
        true
    });
}

/// A download server that supports ranges and counts the ranged requests it serves.
async fn http_server(data: Vec<u8>) -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let gets = Arc::new(AtomicUsize::new(0));
    let g2 = gets.clone();
    let etag = hex_sha(&data);
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            let (data, g, etag) = (data.clone(), g2.clone(), etag.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let range = req.lines().find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("range: bytes=")
                        .map(|r| r.trim().to_string())
                });
                if req.starts_with("HEAD") {
                    let h = format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\netag: \"{etag}\"\r\nconnection: close\r\n\r\n",
                        data.len()
                    );
                    let _ = s.write_all(h.as_bytes()).await;
                    return;
                }
                if let Some(r) = range {
                    g.fetch_add(1, Ordering::SeqCst);
                    let (a, b) = r.split_once('-').unwrap();
                    let a: usize = a.parse().unwrap();
                    let b: usize = b.parse::<usize>().unwrap().min(data.len() - 1);
                    let body = &data[a..=b];
                    let h = format!(
                        "HTTP/1.1 206 Partial Content\r\ncontent-length: {}\r\ncontent-range: bytes {a}-{b}/{}\r\nconnection: close\r\n\r\n",
                        body.len(),
                        data.len()
                    );
                    let _ = s.write_all(h.as_bytes()).await;
                    let _ = s.write_all(body).await;
                } else {
                    let h = format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        data.len()
                    );
                    let _ = s.write_all(h.as_bytes()).await;
                    let _ = s.write_all(&data).await;
                }
            });
        }
    });
    (format!("http://{addr}/m.gguf"), gets)
}

#[tokio::test]
async fn a_model_is_copied_from_a_peer_and_checked_against_its_hash() {
    let devs = cluster(&["alpha", "bravo"]).await;
    let (a, b) = (&devs[0], &devs[1]);
    let data = content(CHUNK as usize * 2 + 12345);
    give(a, "m.gguf", &data).await;
    wait_for("bravo sees the model", 8, || {
        b.mesh.with_registry(|r, now| {
            r.holders("m.gguf", now)
                .iter()
                .any(|e| !e.record.models[0].sha256.is_empty())
        })
    })
    .await;

    b.store.fetch("m.gguf", None, None, None).await.unwrap();
    assert_eq!(std::fs::read(b.store.path("m.gguf")).unwrap(), data);
    while b.store.background_tick().await {}
    assert_eq!(b.store.scan()[0].sha256, hex_sha(&data));
    assert!(b.store.transfers().is_empty());
    assert!(!b.store.path("m.gguf.part").exists());
}

#[tokio::test]
async fn a_wrong_hash_is_refused_and_nothing_is_kept() {
    let devs = cluster(&["alpha", "bravo"]).await;
    let (a, b) = (&devs[0], &devs[1]);
    let data = content(CHUNK as usize + 99);
    give(a, "m.gguf", &data).await;
    wait_for("bravo sees the model", 8, || {
        b.mesh
            .with_registry(|r, now| r.holders("m.gguf", now).len() == 1)
    })
    .await;
    let wrong = "ab".repeat(32);
    let err = b
        .store
        .fetch("m.gguf", None, Some(&wrong), None)
        .await
        .unwrap_err();
    assert!(err.contains("hash"), "{err}");
    assert!(!b.store.path("m.gguf").exists());
    assert!(!b.store.path("m.gguf.part").exists());
}

#[tokio::test]
async fn the_internet_is_used_in_pieces_when_no_peer_has_the_model() {
    let devs = cluster(&["alpha"]).await;
    let a = &devs[0];
    let data = content(CHUNK as usize * 3 + 7);
    let (url, gets) = http_server(data.clone()).await;
    // The server's etag is the file hash, so the download is verified without being told it.
    a.store
        .fetch("m.gguf", Some(&url), None, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(a.store.path("m.gguf")).unwrap(), data);
    assert_eq!(
        gets.load(Ordering::SeqCst),
        4,
        "one ranged request per piece"
    );
}

#[tokio::test]
async fn a_corrupted_download_is_caught_by_the_servers_hash() {
    let devs = cluster(&["alpha"]).await;
    let a = &devs[0];
    let data = content(CHUNK as usize + 100);
    let (url, _) = http_server(data.clone()).await;
    let err = a
        .store
        .fetch("m.gguf", Some(&url), Some(&"cd".repeat(32)), None)
        .await
        .unwrap_err();
    assert!(err.contains("hash"), "{err}");
    assert!(!a.store.path("m.gguf").exists());
}

#[tokio::test]
async fn a_device_prefers_its_neighbours_over_the_internet() {
    let devs = cluster(&["alpha", "bravo"]).await;
    let (a, b) = (&devs[0], &devs[1]);
    let data = content(CHUNK as usize * 3 + 5);
    give(a, "m.gguf", &data).await;
    wait_for("bravo sees the model", 8, || {
        b.mesh
            .with_registry(|r, now| r.holders("m.gguf", now).len() == 1)
    })
    .await;
    let (url, gets) = http_server(data.clone()).await;
    b.store
        .fetch("m.gguf", Some(&url), None, None)
        .await
        .unwrap();
    assert_eq!(
        sha256_file(&b.store.path("m.gguf")).unwrap(),
        hex_sha(&data)
    );
    assert_eq!(
        gets.load(Ordering::SeqCst),
        0,
        "everything was available on the network, so the internet should not be touched"
    );
}

#[tokio::test]
async fn an_interrupted_download_resumes_with_only_the_missing_pieces() {
    let devs = cluster(&["alpha"]).await;
    let a = &devs[0];
    let data = content(CHUNK as usize * 4);
    let (url, gets) = http_server(data.clone()).await;
    // A previous run finished the first two pieces.
    let part = a.store.path("m.gguf.part");
    let mut partial = vec![0u8; data.len()];
    partial[..CHUNK as usize * 2].copy_from_slice(&data[..CHUNK as usize * 2]);
    std::fs::write(&part, &partial).unwrap();
    let map = serde_json::json!({ "size": data.len(), "chunk": CHUNK, "done": [0, 1] });
    std::fs::write(a.store.path("m.gguf.part.map"), map.to_string()).unwrap();
    a.store
        .fetch("m.gguf", Some(&url), None, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(a.store.path("m.gguf")).unwrap(), data);
    assert_eq!(gets.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn three_devices_fetching_at_once_share_pieces() {
    let devs = cluster(&["alpha", "bravo", "charlie"]).await;
    let data = content(CHUNK as usize * 6 + 1);
    give(&devs[0], "m.gguf", &data).await;
    for d in &devs[1..] {
        wait_for("model visible", 8, || {
            d.mesh
                .with_registry(|r, now| r.holders("m.gguf", now).len() == 1)
        })
        .await;
    }
    let (b, c) = (devs[1].store.clone(), devs[2].store.clone());
    let (rb, rc) = tokio::join!(
        b.fetch("m.gguf", None, None, None),
        c.fetch("m.gguf", None, None, None)
    );
    rb.unwrap();
    rc.unwrap();
    for d in &devs[1..] {
        assert_eq!(std::fs::read(d.store.path("m.gguf")).unwrap(), data);
    }
}

#[tokio::test]
async fn an_unknown_model_with_no_link_says_so() {
    let devs = cluster(&["alpha"]).await;
    let err = devs[0]
        .store
        .fetch("nothing.gguf", None, None, None)
        .await
        .unwrap_err();
    assert!(err.contains("No device"), "{err}");
    let err = devs[0]
        .store
        .fetch("../evil.gguf", None, None, None)
        .await
        .unwrap_err();
    assert!(err.contains("folders"), "{err}");
}

#[tokio::test]
async fn deleting_a_model_removes_its_files_and_forgets_it() {
    let devs = cluster(&["alpha"]).await;
    let a = &devs[0];
    give(a, "m.gguf", &content(1000)).await;
    std::fs::write(a.store.path("m.mmproj.gguf"), b"proj").unwrap();
    assert_eq!(a.store.scan().len(), 1); // the projector is not listed on its own
    a.store.delete("m.gguf").unwrap();
    assert!(a.store.scan().is_empty());
    assert!(!a.store.path("m.mmproj.gguf").exists());
    assert!(a.store.delete("../x.gguf").is_err());
}

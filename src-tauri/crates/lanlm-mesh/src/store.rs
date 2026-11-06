//! The cluster's model store.
//!
//! * Every node knows which model files it holds, with a content hash, and tells the cluster.
//! * A node that needs a model takes it from **peers on the local network** first and from the
//!   internet only for what no peer has. Several devices fetching the same model at once each
//!   take different pieces from the internet and swap the rest, so the model crosses the
//!   internet roughly once for the whole cluster.
//! * Pieces are 4 MiB. A piece is served to others as soon as it is finished, before the whole
//!   file is. The finished file is checked against a known hash (from the download source or
//!   from peers) before it is kept.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::gguf;
use crate::mesh::Mesh;
use crate::proto::{HeldModel, Transfer};

/// Size of one piece of a model file.
pub const CHUNK: u64 = 4 * 1024 * 1024;
const PEER_LANES: usize = 2;
const NET_LANES: usize = 4;
const MAX_FAILS: u32 = 4;

/// Download links must be https. Plain http is allowed only to this machine itself (a local
/// server or a test), where nobody on the network can tamper with it.
pub fn link_allowed(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|p| {
            url.strip_prefix(p)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/']))
        })
}

/// A file name that cannot escape the models folder.
pub fn safe_name(name: &str) -> Result<String, String> {
    let ok = !name.is_empty()
        && name.to_lowercase().ends_with(".gguf")
        && !name.contains(['/', '\\', ':', '\0'])
        && !name.contains("..");
    if ok {
        Ok(name.to_string())
    } else {
        Err("The file name must end in .gguf and contain no folders".into())
    }
}

// ───────────────────────────── piece bookkeeping ─────────────────────────────

/// Packs a list of done-flags into hex, lowest piece first.
pub fn encode_bitmap(done: &[bool]) -> String {
    let mut bytes = vec![0u8; done.len().div_ceil(8)];
    for (i, d) in done.iter().enumerate() {
        if *d {
            bytes[i / 8] |= 1 << (i % 8);
        }
    }
    hex::encode(bytes)
}

pub fn decode_bitmap(text: &str, n: usize) -> Vec<bool> {
    let bytes = hex::decode(text).unwrap_or_default();
    (0..n)
        .map(|i| bytes.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) != 0))
        .collect()
}

fn n_chunks(size: u64) -> usize {
    size.div_ceil(CHUNK) as usize
}

fn chunk_range(size: u64, i: usize) -> (u64, u64) {
    let start = i as u64 * CHUNK;
    (start, (start + CHUNK).min(size) - start)
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

// ───────────────────────────── the store ─────────────────────────────

struct Active {
    size: u64,
    done: Mutex<Vec<bool>>,
    claimed: Mutex<HashSet<usize>>,
    bytes: AtomicU64,
    cancel: AtomicBool,
    from: Mutex<BTreeSet<String>>,
    /// Peers that kept failing; their pieces are fetched elsewhere.
    bad_peers: Mutex<HashSet<String>>,
}

#[derive(Default)]
struct State {
    /// file -> (size, modified seconds, sha256)
    hashes: HashMap<String, (u64, u64, String)>,
    /// file -> (size, layers)
    layers: HashMap<String, (u64, u32)>,
    active: HashMap<String, Arc<Active>>,
    hashing: HashSet<String>,
}

struct Inner {
    mesh: Mesh,
    dir: PathBuf,
    http: reqwest::Client,
    no_redirect: reqwest::Client,
    state: Mutex<State>,
}

#[derive(Clone)]
pub struct Store(Arc<Inner>);

#[derive(Serialize, Deserialize)]
struct Req {
    op: String,
    #[serde(default)]
    file: String,
    #[serde(default)]
    offset: u64,
    #[serde(default)]
    len: u64,
}

/// Another device that can supply pieces.
#[derive(Clone, Debug)]
struct PeerSrc {
    id: String,
    name: String,
    size: u64,
    sha: String,
    /// `None`: has the whole file. `Some`: pieces it has so far.
    partial: Option<Vec<bool>>,
}

fn mtime_secs(p: &Path) -> u64 {
    p.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Store {
    pub fn new(mesh: Mesh, dir: PathBuf, http: reqwest::Client) -> Store {
        let no_redirect = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("LanLM")
            .connect_timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| http.clone());
        let _ = std::fs::create_dir_all(&dir);
        Store(Arc::new(Inner {
            mesh,
            dir,
            http,
            no_redirect,
            state: Mutex::new(State::default()),
        }))
    }

    pub fn dir(&self) -> &Path {
        &self.0.dir
    }

    pub fn path(&self, file: &str) -> PathBuf {
        self.0.dir.join(file)
    }

    /// Offers this node's files to the rest of the cluster.
    pub fn start_service(&self) {
        let me = self.clone();
        self.0.mesh.register_stream_service(
            "files",
            Arc::new(move |caller, sec| {
                let me = me.clone();
                Box::pin(async move { me.serve(caller, sec).await })
            }),
        );
    }

    // ───────────── what we hold ─────────────

    /// Model files on disk, with hashes and layer counts where already known.
    pub fn scan(&self) -> Vec<HeldModel> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.0.dir) else {
            return out;
        };
        let st = self.0.state.lock().unwrap();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let lower = name.to_lowercase();
            if !lower.ends_with(".gguf") || lower.ends_with(".mmproj.gguf") {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            let size = meta.len();
            let sha = st
                .hashes
                .get(&name)
                .filter(|(s, t, _)| *s == size && *t == mtime_secs(&e.path()))
                .map(|(_, _, h)| h.clone())
                .unwrap_or_default();
            let n_layer = st
                .layers
                .get(&name)
                .filter(|(s, _)| *s == size)
                .map(|(_, l)| *l)
                .unwrap_or(0);
            out.push(HeldModel {
                file: name,
                size,
                sha256: sha,
                n_layer,
            });
        }
        out.sort_by_key(|m| m.file.to_lowercase());
        out
    }

    /// Does one small piece of background work: reads a header or hashes a file we have not
    /// looked at yet. Call it every few seconds; it returns whether it found anything to do.
    pub async fn background_tick(&self) -> bool {
        let todo = {
            let st = self.0.state.lock().unwrap();
            let Ok(rd) = std::fs::read_dir(&self.0.dir) else {
                return false;
            };
            let mut found = None;
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let lower = name.to_lowercase();
                if !lower.ends_with(".gguf")
                    || lower.ends_with(".mmproj.gguf")
                    || st.active.contains_key(&name)
                    || st.hashing.contains(&name)
                {
                    continue;
                }
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                let layered = st.layers.get(&name).is_some_and(|(s, _)| *s == size);
                let hashed = st
                    .hashes
                    .get(&name)
                    .is_some_and(|(s, t, _)| *s == size && *t == mtime_secs(&e.path()));
                if !layered || !hashed {
                    found = Some((name, e.path(), size, layered, hashed));
                    break;
                }
            }
            found
        };
        let Some((name, path, size, layered, hashed)) = todo else {
            return false;
        };
        self.0.state.lock().unwrap().hashing.insert(name.clone());
        let p2 = path.clone();
        let res = tokio::task::spawn_blocking(move || {
            let layers = if layered {
                None
            } else {
                gguf::read(&p2).ok().map(|g| g.n_layer)
            };
            let sha = if hashed { None } else { sha256_file(&p2).ok() };
            (layers, sha)
        })
        .await
        .unwrap_or((None, None));
        let mtime = mtime_secs(&path);
        let mut st = self.0.state.lock().unwrap();
        st.hashing.remove(&name);
        match res.0 {
            Some(l) => {
                st.layers.insert(name.clone(), (size, l));
            }
            None if !layered => {
                // Unreadable header: remember that so we do not retry forever.
                st.layers.insert(name.clone(), (size, 0));
            }
            None => {}
        }
        if let Some(h) = res.1 {
            st.hashes.insert(name, (size, mtime, h));
        }
        true
    }

    pub fn transfers(&self) -> Vec<Transfer> {
        let st = self.0.state.lock().unwrap();
        let mut v: Vec<Transfer> = st
            .active
            .iter()
            .map(|(file, a)| Transfer {
                file: file.clone(),
                done: a.bytes.load(Ordering::Relaxed),
                total: a.size,
                from: a.from.lock().unwrap().iter().cloned().collect(),
                chunk_size: CHUNK,
                have: encode_bitmap(&a.done.lock().unwrap()),
            })
            .collect();
        v.sort_by(|a, b| a.file.cmp(&b.file));
        v
    }

    pub fn cancel(&self, file: &str) {
        if let Some(a) = self.0.state.lock().unwrap().active.get(file) {
            a.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn delete(&self, file: &str) -> Result<(), String> {
        let file = safe_name(file)?;
        if self.0.state.lock().unwrap().active.contains_key(&file) {
            return Err("That model is being downloaded; cancel it first".into());
        }
        for name in [
            file.clone(),
            format!("{file}.part"),
            format!("{file}.part.map"),
            format!("{}.mmproj.gguf", file.trim_end_matches(".gguf")),
        ] {
            let p = self.0.dir.join(&name);
            if p.exists() {
                std::fs::remove_file(p).map_err(|e| e.to_string())?;
            }
        }
        let mut st = self.0.state.lock().unwrap();
        st.hashes.remove(&file);
        st.layers.remove(&file);
        Ok(())
    }

    // ───────────── serving files to peers ─────────────

    async fn serve(
        &self,
        _caller: String,
        mut sec: crate::crypto::SecureStream<tokio::net::TcpStream>,
    ) {
        loop {
            let Ok(Some(raw)) = sec.reader.recv_msg().await else {
                return;
            };
            let Ok(req) = serde_json::from_slice::<Req>(&raw) else {
                return;
            };
            let reply: Vec<u8> = match self.answer(&req).await {
                Ok((hdr, data)) => {
                    let mut v = hdr.into_bytes();
                    v.push(b'\n');
                    v.extend_from_slice(&data);
                    v
                }
                Err(e) => {
                    format!("{}\n", serde_json::json!({ "ok": false, "error": e })).into_bytes()
                }
            };
            if sec.writer.send_msg(&reply).await.is_err() {
                return;
            }
        }
    }

    async fn answer(&self, req: &Req) -> Result<(String, Vec<u8>), String> {
        let file = safe_name(&req.file)?;
        match req.op.as_str() {
            "stat" => {
                let path = self.path(&file);
                let size = tokio::fs::metadata(&path)
                    .await
                    .map(|m| m.len())
                    .map_err(|_| "no such file".to_string())?;
                let sha = self
                    .scan()
                    .into_iter()
                    .find(|m| m.file == file)
                    .map(|m| m.sha256)
                    .unwrap_or_default();
                Ok((
                    serde_json::json!({ "ok": true, "size": size, "sha256": sha }).to_string(),
                    vec![],
                ))
            }
            "read" => {
                if req.len == 0 || req.len > CHUNK {
                    return Err("bad length".into());
                }
                let data = self.read_range(&file, req.offset, req.len).await?;
                Ok((
                    serde_json::json!({ "ok": true, "len": data.len() }).to_string(),
                    data,
                ))
            }
            _ => Err("unknown request".into()),
        }
    }

    async fn read_range(&self, file: &str, offset: u64, len: u64) -> Result<Vec<u8>, String> {
        let final_path = self.path(file);
        let part_path = self.path(&format!("{file}.part"));
        let (path, check) = if self.0.state.lock().unwrap().active.contains_key(file) {
            (
                part_path.clone(),
                self.0.state.lock().unwrap().active.get(file).cloned(),
            )
        } else if final_path.exists() {
            (final_path, None)
        } else {
            // The transfer may have just finished and been renamed.
            (part_path, None)
        };
        if let Some(a) = check {
            let first = (offset / CHUNK) as usize;
            let last = ((offset + len - 1) / CHUNK) as usize;
            let done = a.done.lock().unwrap();
            if (first..=last).any(|i| !done.get(i).copied().unwrap_or(false)) {
                return Err("piece not ready".into());
            }
        }
        tokio::task::spawn_blocking(move || {
            let mut f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            let size = f.metadata().map_err(|e| e.to_string())?.len();
            if offset >= size {
                return Err("past the end".to_string());
            }
            let n = len.min(size - offset) as usize;
            f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; n];
            f.read_exact(&mut buf).map_err(|e| e.to_string())?;
            Ok(buf)
        })
        .await
        .map_err(|e| e.to_string())?
    }

    // ───────────── getting files ─────────────

    fn peer_sources(&self, file: &str) -> Vec<PeerSrc> {
        let me = self.0.mesh.id();
        self.0.mesh.with_registry(|reg, now| {
            reg.live(now)
                .into_iter()
                .filter(|e| e.record.node_id != me)
                .filter_map(|e| {
                    if let Some(m) = e.record.models.iter().find(|m| m.file == file) {
                        return Some(PeerSrc {
                            id: e.record.node_id.clone(),
                            name: e.record.name.clone(),
                            size: m.size,
                            sha: m.sha256.clone(),
                            partial: None,
                        });
                    }
                    let t = e.record.transfers.iter().find(|t| t.file == file)?;
                    if t.chunk_size != CHUNK || t.total == 0 {
                        return None;
                    }
                    Some(PeerSrc {
                        id: e.record.node_id.clone(),
                        name: e.record.name.clone(),
                        size: t.total,
                        sha: String::new(),
                        partial: Some(decode_bitmap(&t.have, n_chunks(t.total))),
                    })
                })
                .collect()
        })
    }

    /// Size and expected hash from a download link's headers, without downloading.
    async fn head(&self, url: &str) -> (Option<u64>, Option<String>) {
        // `Response::content_length` reports the (empty) body of a HEAD reply, so read the header.
        fn header_len(r: &reqwest::Response) -> Option<u64> {
            r.headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
        }
        let mut size = None;
        let mut sha = None;
        if let Ok(r) = self.0.no_redirect.head(url).send().await {
            let h = r.headers();
            let get = |k: &str| {
                h.get(k)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string())
            };
            size = get("x-linked-size").and_then(|s| s.parse().ok());
            let etag = get("x-linked-etag").or_else(|| get("etag"));
            sha = etag
                .map(|e| {
                    e.trim_matches('"')
                        .trim_start_matches("W/")
                        .trim_matches('"')
                        .to_string()
                })
                .filter(|e| e.len() == 64 && e.chars().all(|c| c.is_ascii_hexdigit()));
            if size.is_none() && r.status().is_success() {
                size = header_len(&r);
            }
            if size.is_none() && r.status().is_redirection() {
                if let Some(loc) = get("location") {
                    if let Ok(r2) = self.0.http.head(&loc).send().await {
                        size = header_len(&r2);
                    }
                }
            }
        }
        (size.filter(|s| *s > 0), sha)
    }

    /// Gets `file` into the models folder: from peers first, from `url` for the rest, and checks
    /// it against `expect_sha` (or a hash learned from the source) before keeping it.
    pub async fn fetch(
        &self,
        file: &str,
        url: Option<&str>,
        expect_sha: Option<&str>,
        size_hint: Option<u64>,
    ) -> Result<(), String> {
        let file = safe_name(file)?;
        let dest = self.path(&file);
        if dest.exists() {
            return Ok(());
        }
        if let Some(u) = url {
            if !link_allowed(u) {
                return Err("Only https:// links can be downloaded".into());
            }
        }
        if self.0.state.lock().unwrap().active.contains_key(&file) {
            return Err("That model is already being downloaded".into());
        }

        let peers = self.peer_sources(&file);
        let mut size = size_hint.or_else(|| peers.iter().map(|p| p.size).find(|s| *s > 0));
        let mut sha = expect_sha
            .map(|s| s.to_ascii_lowercase())
            .filter(|s| !s.is_empty());
        if sha.is_none() {
            // Complete peers advertise a hash; trust it only when they all agree.
            let hashes: BTreeSet<&str> = peers
                .iter()
                .filter(|p| p.partial.is_none() && !p.sha.is_empty())
                .map(|p| p.sha.as_str())
                .collect();
            if hashes.len() == 1 {
                sha = hashes.iter().next().map(|s| s.to_string());
            }
        }
        if let Some(u) = url {
            if size.is_none() || sha.is_none() {
                let (s, h) = self.head(u).await;
                size = size.or(s);
                sha = sha.or(h);
            }
        }
        let size = size.ok_or_else(|| {
            if url.is_none() && peers.is_empty() {
                "No device in the cluster has this model, and no download link was given"
                    .to_string()
            } else {
                "Could not find out how big this model is".to_string()
            }
        })?;
        if peers.is_empty() && url.is_none() {
            return Err(
                "No device in the cluster has this model, and no download link was given".into(),
            );
        }

        let n = n_chunks(size);
        let part = self.path(&format!("{file}.part"));
        let map = self.path(&format!("{file}.part.map"));
        let done = load_map(&part, &map, size, n);
        let already: u64 = done
            .iter()
            .enumerate()
            .filter(|(_, d)| **d)
            .map(|(i, _)| chunk_range(size, i).1)
            .sum();
        {
            // Make the file the right size now, so pieces can be written in any order.
            let f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&part)
                .map_err(|e| format!("Could not create the download file: {e}"))?;
            f.set_len(size).map_err(|e| {
                format!("Could not reserve {:.1} GB on disk: {e}", size as f64 / 1e9)
            })?;
        }
        let active = Arc::new(Active {
            size,
            done: Mutex::new(done),
            claimed: Mutex::new(HashSet::new()),
            bytes: AtomicU64::new(already),
            cancel: AtomicBool::new(false),
            from: Mutex::new(BTreeSet::new()),
            bad_peers: Mutex::new(HashSet::new()),
        });
        self.0
            .state
            .lock()
            .unwrap()
            .active
            .insert(file.clone(), active.clone());
        self.publish();

        let result = self
            .run_fetch(&file, url, size, &part, &map, active.clone())
            .await;

        let finish = match result {
            Ok(()) => self.finish(&file, &part, &map, &dest, sha.as_deref()).await,
            Err(e) => {
                save_map(&map, &active.done.lock().unwrap(), size);
                Err(e)
            }
        };
        self.0.state.lock().unwrap().active.remove(&file);
        self.publish();
        finish
    }

    fn publish(&self) {
        let t = self.transfers();
        self.0.mesh.update_self(|r| {
            if r.transfers == t {
                return false;
            }
            r.transfers = t;
            true
        });
    }

    async fn finish(
        &self,
        file: &str,
        part: &Path,
        map: &Path,
        dest: &Path,
        expect: Option<&str>,
    ) -> Result<(), String> {
        let p2 = part.to_path_buf();
        let got = tokio::task::spawn_blocking(move || sha256_file(&p2))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        if let Some(want) = expect {
            if got != want {
                let _ = std::fs::remove_file(part);
                let _ = std::fs::remove_file(map);
                return Err(format!(
                    "The downloaded file does not match its expected hash (got {}…, expected {}…). It was deleted.",
                    &got[..12],
                    &want[..12.min(want.len())]
                ));
            }
        }
        std::fs::rename(part, dest).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(map);
        let size = dest.metadata().map(|m| m.len()).unwrap_or(0);
        let mut st = self.0.state.lock().unwrap();
        st.hashes
            .insert(file.to_string(), (size, mtime_secs(dest), got));
        Ok(())
    }

    async fn run_fetch(
        &self,
        file: &str,
        url: Option<&str>,
        size: u64,
        part: &Path,
        map: &Path,
        active: Arc<Active>,
    ) -> Result<(), String> {
        let n = n_chunks(size);
        let out = Arc::new(Mutex::new(
            std::fs::OpenOptions::new()
                .write(true)
                .open(part)
                .map_err(|e| e.to_string())?,
        ));
        let me = self.0.mesh.id();
        // Where to start on the internet, so devices fetching together take different parts.
        let start = {
            let h = Sha256::digest(me.as_bytes());
            (u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize) % n.max(1)
        };
        let mut stalled_rounds = 0;
        let mut net_dead = url.is_none();
        let mut net_error = String::new();
        loop {
            let remaining = active.done.lock().unwrap().iter().filter(|d| !**d).count();
            if remaining == 0 {
                return Ok(());
            }
            if active.cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let before = remaining;
            let peers = self.peer_sources(file);
            let mut lanes = Vec::new();
            let bad = active.bad_peers.lock().unwrap().clone();
            for p in peers
                .iter()
                .filter(|p| p.size == size && !bad.contains(&p.id))
            {
                for _ in 0..PEER_LANES {
                    let this = self.clone();
                    let (a, o, p) = (active.clone(), out.clone(), p.clone());
                    let f = file.to_string();
                    lanes.push(tokio::spawn(async move {
                        this.peer_lane(&f, p, a, o).await;
                    }));
                }
            }
            if let (Some(u), false) = (url, net_dead) {
                for _ in 0..NET_LANES {
                    let this = self.clone();
                    let (a, o, u) = (active.clone(), out.clone(), u.to_string());
                    let f = file.to_string();
                    lanes.push(tokio::spawn(async move {
                        this.net_lane(&f, &u, start, a, o).await
                    }));
                }
            }
            if lanes.is_empty() {
                return Err(if net_error.is_empty() {
                    "No device has the rest of this model and there is no download link".into()
                } else {
                    net_error
                });
            }
            let saver = {
                let (m, a) = (map.to_path_buf(), active.clone());
                let this = self.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_millis(700)).await;
                        save_map(&m, &a.done.lock().unwrap(), a.size);
                        this.publish();
                    }
                })
            };
            for l in lanes {
                let _ = l.await;
            }
            saver.abort();
            save_map(map, &active.done.lock().unwrap(), size);

            let remaining = active.done.lock().unwrap().iter().filter(|d| !**d).count();
            if remaining == 0 {
                return Ok(());
            }
            if remaining >= before {
                stalled_rounds += 1;
                if url.is_some() && stalled_rounds >= 2 {
                    net_dead = true;
                    net_error = "The download link stopped working (the server must support \
                                 range requests and stay reachable)"
                        .into();
                }
                if stalled_rounds >= 4 {
                    return Err(if net_error.is_empty() {
                        "Could not get the whole model from the devices that have it".into()
                    } else {
                        net_error
                    });
                }
                tokio::time::sleep(Duration::from_millis(800)).await;
            } else {
                stalled_rounds = 0;
            }
        }
    }

    /// Picks the next piece a source can supply and marks it taken.
    fn claim(&self, a: &Active, ok: impl Fn(usize) -> bool, start: usize) -> Option<usize> {
        let done = a.done.lock().unwrap();
        let mut claimed = a.claimed.lock().unwrap();
        let n = done.len();
        (0..n)
            .map(|k| (start + k) % n)
            .find(|&i| !done[i] && !claimed.contains(&i) && ok(i))
            .inspect(|&i| {
                claimed.insert(i);
            })
    }

    fn store_chunk(
        &self,
        a: &Active,
        out: &Arc<Mutex<std::fs::File>>,
        i: usize,
        data: &[u8],
        from: &str,
    ) -> Result<(), String> {
        let (start, len) = chunk_range(a.size, i);
        if data.len() as u64 != len {
            return Err("a piece arrived with the wrong length".into());
        }
        {
            let mut f = out.lock().unwrap();
            f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
            f.write_all(data).map_err(|e| e.to_string())?;
        }
        a.done.lock().unwrap()[i] = true;
        a.claimed.lock().unwrap().remove(&i);
        a.bytes.fetch_add(len, Ordering::Relaxed);
        a.from.lock().unwrap().insert(from.to_string());
        Ok(())
    }

    fn unclaim(a: &Active, i: usize) {
        a.claimed.lock().unwrap().remove(&i);
    }

    async fn peer_lane(
        &self,
        file: &str,
        peer: PeerSrc,
        a: Arc<Active>,
        out: Arc<Mutex<std::fs::File>>,
    ) {
        let Ok(mut sec) = self.0.mesh.open_tunnel(&peer.id, "files").await else {
            a.bad_peers.lock().unwrap().insert(peer.id.clone());
            return;
        };
        let mut fails = 0;
        loop {
            if a.cancel.load(Ordering::Relaxed) {
                return;
            }
            if fails >= MAX_FAILS {
                a.bad_peers.lock().unwrap().insert(peer.id.clone());
                return;
            }
            // Pieces this peer has, as of its latest record.
            let have = self
                .peer_sources(file)
                .into_iter()
                .find(|p| p.id == peer.id)
                .map(|p| p.partial);
            let Some(have) = have else { return };
            let Some(i) = self.claim(
                &a,
                |i| {
                    have.as_ref()
                        .is_none_or(|h| h.get(i).copied().unwrap_or(false))
                },
                0,
            ) else {
                return;
            };
            let (start, len) = chunk_range(a.size, i);
            let req =
                serde_json::json!({ "op": "read", "file": file, "offset": start, "len": len });
            let ok = async {
                sec.writer
                    .send_msg(req.to_string().as_bytes())
                    .await
                    .map_err(|e| e.to_string())?;
                let resp = tokio::time::timeout(Duration::from_secs(60), sec.reader.recv_msg())
                    .await
                    .map_err(|_| "timed out".to_string())?
                    .map_err(|e| e.to_string())?
                    .ok_or("closed")?;
                let nl = resp.iter().position(|b| *b == b'\n').ok_or("bad reply")?;
                let hdr: serde_json::Value =
                    serde_json::from_slice(&resp[..nl]).map_err(|e| e.to_string())?;
                if hdr["ok"] != true {
                    return Err(hdr["error"].as_str().unwrap_or("refused").to_string());
                }
                self.store_chunk(&a, &out, i, &resp[nl + 1..], &peer.name)
            }
            .await;
            if ok.is_err() {
                Self::unclaim(&a, i);
                fails += 1;
                if fails < MAX_FAILS {
                    // The link may be broken: reconnect before trying again.
                    match self.0.mesh.open_tunnel(&peer.id, "files").await {
                        Ok(s) => sec = s,
                        Err(_) => {
                            a.bad_peers.lock().unwrap().insert(peer.id.clone());
                            return;
                        }
                    }
                }
            }
        }
    }

    async fn net_lane(
        &self,
        file: &str,
        url: &str,
        start: usize,
        a: Arc<Active>,
        out: Arc<Mutex<std::fs::File>>,
    ) {
        let mut fails = 0;
        loop {
            if a.cancel.load(Ordering::Relaxed) || fails >= MAX_FAILS {
                return;
            }
            // Leave pieces a nearby device has (or will soon have) to that device: only fetch
            // from the internet what nobody on the network can supply.
            let peers = self.peer_sources(file);
            let bad = a.bad_peers.lock().unwrap().clone();
            let held = |i: usize| {
                peers.iter().any(|p| {
                    p.size == a.size
                        && !bad.contains(&p.id)
                        && p.partial
                            .as_ref()
                            .is_none_or(|h| h.get(i).copied().unwrap_or(false))
                })
            };
            let Some(i) = self.claim(&a, |i| !held(i), start) else {
                return;
            };
            let (s, len) = chunk_range(a.size, i);
            let res = async {
                let r = self
                    .0
                    .http
                    .get(url)
                    .header("Range", format!("bytes={}-{}", s, s + len - 1))
                    .timeout(Duration::from_secs(120))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                if r.status().as_u16() != 206 {
                    return Err(format!(
                        "the server answered {} to a range request",
                        r.status()
                    ));
                }
                let bytes = r.bytes().await.map_err(|e| e.to_string())?;
                self.store_chunk(&a, &out, i, &bytes, "internet")
            }
            .await;
            if res.is_err() {
                Self::unclaim(&a, i);
                fails += 1;
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
    }
}

// ───────────────────────────── resume ─────────────────────────────

#[derive(Serialize, Deserialize)]
struct MapFile {
    size: u64,
    chunk: u64,
    done: Vec<usize>,
}

fn save_map(path: &Path, done: &[bool], size: u64) {
    let m = MapFile {
        size,
        chunk: CHUNK,
        done: done
            .iter()
            .enumerate()
            .filter(|(_, d)| **d)
            .map(|(i, _)| i)
            .collect(),
    };
    if let Ok(text) = serde_json::to_string(&m) {
        let _ = std::fs::write(path, text);
    }
}

fn load_map(part: &Path, map: &Path, size: u64, n: usize) -> Vec<bool> {
    let mut done = vec![false; n];
    let part_ok = part.metadata().is_ok_and(|m| m.len() == size);
    if !part_ok {
        return done;
    }
    if let Some(m) = std::fs::read_to_string(map)
        .ok()
        .and_then(|t| serde_json::from_str::<MapFile>(&t).ok())
    {
        if m.size == size && m.chunk == CHUNK {
            for i in m.done {
                if i < n {
                    done[i] = true;
                }
            }
        }
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_or_this_machine_may_be_downloaded_from() {
        assert!(link_allowed("https://huggingface.co/x/y.gguf"));
        assert!(link_allowed("http://127.0.0.1:8080/m.gguf"));
        assert!(link_allowed("http://localhost/m.gguf"));
        assert!(!link_allowed("http://example.com/m.gguf"));
        assert!(!link_allowed("http://127.0.0.1.evil.com/m.gguf"));
        assert!(!link_allowed("http://localhost.evil.com/m.gguf"));
        assert!(!link_allowed("ftp://127.0.0.1/m.gguf"));
    }

    #[test]
    fn bitmaps_round_trip() {
        let v = vec![true, false, false, true, true, false, false, false, true];
        let text = encode_bitmap(&v);
        assert_eq!(decode_bitmap(&text, v.len()), v);
        assert_eq!(decode_bitmap("zz", 3), vec![false; 3]);
    }

    #[test]
    fn pieces_cover_the_file_exactly() {
        let size = CHUNK * 2 + 123;
        assert_eq!(n_chunks(size), 3);
        assert_eq!(chunk_range(size, 0), (0, CHUNK));
        assert_eq!(chunk_range(size, 2), (CHUNK * 2, 123));
        let total: u64 = (0..3).map(|i| chunk_range(size, i).1).sum();
        assert_eq!(total, size);
    }

    #[test]
    fn file_names_cannot_escape_the_folder() {
        assert!(safe_name("model.gguf").is_ok());
        assert!(safe_name("../model.gguf").is_err());
        assert!(safe_name("a/b.gguf").is_err());
        assert!(safe_name("a\\b.gguf").is_err());
        assert!(safe_name("C:evil.gguf").is_err());
        assert!(safe_name("model.bin").is_err());
        assert!(safe_name("").is_err());
    }

    #[test]
    fn resume_only_trusts_a_matching_part_file() {
        let dir = std::env::temp_dir().join(format!("lanlm-map-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("m.gguf.part");
        let map = dir.join("m.gguf.part.map");
        let size = CHUNK * 3;
        save_map(&map, &[true, false, true], size);
        assert_eq!(load_map(&part, &map, size, 3), vec![false; 3]); // no part file
        std::fs::File::create(&part).unwrap().set_len(size).unwrap();
        assert_eq!(load_map(&part, &map, size, 3), vec![true, false, true]);
        assert_eq!(load_map(&part, &map, size + 1, 3), vec![false; 3]); // different size
        let _ = std::fs::remove_dir_all(dir);
    }
}

//! The mesh node: identity, cluster membership, the shared table and the API the rest of the
//! program uses. The network side (listening, dialling, sessions, tunnels) is in `link.rs`.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{broadcast, oneshot, watch};

use crate::crypto::{ClusterKey, Identity};
use crate::proto::{now_ms, Command, Msg, NodeRecord};
use crate::registry::{NodeView, Registry};

pub const DEFAULT_PORT: u16 = 50500;
/// How long a pairing window or a pairing request stays open.
pub const PAIR_WINDOW: Duration = Duration::from_secs(120);

pub struct MeshConfig {
    /// Where the identity, the cluster key and settings are kept.
    pub data_dir: PathBuf,
    /// Device name; the computer's name when `None`.
    pub name: Option<String>,
    /// First port to try for the mesh listener.
    pub port: u16,
    pub version: String,
    /// Announce this node on the local network (mDNS) and look for others.
    pub discovery: bool,
}

impl MeshConfig {
    pub fn new(data_dir: PathBuf) -> MeshConfig {
        MeshConfig {
            data_dir,
            name: None,
            port: DEFAULT_PORT,
            version: env!("CARGO_PKG_VERSION").to_string(),
            discovery: true,
        }
    }
}

/// Runs a command that another node sent us.
pub type CmdFuture = BoxFuture<'static, Result<Value, String>>;
pub type Executor = Arc<dyn Fn(String, Command) -> CmdFuture + Send + Sync>;

/// Handles an encrypted stream opened to a named service (used where a plain TCP port would be
/// the wrong shape, like the model file service).
pub type StreamHandler = Arc<
    dyn Fn(String, crate::crypto::SecureStream<tokio::net::TcpStream>) -> BoxFuture<'static, ()>
        + Send
        + Sync,
>;

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
struct Persist {
    name: String,
    private: String,
    public: String,
    cluster_key: String,
    cluster_name: String,
    known_addrs: Vec<String>,
    lend: bool,
    extra: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug)]
pub enum Event {
    PeerUp { id: String, name: String },
    PeerDown { id: String, name: String },
    PairRequested { name: String, sas: String },
    Joined,
    Left,
    Log(String),
}

/// A device waiting for us to allow it into the cluster.
pub struct PairRequest {
    pub id: u64,
    pub remote_id: String,
    pub name: String,
    pub sas: String,
    pub created: Instant,
    pub decide: Option<oneshot::Sender<bool>>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum PairStatus {
    #[default]
    Idle,
    Connecting,
    /// Both screens show this code; the host has to press Allow.
    Waiting {
        sas: String,
    },
    Joined,
    Denied,
    Failed {
        error: String,
    },
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PairRequestView {
    pub id: u64,
    pub name: String,
    pub remote_id: String,
    pub sas: String,
    pub age_ms: u64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub me: String,
    pub name: String,
    pub joined: bool,
    pub cluster_id: String,
    pub cluster_name: String,
    pub port: u16,
    pub addrs: Vec<String>,
    pub nodes: Vec<NodeView>,
    pub pairing_open: bool,
    pub nearby: Vec<crate::discovery::Nearby>,
    pub pair_requests: Vec<PairRequestView>,
    pub pair_status: PairStatus,
}

/// A local doorway to a service on another node, and the task that serves it.
pub(crate) type Forward = (SocketAddr, tokio::task::JoinHandle<()>);

pub(crate) struct PeerLink {
    pub tx: tokio::sync::mpsc::UnboundedSender<Msg>,
    /// Node id of the side that opened this connection.
    pub initiator: String,
    pub conn: u64,
}

pub(crate) struct Inner {
    pub id: Identity,
    pub cfg: MeshConfig,
    persist: Mutex<Persist>,
    key: RwLock<Option<ClusterKey>>,
    pub reg: Mutex<Registry>,
    pub peers: Mutex<HashMap<String, PeerLink>>,
    pub dial_state: Mutex<HashMap<String, (Instant, u32)>>,
    pub known_addrs: Mutex<HashSet<String>>,
    /// Which device answered at an address we dialled.
    pub addr_owner: Mutex<HashMap<String, String>>,
    pub services: Mutex<HashMap<String, SocketAddr>>,
    pub streams: Mutex<HashMap<String, StreamHandler>>,
    pub forwards: Mutex<HashMap<(String, String), Forward>>,
    pub executor: RwLock<Option<Executor>>,
    /// Bumped on every change to our own record so sessions send it at once.
    pub changed: watch::Sender<u64>,
    /// Bumped when the cluster key changes (discovery restarts, links are rebuilt).
    pub membership: watch::Sender<u64>,
    /// Bumped when the cluster key changes; every control session of an older one must end.
    pub epoch: watch::Sender<u64>,
    pub pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    pub next_req: AtomicU64,
    pub next_conn: AtomicU64,
    pub port: AtomicU64,
    pub pair_open_until: Mutex<Option<Instant>>,
    pub pair_in: Mutex<Vec<PairRequest>>,
    pub pair_out: Mutex<PairStatus>,
    pub nearby: Mutex<crate::discovery::NearbyList>,
    pub events: broadcast::Sender<Event>,
}

/// A handle to this node. Cheap to clone.
#[derive(Clone)]
pub struct Mesh(pub(crate) Arc<Inner>);

fn host_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "this-pc".into())
}

fn load_persist(dir: &std::path::Path) -> Persist {
    std::fs::read_to_string(dir.join("mesh.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_persist(dir: &std::path::Path, p: &Persist) {
    let _ = std::fs::create_dir_all(dir);
    let tmp = dir.join("mesh.json.tmp");
    if let Ok(text) = serde_json::to_string_pretty(p) {
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, dir.join("mesh.json"));
        }
    }
}

impl Mesh {
    /// Loads (or creates) this node's identity and starts listening. Discovery and the
    /// background loops start here too.
    pub async fn start(cfg: MeshConfig) -> Result<Mesh, String> {
        let mut p = load_persist(&cfg.data_dir);
        let fresh = p.private.is_empty();
        let id = if fresh {
            let id = Identity::generate();
            p.private = id.private_hex();
            p.public = id.node_id();
            p.lend = true;
            id
        } else {
            Identity::from_hex(&p.private, &p.public)?
        };
        if let Some(n) = cfg.name.as_ref().filter(|n| !n.trim().is_empty()) {
            p.name = n.trim().to_string();
        }
        if p.name.is_empty() {
            p.name = host_name();
        }
        let key = ClusterKey::parse(&p.cluster_key).ok();

        let listener = crate::link::bind(cfg.port).await?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();

        let mut me = NodeRecord {
            node_id: id.node_id(),
            name: p.name.clone(),
            cluster_id: key.as_ref().map(|k| k.cluster_id()).unwrap_or_default(),
            seq: now_ms(),
            version: cfg.version.clone(),
            os: std::env::consts::OS.to_string(),
            lend: p.lend,
            ..Default::default()
        };
        me.addrs = crate::link::local_addrs(port);
        let known: HashSet<String> = p.known_addrs.iter().cloned().collect();

        let (events, _) = broadcast::channel(256);
        let inner = Inner {
            id,
            persist: Mutex::new(p),
            key: RwLock::new(key),
            reg: Mutex::new(Registry::new(me, Instant::now())),
            peers: Mutex::new(HashMap::new()),
            dial_state: Mutex::new(HashMap::new()),
            known_addrs: Mutex::new(known),
            addr_owner: Mutex::new(HashMap::new()),
            services: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
            forwards: Mutex::new(HashMap::new()),
            executor: RwLock::new(None),
            changed: watch::channel(0).0,
            membership: watch::channel(0).0,
            epoch: watch::channel(0).0,
            pending: Mutex::new(HashMap::new()),
            next_req: AtomicU64::new(1),
            next_conn: AtomicU64::new(1),
            port: AtomicU64::new(port as u64),
            pair_open_until: Mutex::new(None),
            pair_in: Mutex::new(Vec::new()),
            pair_out: Mutex::new(PairStatus::Idle),
            nearby: Mutex::new(Default::default()),
            events,
            cfg,
        };
        let mesh = Mesh(Arc::new(inner));
        if fresh {
            mesh.save();
        }
        crate::link::spawn_all(&mesh, listener);
        if mesh.0.cfg.discovery {
            crate::discovery::spawn(&mesh);
        }
        Ok(mesh)
    }

    fn save(&self) {
        let p = self.0.persist.lock().unwrap().clone();
        save_persist(&self.0.cfg.data_dir, &p);
    }

    // ───────────── identity and membership ─────────────

    pub fn id(&self) -> String {
        self.0.id.node_id()
    }

    pub fn name(&self) -> String {
        self.0.persist.lock().unwrap().name.clone()
    }

    pub fn port(&self) -> u16 {
        self.0.port.load(Ordering::Relaxed) as u16
    }

    pub fn data_dir(&self) -> PathBuf {
        self.0.cfg.data_dir.clone()
    }

    pub fn set_name(&self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.0.persist.lock().unwrap().name = name.to_string();
        self.save();
        self.update_self(|r| {
            r.name = name.to_string();
            true
        });
    }

    pub fn joined(&self) -> bool {
        self.0.key.read().unwrap().is_some()
    }

    pub fn cluster_id(&self) -> String {
        self.0
            .key
            .read()
            .unwrap()
            .as_ref()
            .map(|k| k.cluster_id())
            .unwrap_or_default()
    }

    pub fn cluster_name(&self) -> String {
        self.0.persist.lock().unwrap().cluster_name.clone()
    }

    /// The text other devices need to join this cluster.
    pub fn cluster_key_text(&self) -> Option<String> {
        self.0.key.read().unwrap().as_ref().map(|k| k.encode())
    }

    pub(crate) fn psk(&self) -> Option<[u8; 32]> {
        self.0.key.read().unwrap().as_ref().map(|k| k.psk())
    }

    /// Starts a new cluster with a fresh key and returns the key to share.
    pub fn create_cluster(&self, name: &str) -> String {
        let key = ClusterKey::generate();
        let text = key.encode();
        self.set_key(Some(key), name);
        text
    }

    pub fn join_cluster(&self, key_text: &str, cluster_name: &str) -> Result<(), String> {
        let key = ClusterKey::parse(key_text)?;
        self.set_key(Some(key), cluster_name);
        Ok(())
    }

    /// Leaves the cluster: forgets the key and drops every link.
    pub fn leave_cluster(&self) {
        self.set_key(None, "");
    }

    fn set_key(&self, key: Option<ClusterKey>, name: &str) {
        {
            let mut p = self.0.persist.lock().unwrap();
            p.cluster_key = key.as_ref().map(|k| k.encode()).unwrap_or_default();
            p.cluster_name = name.to_string();
            if key.is_none() {
                p.known_addrs.clear();
            }
        }
        let cid = key.as_ref().map(|k| k.cluster_id()).unwrap_or_default();
        let joined = key.is_some();
        *self.0.key.write().unwrap() = key;
        self.save();
        // Old links were made with the old key: drop them, and forget the old cluster's nodes.
        for (_, link) in self.0.peers.lock().unwrap().drain() {
            let _ = link.tx.send(Msg::Bye);
        }
        {
            let mut known = self.0.known_addrs.lock().unwrap();
            known.clear();
        }
        {
            let mut reg = self.0.reg.lock().unwrap();
            let mut mine = reg.my_record().clone();
            mine.cluster_id = cid;
            mine.seq += 1;
            let fresh = Registry::new(mine, Instant::now());
            *reg = fresh;
        }
        self.0.changed.send_modify(|v| *v += 1);
        self.0.membership.send_modify(|v| *v += 1);
        // Every session made with the old key ends, whatever its state.
        self.0.epoch.send_modify(|v| *v += 1);
        self.emit(if joined { Event::Joined } else { Event::Left });
    }

    // ───────────── the shared table ─────────────

    /// Changes our own record. The closure returns whether it changed anything; only then is a
    /// new version published to the cluster.
    pub fn update_self(&self, f: impl FnOnce(&mut NodeRecord) -> bool) -> bool {
        let changed = {
            let mut reg = self.0.reg.lock().unwrap();
            let mut r = reg.my_record().clone();
            if !f(&mut r) {
                return false;
            }
            r.seq = r.seq.max(now_ms()).max(reg.my_record().seq + 1);
            reg.set_self(r, Instant::now());
            true
        };
        if changed {
            self.0.changed.send_modify(|v| *v += 1);
        }
        changed
    }

    pub fn my_record(&self) -> NodeRecord {
        self.0.reg.lock().unwrap().my_record().clone()
    }

    /// Reads the table while holding its lock briefly.
    pub fn with_registry<T>(&self, f: impl FnOnce(&Registry, Instant) -> T) -> T {
        let reg = self.0.reg.lock().unwrap();
        f(&reg, Instant::now())
    }

    pub fn snapshot(&self) -> Snapshot {
        let now = Instant::now();
        let nodes = self.0.reg.lock().unwrap().views(now);
        let pairing_open = self
            .0
            .pair_open_until
            .lock()
            .unwrap()
            .is_some_and(|t| t > now);
        let pair_requests = self
            .0
            .pair_in
            .lock()
            .unwrap()
            .iter()
            .map(|r| PairRequestView {
                id: r.id,
                name: r.name.clone(),
                remote_id: r.remote_id.clone(),
                sas: r.sas.clone(),
                age_ms: now.saturating_duration_since(r.created).as_millis() as u64,
            })
            .collect();
        Snapshot {
            me: self.id(),
            name: self.name(),
            joined: self.joined(),
            cluster_id: self.cluster_id(),
            cluster_name: self.cluster_name(),
            port: self.port(),
            addrs: crate::link::local_addrs(self.port()),
            nodes,
            pairing_open,
            nearby: self.nearby(),
            pair_requests,
            pair_status: self.0.pair_out.lock().unwrap().clone(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.0.events.subscribe()
    }

    pub(crate) fn emit(&self, e: Event) {
        let _ = self.0.events.send(e);
    }

    pub fn log(&self, text: impl Into<String>) {
        self.emit(Event::Log(text.into()));
    }

    // ───────────── commands between nodes ─────────────

    /// Installs the function that runs commands other nodes send us.
    pub fn set_executor(&self, ex: Executor) {
        *self.0.executor.write().unwrap() = Some(ex);
    }

    pub(crate) fn executor(&self) -> Option<Executor> {
        self.0.executor.read().unwrap().clone()
    }

    /// Runs `cmd` on `node` (here, or over the link to it) and returns its answer.
    pub async fn call(&self, node: &str, cmd: Command, wait: Duration) -> Result<Value, String> {
        if node == self.id() {
            let ex = self.executor().ok_or("this node cannot run commands yet")?;
            return ex(self.id(), cmd).await;
        }
        let tx = self
            .0
            .peers
            .lock()
            .unwrap()
            .get(node)
            .map(|l| l.tx.clone())
            .ok_or_else(|| "that device is not connected right now".to_string())?;
        let id = self.0.next_req.fetch_add(1, Ordering::Relaxed);
        let (otx, orx) = oneshot::channel();
        self.0.pending.lock().unwrap().insert(id, otx);
        if tx.send(Msg::Req { id, cmd }).is_err() {
            self.0.pending.lock().unwrap().remove(&id);
            return Err("that device just disconnected".into());
        }
        match tokio::time::timeout(wait, orx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("that device disconnected before it answered".into()),
            Err(_) => {
                self.0.pending.lock().unwrap().remove(&id);
                Err("that device did not answer in time".into())
            }
        }
    }

    // ───────────── services other nodes may tunnel to ─────────────

    /// Makes a local TCP service (the RPC worker, the model API) reachable by cluster members,
    /// and only by them, through an encrypted tunnel.
    pub fn register_service(&self, name: &str, addr: SocketAddr) {
        self.0
            .services
            .lock()
            .unwrap()
            .insert(name.to_string(), addr);
    }

    /// Offers a service that gets the encrypted stream itself. The handler receives the
    /// caller's node id and the stream.
    pub fn register_stream_service(&self, name: &str, handler: StreamHandler) {
        self.0
            .streams
            .lock()
            .unwrap()
            .insert(name.to_string(), handler);
    }

    pub fn unregister_service(&self, name: &str) {
        self.0.services.lock().unwrap().remove(name);
    }

    // ───────────── saved settings for other modules ─────────────

    pub fn setting<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        let p = self.0.persist.lock().unwrap();
        p.extra
            .get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) {
        if let Ok(v) = serde_json::to_value(value) {
            self.0
                .persist
                .lock()
                .unwrap()
                .extra
                .insert(key.to_string(), v);
            self.save();
        }
    }

    pub fn lend(&self) -> bool {
        self.0.persist.lock().unwrap().lend
    }

    pub fn set_lend(&self, on: bool) {
        self.0.persist.lock().unwrap().lend = on;
        self.save();
        self.update_self(|r| {
            let c = r.lend != on;
            r.lend = on;
            c
        });
    }

    /// Remembers an address so we dial it now and again after a restart.
    pub fn add_peer(&self, addr: &str) {
        let addr = addr.trim().to_string();
        if addr.is_empty() {
            return;
        }
        let added = self.0.known_addrs.lock().unwrap().insert(addr.clone());
        if added {
            let mut p = self.0.persist.lock().unwrap();
            if !p.known_addrs.contains(&addr) {
                p.known_addrs.push(addr);
            }
            drop(p);
            self.save();
        }
    }

    /// Addresses found by discovery: tried, but not saved.
    pub(crate) fn add_candidates(&self, addrs: impl IntoIterator<Item = String>) {
        let mut k = self.0.known_addrs.lock().unwrap();
        for a in addrs {
            k.insert(a);
        }
    }

    // ───────────── pairing ─────────────

    /// Lets devices ask to join for the next two minutes.
    pub fn open_pairing(&self) {
        *self.0.pair_open_until.lock().unwrap() = Some(Instant::now() + PAIR_WINDOW);
        // Discovery announces this device as open to new members.
        self.0.membership.send_modify(|v| *v += 1);
    }

    /// Devices nearby that are open to new members (only listed while this device is not in a
    /// cluster).
    pub fn nearby(&self) -> Vec<crate::discovery::Nearby> {
        self.0.nearby.lock().unwrap().list()
    }

    pub fn close_pairing(&self) {
        *self.0.pair_open_until.lock().unwrap() = None;
        self.0.membership.send_modify(|v| *v += 1);
        for mut r in self.0.pair_in.lock().unwrap().drain(..) {
            if let Some(tx) = r.decide.take() {
                let _ = tx.send(false);
            }
        }
    }

    pub(crate) fn pairing_is_open(&self) -> bool {
        self.0
            .pair_open_until
            .lock()
            .unwrap()
            .is_some_and(|t| t > Instant::now())
    }

    /// Allows or refuses a device that asked to join.
    pub fn pair_decide(&self, request: u64, allow: bool) -> bool {
        let mut list = self.0.pair_in.lock().unwrap();
        if let Some(pos) = list.iter().position(|r| r.id == request) {
            let mut r = list.remove(pos);
            if let Some(tx) = r.decide.take() {
                let _ = tx.send(allow);
                return true;
            }
        }
        false
    }

    pub(crate) fn key_for_pairing(&self) -> Option<(String, String)> {
        let k = self.cluster_key_text()?;
        Some((k, self.cluster_name()))
    }
}

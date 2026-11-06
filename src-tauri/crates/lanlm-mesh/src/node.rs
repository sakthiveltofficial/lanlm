//! A node: this machine's part in the cluster. It starts and watches the llama.cpp programs
//! (a model server, or an RPC worker holding part of someone else's model), keeps the cluster
//! table up to date with what this machine has and is doing, and runs commands from other nodes.

use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command as Proc;
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use tokio::task::JoinHandle;

use crate::engine;
use crate::hw;
use crate::mesh::{CmdFuture, Mesh};
use crate::proto::{
    now_ms, Bench, Command, Deployment, EngineInfo, InstanceInfo, InstanceSpec, InstanceState,
    LinkInfo, Share,
};
use crate::store::Store;

/// How long a model may take to load. Pipelines move gigabytes between devices at load time.
const LOAD_TIMEOUT: Duration = Duration::from_secs(45 * 60);

#[derive(Clone)]
pub struct NodeConfig {
    pub engine_dir: PathBuf,
    pub models_dir: PathBuf,
    /// Where an RPC worker keeps the weights it was sent, so the next load does not resend them.
    pub cache_dir: PathBuf,
}

/// A running program we can stop. Stopping it on purpose is not reported as a crash.
struct Running {
    kill: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl Running {
    async fn stop(mut self) {
        if let Some(k) = self.kill.take() {
            let _ = k.send(());
        }
        let _ = tokio::time::timeout(Duration::from_secs(10), self.handle).await;
    }
}

struct LlamaRun {
    id: String,
    proc: Running,
    port: u16,
    tasks: Vec<JoinHandle<()>>,
    rpc_nodes: Vec<String>,
}

struct RpcRun {
    instance: String,
    proc: Running,
}

#[derive(Default)]
struct Runtime {
    llama: Option<LlamaRun>,
    rpc: Option<RpcRun>,
    /// An instance or share that is starting (so a second start is refused, and can be cancelled).
    starting: Option<(String, Arc<AtomicBool>)>,
}

pub(crate) struct NodeInner {
    pub mesh: Mesh,
    pub cfg: NodeConfig,
    pub http: reqwest::Client,
    pub store: Store,
    rt: AsyncMutex<Runtime>,
    logs: Mutex<VecDeque<String>>,
    /// Deployments this node owns and keeps running.
    pub deployments: Mutex<Vec<Deployment>>,
    /// Nodes (and instance ids) that a launch in progress has already claimed.
    pub launching: Mutex<HashSet<String>>,
    pub orch: crate::orchestrator::Orch,
}

#[derive(Clone)]
pub struct Node(pub(crate) Arc<NodeInner>);

fn free_port() -> Result<u16, String> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    Ok(l.local_addr().map_err(|e| e.to_string())?.port())
}

fn pump<R: AsyncRead + Unpin + Send + 'static>(node: Node, label: String, reader: R) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            node.log(format!("[{label}] {line}"));
        }
    });
}

// ───────────────────────────── arguments ─────────────────────────────

/// The command line for `llama-server`. A pure function, so every mode is tested.
pub fn llama_args(
    spec: &InstanceSpec,
    model: &Path,
    mmproj: Option<&Path>,
    port: u16,
    rpc: &[SocketAddr],
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "-m".into(),
        model.display().to_string(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "-c".into(),
        spec.ctx.max(512).to_string(),
        "-t".into(),
        spec.threads.max(1).to_string(),
        "-np".into(),
        spec.parallel.max(1).to_string(),
        "--metrics".into(),
    ];
    if let Some(n) = spec.ngl {
        a.extend(["-ngl".into(), n.to_string()]);
    }
    if !rpc.is_empty() {
        a.extend([
            "--rpc".into(),
            rpc.iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(","),
        ]);
    }
    if !spec.tensor_split.is_empty() {
        a.extend([
            "-ts".into(),
            spec.tensor_split
                .iter()
                .map(|v| format!("{v:.3}"))
                .collect::<Vec<_>>()
                .join(","),
        ]);
    }
    if !rpc.is_empty() && !spec.pin_output.is_empty() {
        // The embedding and output matrices stay on the master, so a result the size of the
        // vocabulary never crosses the network for every token.
        a.extend([
            "-ot".into(),
            format!(r"^(output|token_embd)\.weight$={}", spec.pin_output),
        ]);
    }
    if let Some(p) = mmproj {
        a.extend(["--mmproj".into(), p.display().to_string()]);
    }
    a
}

/// What `llama-server --metrics` reports that the cluster cares about.
#[derive(Debug, Default, PartialEq, Clone)]
pub struct Metrics {
    pub processing: u32,
    pub deferred: u32,
    pub tok_s: f64,
    pub tokens_total: u64,
}

pub fn parse_metrics(text: &str) -> Metrics {
    let mut m = Metrics::default();
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(name), Some(val)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(v) = val.parse::<f64>() else { continue };
        match name {
            "llamacpp:requests_processing" => m.processing = v as u32,
            "llamacpp:requests_deferred" => m.deferred = v as u32,
            "llamacpp:predicted_tokens_seconds" => m.tok_s = (v * 10.0).round() / 10.0,
            "llamacpp:tokens_predicted_total" => m.tokens_total = v as u64,
            _ => {}
        }
    }
    m
}

fn instance_info(
    spec: &InstanceSpec,
    state: InstanceState,
    error: &str,
    started: u64,
) -> InstanceInfo {
    InstanceInfo {
        id: spec.id.clone(),
        deployment: spec.deployment.clone(),
        model: spec.model.clone(),
        kind: spec.kind.clone(),
        master: spec
            .stages
            .first()
            .map(|s| s.node_id.clone())
            .unwrap_or_default(),
        stages: spec.stages.clone(),
        state,
        error: error.to_string(),
        ctx: spec.ctx,
        slots: spec.parallel.max(1),
        started_ms: started,
        ..Default::default()
    }
}

// ───────────────────────────── the node ─────────────────────────────

impl Node {
    /// Starts the node: wires up command handling, the model service and the background loops.
    pub fn start(mesh: Mesh, cfg: NodeConfig, http: reqwest::Client) -> Node {
        let _ = std::fs::create_dir_all(&cfg.models_dir);
        let _ = std::fs::create_dir_all(&cfg.cache_dir);
        let store = Store::new(mesh.clone(), cfg.models_dir.clone(), http.clone());
        store.start_service();
        let deployments: Vec<Deployment> = mesh.setting("deployments").unwrap_or_default();
        let node = Node(Arc::new(NodeInner {
            mesh: mesh.clone(),
            cfg,
            http,
            store,
            rt: AsyncMutex::new(Runtime::default()),
            logs: Mutex::new(VecDeque::new()),
            deployments: Mutex::new(deployments),
            launching: Mutex::new(HashSet::new()),
            orch: crate::orchestrator::Orch::new(),
        }));
        let n = node.clone();
        mesh.set_executor(Arc::new(move |from, cmd| n.exec(from, cmd)));
        tokio::spawn(sampler(node.clone()));
        tokio::spawn(crate::orchestrator::reconcile_loop(node.clone()));
        node
    }

    pub fn mesh(&self) -> &Mesh {
        &self.0.mesh
    }

    pub fn store(&self) -> &Store {
        &self.0.store
    }

    pub fn log(&self, line: impl Into<String>) {
        let mut l = self.0.logs.lock().unwrap();
        l.push_back(line.into());
        while l.len() > 800 {
            l.pop_front();
        }
    }

    pub fn logs(&self) -> Vec<String> {
        self.0.logs.lock().unwrap().iter().cloned().collect()
    }

    pub fn server_exe(&self) -> Option<PathBuf> {
        engine::find_exe(&self.0.cfg.engine_dir, "llama-server")
    }

    fn threads(&self) -> u32 {
        self.0.mesh.my_record().hw.cores.max(1)
    }

    fn exec(&self, _from: String, cmd: Command) -> CmdFuture {
        let node = self.clone();
        Box::pin(async move {
            match cmd {
                Command::StartShare(s) => node.start_share(s).await.map(|_| json!({})),
                Command::StopShare { instance } => {
                    node.stop_share(&instance).await;
                    Ok(json!({}))
                }
                Command::StartInstance(spec) => node.start_instance(*spec).await,
                Command::StopInstance { instance } => {
                    node.stop_instance(&instance).await;
                    Ok(json!({}))
                }
                Command::Fetch {
                    file,
                    url,
                    sha256,
                    size,
                } => node
                    .0
                    .store
                    .fetch(&file, url.as_deref(), sha256.as_deref(), size)
                    .await
                    .map(|_| json!({})),
                Command::CancelFetch { file } => {
                    node.0.store.cancel(&file);
                    Ok(json!({}))
                }
                Command::DeleteModel { file } => node.0.store.delete(&file).map(|_| json!({})),
                Command::Bench => node.bench().await,
                Command::SetDraining { on } => {
                    node.0.mesh.update_self(|r| {
                        let c = r.draining != on;
                        r.draining = on;
                        c
                    });
                    Ok(json!({}))
                }
                Command::SetLend { on } => {
                    node.0.mesh.set_lend(on);
                    Ok(json!({}))
                }
                Command::ClearTensorCache => {
                    let _ = std::fs::remove_dir_all(&node.0.cfg.cache_dir);
                    let _ = std::fs::create_dir_all(&node.0.cfg.cache_dir);
                    Ok(json!({}))
                }
            }
        })
    }

    // ───────────── starting programs ─────────────

    fn spawn_proc(
        &self,
        label: &str,
        mut cmd: Proc,
        on_exit: impl FnOnce(String) + Send + 'static,
    ) -> Result<Running, String> {
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Could not start {label}: {e}"))?;
        if let Some(o) = child.stdout.take() {
            pump(self.clone(), label.to_string(), o);
        }
        if let Some(e) = child.stderr.take() {
            pump(self.clone(), label.to_string(), e);
        }
        let (ktx, krx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            tokio::select! {
                st = child.wait() => {
                    let text = match st {
                        Ok(s) => format!("exited ({s})"),
                        Err(e) => format!("stopped: {e}"),
                    };
                    on_exit(text);
                }
                _ = krx => { let _ = child.kill().await; }
            }
        });
        Ok(Running {
            kill: Some(ktx),
            handle,
        })
    }

    fn log_tail(&self, n: usize) -> String {
        let l = self.0.logs.lock().unwrap();
        let v: Vec<&String> = l.iter().rev().take(n).collect();
        v.into_iter().rev().cloned().collect::<Vec<_>>().join("\n")
    }

    fn check_accepting(&self) -> Result<(), String> {
        let r = self.0.mesh.my_record();
        if !r.lend {
            return Err("This device is not lending itself to the cluster".into());
        }
        if r.draining {
            return Err("This device is being emptied and takes no new work".into());
        }
        Ok(())
    }

    // ───────────── sharing: hold part of someone else's model ─────────────

    pub async fn start_share(&self, share: Share) -> Result<(), String> {
        self.check_accepting()?;
        let exe = engine::find_rpc_server(&self.0.cfg.engine_dir)
            .ok_or("This device's engine has no RPC worker. Update the engine on this device.")?;
        {
            let mut rt = self.0.rt.lock().await;
            if let Some(r) = &rt.rpc {
                if r.instance == share.instance {
                    return Ok(()); // already holding it
                }
                return Err("This device is already holding part of another model".into());
            }
            if rt.llama.is_some() || rt.starting.is_some() {
                return Err("This device is busy running a model".into());
            }
            rt.starting = Some((share.instance.clone(), Arc::new(AtomicBool::new(false))));
        }
        let result = self.start_share_inner(&exe, &share).await;
        let mut rt = self.0.rt.lock().await;
        rt.starting = None;
        match result {
            Ok(run) => {
                rt.rpc = Some(run);
                drop(rt);
                self.0.mesh.update_self(|r| {
                    r.share = Some(Share {
                        running: true,
                        ..share.clone()
                    });
                    true
                });
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    async fn start_share_inner(&self, exe: &Path, share: &Share) -> Result<RpcRun, String> {
        let port = free_port()?;
        let supports_cache = {
            let exe = exe.to_path_buf();
            tokio::task::spawn_blocking(move || {
                hw::run_capture(&exe, &["--help"], Duration::from_secs(10))
                    .is_some_and(|t| t.contains("--cache"))
            })
            .await
            .unwrap_or(false)
        };
        let mut cmd = Proc::new(exe);
        cmd.current_dir(exe.parent().unwrap_or(Path::new(".")))
            .args(["-H", "127.0.0.1", "-p", &port.to_string()])
            .args(["-t", &self.threads().to_string()])
            .env("LLAMA_CACHE", &self.0.cfg.cache_dir);
        if supports_cache {
            cmd.arg("-c");
        }
        let node = self.clone();
        let inst = share.instance.clone();
        let proc = self.spawn_proc("rpc-worker", cmd, move |why| {
            let node = node.clone();
            let inst = inst.clone();
            tokio::spawn(async move {
                node.log(format!("The RPC worker {why}"));
                node.share_exited(&inst).await;
            });
        })?;
        // Wait until it accepts connections.
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if proc.handle.is_finished() {
                return Err(format!(
                    "The RPC worker exited right away.\n{}",
                    self.log_tail(6)
                ));
            }
            if tokio::net::TcpStream::connect(addr).await.is_ok() {
                self.0.mesh.register_service("rpc", addr);
                return Ok(RpcRun {
                    instance: share.instance.clone(),
                    proc,
                });
            }
        }
        proc.stop().await;
        Err("The RPC worker did not start listening in time".into())
    }

    async fn share_exited(&self, instance: &str) {
        let mut rt = self.0.rt.lock().await;
        if rt.rpc.as_ref().is_some_and(|r| r.instance == instance) {
            rt.rpc = None;
            drop(rt);
            self.0.mesh.unregister_service("rpc");
            self.0.mesh.update_self(|r| {
                r.share = None;
                true
            });
        }
    }

    pub async fn stop_share(&self, instance: &str) {
        let run = {
            let mut rt = self.0.rt.lock().await;
            if rt
                .rpc
                .as_ref()
                .is_some_and(|r| instance.is_empty() || r.instance == instance)
            {
                rt.rpc.take()
            } else {
                None
            }
        };
        if let Some(r) = run {
            self.0.mesh.unregister_service("rpc");
            r.proc.stop().await;
            self.0.mesh.update_self(|r| {
                r.share = None;
                true
            });
        }
    }

    // ───────────── instances: serve a model ─────────────

    pub async fn start_instance(&self, spec: InstanceSpec) -> Result<Value, String> {
        self.check_accepting()?;
        let exe = self
            .server_exe()
            .ok_or("The engine is not installed on this device yet")?;
        let model_path = self.0.store.path(&crate::store::safe_name(&spec.model)?);
        if !model_path.exists() {
            return Err(format!("{} is not on this device yet", spec.model));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut rt = self.0.rt.lock().await;
            if rt.llama.is_some() || rt.rpc.is_some() || rt.starting.is_some() {
                return Err("This device is already busy with a model".into());
            }
            rt.starting = Some((spec.id.clone(), cancel.clone()));
        }
        let started = now_ms();
        self.0.mesh.update_self(|r| {
            r.instance = Some(instance_info(&spec, InstanceState::Loading, "", started));
            true
        });

        let result = self
            .start_instance_inner(&exe, &model_path, &spec, cancel.clone(), started)
            .await;
        match result {
            Ok(()) => Ok(json!({ "id": spec.id })),
            Err(e) => {
                // Leave a failed record so the owner (and the screen) can see why.
                let mut rt = self.0.rt.lock().await;
                rt.starting = None;
                drop(rt);
                for n in &spec.rpc_nodes {
                    self.0.mesh.stop_forwards_to(n);
                }
                let msg = e.clone();
                self.0.mesh.update_self(|r| {
                    r.instance = Some(instance_info(&spec, InstanceState::Failed, &msg, started));
                    true
                });
                Err(e)
            }
        }
    }

    async fn start_instance_inner(
        &self,
        exe: &Path,
        model: &Path,
        spec: &InstanceSpec,
        cancel: Arc<AtomicBool>,
        started: u64,
    ) -> Result<(), String> {
        // Doorways to the workers' RPC servers, through encrypted tunnels.
        let mut rpc: Vec<SocketAddr> = Vec::new();
        for n in &spec.rpc_nodes {
            rpc.push(
                self.0
                    .mesh
                    .forward(n, "rpc")
                    .await
                    .map_err(|e| format!("Cannot reach a worker device for this model: {e}"))?,
            );
        }
        let port = free_port()?;
        let mmproj = {
            let p = self.0.store.path(&format!(
                "{}.mmproj.gguf",
                spec.model.trim_end_matches(".gguf")
            ));
            p.exists().then_some(p)
        };
        let args = llama_args(spec, model, mmproj.as_deref(), port, &rpc);
        self.log(format!("[node] llama-server {}", args.join(" ")));
        let mut cmd = Proc::new(exe);
        cmd.current_dir(exe.parent().unwrap_or(Path::new(".")))
            .args(&args);

        let node = self.clone();
        let id = spec.id.clone();
        let proc = self.spawn_proc("llama-server", cmd, move |why| {
            let node = node.clone();
            let id = id.clone();
            tokio::spawn(async move { node.instance_exited(&id, &why).await });
        })?;

        let health = format!("http://127.0.0.1:{port}/health");
        let deadline = tokio::time::Instant::now() + LOAD_TIMEOUT;
        loop {
            if cancel.load(Ordering::Relaxed) {
                proc.stop().await;
                return Err("cancelled".into());
            }
            if proc.handle.is_finished() {
                return Err(format!(
                    "llama-server stopped while loading the model.\n{}",
                    self.log_tail(12)
                ));
            }
            if tokio::time::Instant::now() > deadline {
                proc.stop().await;
                return Err("Timed out waiting for the model to load".into());
            }
            if let Ok(r) = self
                .0
                .http
                .get(&health)
                .timeout(Duration::from_secs(2))
                .send()
                .await
            {
                if r.status().is_success() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        let mut rt = self.0.rt.lock().await;
        rt.starting = None;
        let monitor = tokio::spawn(monitor_instance(self.clone(), spec.clone(), port, started));
        rt.llama = Some(LlamaRun {
            id: spec.id.clone(),
            proc,
            port,
            tasks: vec![monitor],
            rpc_nodes: spec.rpc_nodes.clone(),
        });
        drop(rt);
        self.0.mesh.register_service("api", addr);
        self.0.mesh.update_self(|r| {
            r.instance = Some(instance_info(spec, InstanceState::Ready, "", started));
            true
        });
        Ok(())
    }

    async fn instance_exited(&self, id: &str, why: &str) {
        let mut rt = self.0.rt.lock().await;
        let ours = rt.llama.as_ref().is_some_and(|l| l.id == id);
        if !ours {
            return;
        }
        let run = rt.llama.take().unwrap();
        drop(rt);
        for t in &run.tasks {
            t.abort();
        }
        self.0.mesh.unregister_service("api");
        for n in &run.rpc_nodes {
            self.0.mesh.stop_forwards_to(n);
        }
        let error = format!("The model server {why}.\n{}", self.log_tail(8));
        self.0.mesh.update_self(|r| {
            if let Some(i) = r.instance.as_mut() {
                i.state = InstanceState::Failed;
                i.error = error.clone();
                i.busy = 0;
                i.queued = 0;
            }
            true
        });
    }

    /// Stops the model server here. An empty id stops whatever is running.
    pub async fn stop_instance(&self, id: &str) {
        let (run, cancel) = {
            let mut rt = self.0.rt.lock().await;
            let cancel = rt
                .starting
                .as_ref()
                .filter(|(sid, _)| id.is_empty() || sid == id)
                .map(|(_, c)| c.clone());
            let run = if rt
                .llama
                .as_ref()
                .is_some_and(|l| id.is_empty() || l.id == id)
            {
                rt.llama.take()
            } else {
                None
            };
            (run, cancel)
        };
        if let Some(c) = cancel {
            c.store(true, Ordering::Relaxed);
            // Give a start in progress a moment to notice and clean up.
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
        if let Some(run) = run {
            self.0.mesh.update_self(|r| {
                if let Some(i) = r.instance.as_mut() {
                    i.state = InstanceState::Stopping;
                }
                true
            });
            for t in &run.tasks {
                t.abort();
            }
            self.0.mesh.unregister_service("api");
            run.proc.stop().await;
            for n in &run.rpc_nodes {
                self.0.mesh.stop_forwards_to(n);
            }
        }
        // Whatever the record held (ready, failed, loading) is gone now.
        let still = self.0.rt.lock().await.llama.is_some();
        if !still {
            self.0.mesh.update_self(|r| {
                if r.instance.is_some()
                    && (id.is_empty() || r.instance.as_ref().is_some_and(|i| i.id == id))
                {
                    r.instance = None;
                    return true;
                }
                false
            });
        }
    }

    /// Stops everything this node is running (for a clean exit).
    pub async fn shutdown(&self) {
        self.stop_instance("").await;
        self.stop_share("").await;
    }

    pub async fn local_api(&self) -> Option<(SocketAddr, String)> {
        let rt = self.0.rt.lock().await;
        rt.llama.as_ref().map(|l| {
            (
                ([127, 0, 0, 1], l.port).into(),
                self.0
                    .mesh
                    .my_record()
                    .instance
                    .map(|i| i.model)
                    .unwrap_or_default(),
            )
        })
    }

    // ───────────── speed measurement ─────────────

    /// Measures how fast this device generates tokens with its smallest local model.
    pub async fn bench(&self) -> Result<Value, String> {
        {
            let rt = self.0.rt.lock().await;
            if rt.llama.is_some() || rt.rpc.is_some() || rt.starting.is_some() {
                return Err("Stop the model on this device before measuring its speed".into());
            }
        }
        let exe = engine::find_exe(&self.0.cfg.engine_dir, "llama-bench")
            .ok_or("This engine build has no llama-bench")?;
        let mut models = self.0.store.scan();
        models.sort_by_key(|m| m.size);
        let m = models
            .first()
            .ok_or("Download a model on this device first")?
            .clone();
        let path = self.0.store.path(&m.file);
        let threads = self.threads();
        let has_gpu = !self.0.mesh.my_record().hw.gpus.is_empty();
        let out = tokio::task::spawn_blocking(move || {
            let t = threads.to_string();
            let mut args = vec![
                "-m",
                path.to_str().unwrap_or(""),
                "-p",
                "0",
                "-n",
                "48",
                "-r",
                "2",
                "-t",
                &t,
                "-o",
                "json",
            ];
            if has_gpu {
                args.extend(["-ngl", "99"]);
            }
            hw::run_capture(&exe, &args, Duration::from_secs(300))
        })
        .await
        .map_err(|e| e.to_string())?
        .ok_or("The speed test did not finish")?;
        let tok_s = parse_bench(&out).ok_or("Could not read the speed test result")?;
        let model_gb = m.size as f64 / 1e9;
        self.0.mesh.update_self(|r| {
            r.bench = Some(Bench {
                tok_s,
                model: m.file.clone(),
                model_gb,
                at_ms: now_ms(),
            });
            true
        });
        Ok(json!({ "tokS": tok_s, "model": m.file }))
    }
}

/// Reads `llama-bench -o json` output: the generation speed in tokens per second.
pub fn parse_bench(text: &str) -> Option<f64> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    let v: Value = serde_json::from_str(&text[start..=end]).ok()?;
    v.as_array()?
        .iter()
        .filter(|e| e["n_gen"].as_u64().unwrap_or(0) > 0)
        .filter_map(|e| e["avg_ts"].as_f64())
        .next_back()
        .map(|x| (x * 10.0).round() / 10.0)
}

// ───────────────────────────── background loops ─────────────────────────────

/// Polls the model server for load and speed and publishes them.
async fn monitor_instance(node: Node, spec: InstanceSpec, port: u16, started: u64) {
    let url = format!("http://127.0.0.1:{port}/metrics");
    let mut tick = tokio::time::interval(Duration::from_millis(1000));
    loop {
        tick.tick().await;
        let Ok(r) = node
            .0
            .http
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        else {
            continue;
        };
        let Ok(text) = r.text().await else { continue };
        let m = parse_metrics(&text);
        node.0.mesh.update_self(|rec| {
            let Some(i) = rec.instance.as_mut() else {
                return false;
            };
            if i.id != spec.id || i.state != InstanceState::Ready {
                return false;
            }
            let changed = i.busy != m.processing
                || i.queued != m.deferred
                || (i.tok_s - m.tok_s).abs() > 0.05
                || i.requests_total != m.tokens_total;
            i.busy = m.processing;
            i.queued = m.deferred;
            i.tok_s = m.tok_s;
            i.requests_total = m.tokens_total;
            i.started_ms = started;
            changed
        });
    }
}

fn dir_size(p: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                total += dir_size(&path);
            } else {
                total += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// Keeps this node's record true: memory, GPUs, models, engine, links.
async fn sampler(node: Node) {
    let mut n = 0u64;
    let mut hw_cache: Option<crate::proto::HwSpec> = None;
    let mut cache_mb = 0u64;
    let mut tick = tokio::time::interval(Duration::from_secs(4));
    loop {
        tick.tick().await;
        let slow = n % 8 == 0; // every ~30 s (and the first time)
        if slow {
            let exe = node.server_exe();
            let first = hw_cache.is_none();
            let prev_bw = hw_cache.as_ref().map(|h| h.mem_bw_gbps).unwrap_or(0.0);
            let spec = tokio::task::spawn_blocking(move || hw::sample(exe.as_deref(), first))
                .await
                .ok();
            if let Some(mut s) = spec {
                if !first {
                    s.mem_bw_gbps = prev_bw;
                }
                hw_cache = Some(s);
            }
            let cache = node.0.cfg.cache_dir.clone();
            cache_mb = tokio::task::spawn_blocking(move || dir_size(&cache) / (1024 * 1024))
                .await
                .unwrap_or(0);
        }
        n += 1;
        let (_, free) = hw::memory_mb();
        let models = node.0.store.scan();
        let transfers = node.0.store.transfers();
        let engine_info = {
            let dir = &node.0.cfg.engine_dir;
            EngineInfo {
                installed: node.server_exe().is_some(),
                version: engine::installed_version(dir).unwrap_or_default(),
                variant: engine::installed_variant(dir).unwrap_or_else(|| "cpu".into()),
                has_rpc: engine::find_rpc_server(dir).is_some(),
            }
        };
        let links: Vec<LinkInfo> = node.0.mesh.with_registry(|reg, now| {
            reg.views(now)
                .into_iter()
                .filter(|v| !v.is_me && v.alive)
                .filter_map(|v| {
                    v.rtt_ms.map(|rtt| LinkInfo {
                        node_id: v.record.node_id,
                        rtt_ms: (rtt * 10.0).round() / 10.0,
                    })
                })
                .collect()
        });
        node.0.mesh.update_self(|r| {
            let mut changed = false;
            if let Some(h) = &hw_cache {
                let mut h = h.clone();
                h.ram_free_mb = free;
                // Keep per-GPU "free" fresh only on the slow refresh; the rest is identical.
                if r.hw != h {
                    r.hw = h;
                    changed = true;
                }
            } else if r.hw.ram_free_mb != free {
                r.hw.ram_free_mb = free;
                changed = true;
            }
            if r.models != models {
                r.models = models.clone();
                changed = true;
            }
            if r.transfers != transfers {
                r.transfers = transfers.clone();
                changed = true;
            }
            if r.engine != engine_info {
                r.engine = engine_info.clone();
                changed = true;
            }
            if r.tensor_cache_mb != cache_mb {
                r.tensor_cache_mb = cache_mb;
                changed = true;
            }
            if r.links != links {
                r.links = links.clone();
                changed = true;
            }
            changed
        });
        // One small chore per tick: read a model's header or hash it.
        node.0.store.background_tick().await;
    }
}

#[cfg(test)]
mod tests;

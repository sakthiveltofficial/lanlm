//! A whole cluster in one process, running stand-ins for llama.cpp's programs.
//!
//! The stand-ins (small Python scripts) behave like the real ones where it matters here: the
//! server loads for a moment, answers `/health`, `/metrics` and the OpenAI API (streaming too),
//! connects to its RPC workers at start-up, and exits if a worker goes away.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lanlm_mesh::gateway::{Gateway, GatewayConfig};
use lanlm_mesh::node::{Node, NodeConfig};
use lanlm_mesh::proto::{Deployment, InstanceState};
use lanlm_mesh::{Mesh, MeshConfig};

const FAKE_SERVER: &str = r#"#!/usr/bin/env python3
import sys, json, time, threading, socket, os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

args = sys.argv[1:]
def opt(name, default=None):
    return args[args.index(name) + 1] if name in args else default
if "--list-devices" in args:
    print("Available devices:")
    sys.exit(0)
port = int(opt("--port")); model = opt("-m"); rpc = opt("--rpc")
delay = float(os.environ.get("FAKE_DELAY", "0"))
print("fake llama-server", " ".join(args), flush=True)
served_by = os.path.basename(os.path.dirname(model))
socks = []
if rpc:
    for ep in rpc.split(","):
        h, p = ep.rsplit(":", 1)
        socks.append(socket.create_connection((h, int(p)), timeout=5))
        print("connected to rpc", ep, flush=True)
loaded = threading.Event()
threading.Thread(target=lambda: (time.sleep(0.4), loaded.set()), daemon=True).start()
def watch():
    while True:
        time.sleep(0.3)
        for s in socks:
            try:
                s.setblocking(False)
                if s.recv(1, socket.MSG_PEEK) == b"":
                    print("a worker went away", flush=True); os._exit(1)
            except BlockingIOError:
                pass
            except Exception:
                os._exit(1)
threading.Thread(target=watch, daemon=True).start()
state = {"busy": 0, "tokens": 0}
lock = threading.Lock()
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send_json(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        if self.path == "/health":
            self.send_json(200 if loaded.is_set() else 503, {"status": "ok" if loaded.is_set() else "loading"})
        elif self.path == "/metrics":
            with lock: b, t = state["busy"], state["tokens"]
            text = ("llamacpp:requests_processing %d\nllamacpp:requests_deferred 0\n"
                    "llamacpp:predicted_tokens_seconds 25.0\nllamacpp:tokens_predicted_total %d\n" % (b, t)).encode()
            self.send_response(200); self.send_header("content-length", str(len(text))); self.end_headers(); self.wfile.write(text)
        else:
            self.send_json(404, {})
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(n) or b"{}")
        with lock: state["busy"] += 1
        try:
            time.sleep(delay)
            if body.get("stream"):
                self.send_response(200); self.send_header("content-type", "text/event-stream"); self.end_headers()
                for i in range(5):
                    self.wfile.write(("data: " + json.dumps({"served_by": served_by, "choices": [{"delta": {"content": "t%d " % i}}]}) + "\n\n").encode())
                    self.wfile.flush(); time.sleep(0.03)
                self.wfile.write(b"data: [DONE]\n\n")
            else:
                self.send_json(200, {"served_by": served_by, "choices": [{"message": {"role": "assistant", "content": "ok"}}]})
        finally:
            with lock: state["busy"] -= 1; state["tokens"] += 5
ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
"#;

const FAKE_RPC: &str = r#"#!/usr/bin/env python3
import sys, socket, threading
args = sys.argv[1:]
if "--help" in args or "-h" in args:
    print("options:\n  -c, --cache   enable the local file cache")
    sys.exit(0)
host = args[args.index("-H") + 1]; port = int(args[args.index("-p") + 1])
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind((host, port)); s.listen(8)
print("fake rpc-server on", host, port, flush=True)
def serve(c):
    try:
        while c.recv(65536): pass
    except Exception: pass
while True:
    c, _ = s.accept()
    threading.Thread(target=serve, args=(c,), daemon=True).start()
"#;

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "lanlm-e2e-{tag}-{}-{}",
        std::process::id(),
        rand::random::<u32>()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn script(dir: &Path, name: &str, text: &str) {
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A GGUF header that says "8 layers", padded to `mb` megabytes.
fn model_bytes(mb: usize) -> Vec<u8> {
    fn s(b: &mut Vec<u8>, t: &str) {
        b.extend_from_slice(&(t.len() as u64).to_le_bytes());
        b.extend_from_slice(t.as_bytes());
    }
    let mut b = Vec::new();
    b.extend_from_slice(b"GGUF");
    b.extend_from_slice(&3u32.to_le_bytes());
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&3u64.to_le_bytes());
    s(&mut b, "general.architecture");
    b.extend_from_slice(&8u32.to_le_bytes());
    s(&mut b, "llama");
    s(&mut b, "llama.block_count");
    b.extend_from_slice(&4u32.to_le_bytes());
    b.extend_from_slice(&8u32.to_le_bytes());
    s(&mut b, "llama.context_length");
    b.extend_from_slice(&4u32.to_le_bytes());
    b.extend_from_slice(&4096u32.to_le_bytes());
    b.resize(mb * 1024 * 1024, 7);
    b
}

struct Dev {
    node: Node,
    mesh: Mesh,
}

async fn dev(name: &str, delay: &str) -> Dev {
    let root = temp(name);
    let engine = root.join("engine");
    std::fs::create_dir_all(&engine).unwrap();
    script(
        &engine,
        "llama-server",
        &FAKE_SERVER.replace("FAKE_DELAY\", \"0\"", &format!("FAKE_DELAY\", \"{delay}\"")),
    );
    script(&engine, "rpc-server", FAKE_RPC);
    let mut cfg = MeshConfig::new(root.join("mesh"));
    cfg.name = Some(name.to_string());
    cfg.port = 0;
    cfg.discovery = false;
    let mesh = Mesh::start(cfg).await.unwrap();
    let node = Node::start(
        mesh.clone(),
        NodeConfig {
            engine_dir: engine,
            models_dir: root.join(format!("{name}-models")),
            cache_dir: root.join("cache"),
        },
        reqwest::Client::new(),
    );
    Dev { node, mesh }
}

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn alive(m: &Mesh) -> usize {
    m.snapshot().nodes.iter().filter(|n| n.alive).count()
}

async fn cluster(names: &[&str], delay: &str) -> (Vec<Dev>, String) {
    let mut devs = Vec::new();
    for n in names {
        devs.push(dev(n, delay).await);
    }
    let key = devs[0].mesh.create_cluster("e2e");
    for d in &devs[1..] {
        d.mesh.join_cluster(&key, "e2e").unwrap();
    }
    for a in &devs {
        for b in &devs {
            if a.mesh.id() != b.mesh.id() {
                a.mesh.add_peer(&format!("127.0.0.1:{}", b.mesh.port()));
            }
        }
    }
    for d in &devs {
        wait_for("cluster", 20, || alive(&d.mesh) == names.len()).await;
    }
    (devs, key)
}

/// Puts a model on one device and waits until the cluster knows its size and layer count.
async fn give_model(on: &Dev, others: &[&Dev], file: &str, mb: usize) {
    std::fs::write(on.node.store().path(file), model_bytes(mb)).unwrap();
    for d in std::iter::once(on).chain(others.iter().copied()) {
        wait_for("model header read", 30, || {
            d.mesh.with_registry(|r, now| {
                r.holders(file, now).iter().any(|e| {
                    e.record
                        .models
                        .iter()
                        .any(|m| m.file == file && m.n_layer == 8 && !m.sha256.is_empty())
                })
            })
        })
        .await;
    }
}

fn ready_instances(m: &Mesh, dep: &str) -> Vec<(String, String)> {
    m.with_registry(|r, now| {
        r.instances(now)
            .into_iter()
            .filter(|(_, i)| i.deployment == dep && i.state == InstanceState::Ready)
            .map(|(rec, i)| (rec.name.clone(), i.kind.clone()))
            .collect()
    })
}

fn deployment(model: &str, mode: &str, parallel: u32) -> Deployment {
    Deployment {
        id: "dep1".into(),
        model: model.into(),
        mode: mode.into(),
        ctx: 1024,
        parallel,
        ..Default::default()
    }
}

async fn chat(addr: std::net::SocketAddr, stream: bool) -> serde_json::Value {
    let body = serde_json::json!({
        "model": "auto",
        "stream": stream,
        "messages": [{ "role": "user", "content": format!("hello {}", rand::random::<u32>()) }]
    });
    let r = reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{:?}", r.text().await);
    if stream {
        let text = r.text().await.unwrap();
        assert!(text.contains("[DONE]"), "{text}");
        let first = text.lines().find(|l| l.starts_with("data: {")).unwrap();
        serde_json::from_str(&first[6..]).unwrap()
    } else {
        r.json().await.unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_runs_on_every_device_and_the_gateway_shares_the_load() {
    let (devs, _) = cluster(&["alpha", "bravo", "charlie"], "0.15").await;
    give_model(&devs[0], &[&devs[1], &devs[2]], "tiny.gguf", 3).await;

    // Everyone's engine and hardware must be known before a plan can use the device.
    for d in &devs {
        wait_for("engine seen", 30, || {
            d.mesh.with_registry(|r, now| {
                r.live(now)
                    .iter()
                    .all(|e| e.record.engine.installed && e.record.hw.ram_free_mb > 0)
            })
        })
        .await;
    }

    devs[0]
        .node
        .deploy(deployment("tiny.gguf", "auto", 2))
        .unwrap();
    wait_for("three replicas ready", 90, || {
        ready_instances(&devs[0].mesh, "dep1").len() == 3
    })
    .await;
    let kinds: Vec<String> = ready_instances(&devs[0].mesh, "dep1")
        .into_iter()
        .map(|x| x.1)
        .collect();
    assert!(kinds.iter().all(|k| k == "replica"), "{kinds:?}");
    // The model was copied to the devices that did not have it, from a neighbour.
    for d in &devs {
        assert!(d.node.store().path("tiny.gguf").exists());
    }

    // The gateway on one device reaches servers on all of them.
    let gw = Gateway::start(
        devs[0].node.clone(),
        GatewayConfig {
            port: 0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut tasks = Vec::new();
    for i in 0..18 {
        let addr = gw.addr;
        tasks.push(tokio::spawn(async move { chat(addr, i % 2 == 0).await }));
    }
    let mut served = std::collections::BTreeSet::new();
    for t in tasks {
        let v = t.await.unwrap();
        served.insert(v["served_by"].as_str().unwrap().to_string());
    }
    assert!(
        served.len() >= 2,
        "all requests went to one device: {served:?}"
    );
    let s = gw.status();
    assert_eq!(s.total, 18);
    assert_eq!(s.failed, 0);
    assert_eq!(s.backends.len(), 3);

    // Models listing and health.
    let models: serde_json::Value = reqwest::get(format!("http://{}/v1/models", gw.addr))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(models["data"][0]["id"], "tiny");
    gw.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pipeline_sends_layers_through_encrypted_tunnels() {
    let (devs, _) = cluster(&["alpha", "bravo"], "0").await;
    give_model(&devs[0], &[&devs[1]], "tiny.gguf", 3).await;
    for d in &devs {
        wait_for("engine seen", 30, || {
            d.mesh.with_registry(|r, now| {
                r.live(now)
                    .iter()
                    .all(|e| e.record.engine.installed && e.record.hw.ram_free_mb > 0)
            })
        })
        .await;
    }
    devs[0]
        .node
        .deploy(deployment("tiny.gguf", "pipeline", 1))
        .unwrap();
    wait_for("pipeline ready", 90, || {
        ready_instances(&devs[0].mesh, "dep1").len() == 1
    })
    .await;
    let (master, kind) = ready_instances(&devs[0].mesh, "dep1").remove(0);
    assert_eq!(kind, "pipeline");

    // The master's server was started with a local doorway to the worker, and it connected.
    let log = devs
        .iter()
        .flat_map(|d| d.node.logs())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(log.contains("--rpc 127.0.0.1:"), "{log}");
    assert!(log.contains("connected to rpc"), "{log}");
    assert!(log.contains("fake rpc-server"), "{log}");
    // Layers: the master holds some, the worker the rest, and they add up.
    let (stages, share) = devs[0].mesh.with_registry(|r, now| {
        let inst = r.instances(now).into_iter().next().unwrap().1.clone();
        let sh = r
            .live(now)
            .iter()
            .filter_map(|e| e.record.share.clone())
            .next();
        (inst.stages, sh)
    });
    assert_eq!(stages.len(), 2);
    assert_eq!(stages.iter().map(|s| s.layers).sum::<u32>(), 8);
    assert_eq!(stages[0].name, master);
    assert!(share.unwrap().running);

    // And a request goes through.
    let gw = Gateway::start(
        devs[1].node.clone(),
        GatewayConfig {
            port: 0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let v = chat(gw.addr, true).await;
    assert!(v["served_by"].is_string());
    gw.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cluster_heals_when_a_device_disappears_and_grows_when_it_returns() {
    let (devs, key) = cluster(&["alpha", "bravo", "charlie"], "0").await;
    give_model(&devs[0], &[&devs[1], &devs[2]], "tiny.gguf", 3).await;
    for d in &devs {
        wait_for("engine seen", 30, || {
            d.mesh.with_registry(|r, now| {
                r.live(now)
                    .iter()
                    .all(|e| e.record.engine.installed && e.record.hw.ram_free_mb > 0)
            })
        })
        .await;
    }
    devs[0]
        .node
        .deploy(deployment("tiny.gguf", "auto", 2))
        .unwrap();
    wait_for("three replicas", 90, || {
        ready_instances(&devs[0].mesh, "dep1").len() == 3
    })
    .await;

    // 1. A server that dies is restarted on the same (still free) device.
    devs[2].node.stop_instance("").await;
    wait_for("charlie's server restarted", 60, || {
        ready_instances(&devs[0].mesh, "dep1").len() == 3
    })
    .await;

    // 2. A device that leaves takes its server with it; the rest keep serving.
    let gw = Gateway::start(
        devs[0].node.clone(),
        GatewayConfig {
            port: 0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    devs[1].node.shutdown().await;
    devs[1].mesh.leave_cluster();
    wait_for("two left", 30, || {
        ready_instances(&devs[0].mesh, "dep1").len() == 2
    })
    .await;
    for _ in 0..6 {
        let v = chat(gw.addr, false).await;
        assert_ne!(v["served_by"], "bravo-models");
    }

    // 3. It comes back and is used again, without anyone touching the deployment.
    devs[1].mesh.join_cluster(&key, "e2e").unwrap();
    devs[1]
        .mesh
        .add_peer(&format!("127.0.0.1:{}", devs[0].mesh.port()));
    wait_for("three again", 90, || {
        ready_instances(&devs[0].mesh, "dep1").len() == 3
    })
    .await;
    gw.stop().await;

    // 4. Removing the deployment stops everything everywhere.
    devs[0].node.undeploy("dep1").await.unwrap();
    wait_for("all stopped", 30, || {
        ready_instances(&devs[0].mesh, "dep1").is_empty()
    })
    .await;
    for d in &devs {
        wait_for("no instance left", 20, || {
            d.mesh.my_record().instance.is_none()
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_wait_in_line_when_every_slot_is_taken() {
    // One server with one slot and slow answers; six requests arrive together.
    let (devs, _) = cluster(&["solo"], "0.4").await;
    give_model(&devs[0], &[], "tiny.gguf", 3).await;
    wait_for("engine seen", 30, || {
        devs[0].mesh.my_record().engine.installed && devs[0].mesh.my_record().hw.ram_free_mb > 0
    })
    .await;
    devs[0]
        .node
        .deploy(deployment("tiny.gguf", "replicas", 1))
        .unwrap();
    wait_for("ready", 60, || {
        ready_instances(&devs[0].mesh, "dep1").len() == 1
    })
    .await;
    let gw = Gateway::start(
        devs[0].node.clone(),
        GatewayConfig {
            port: 0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let start = Instant::now();
    let mut tasks = Vec::new();
    for _ in 0..6 {
        let addr = gw.addr;
        tasks.push(tokio::spawn(async move { chat(addr, false).await }));
    }
    let mut peak_queue = 0;
    while !tasks.iter().all(|t| t.is_finished()) {
        peak_queue = peak_queue.max(gw.status().queued);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for t in tasks {
        t.await.unwrap();
    }
    // One slot, 0.4 s each: six requests cannot finish faster than they take one after another.
    assert!(
        start.elapsed() >= Duration::from_millis(2000),
        "{:?}",
        start.elapsed()
    );
    assert!(peak_queue >= 3, "queue never built up: {peak_queue}");
    assert_eq!(gw.status().failed, 0);
    gw.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_gateway_asks_for_a_key_when_it_is_shared() {
    let (devs, _) = cluster(&["solo"], "0").await;
    let open = Gateway::start(
        devs[0].node.clone(),
        GatewayConfig {
            port: 0,
            network: true,
            api_keys: vec![],
            ..Default::default()
        },
    )
    .await;
    assert!(
        open.is_err(),
        "a network-wide gateway without a key must be refused"
    );
    let gw = Gateway::start(
        devs[0].node.clone(),
        GatewayConfig {
            port: 0,
            api_keys: vec!["k1".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let c = reqwest::Client::new();
    let url = format!("http://{}/v1/models", gw.addr);
    assert_eq!(c.get(&url).send().await.unwrap().status(), 401);
    assert_eq!(
        c.get(&url)
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        c.get(&url).bearer_auth("k1").send().await.unwrap().status(),
        200
    );
    // With nothing deployed there is nothing to serve.
    let r = c
        .post(format!("http://{}/v1/chat/completions", gw.addr))
        .bearer_auth("k1")
        .json(&serde_json::json!({ "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    gw.stop().await;
}

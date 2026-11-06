//! One address for the whole cluster.
//!
//! Applications talk to the gateway with the OpenAI API. It sends each request to the model
//! server with the most free capacity, holds requests in a queue when every server is full
//! (each server batches the requests it has continuously, so a freed slot is used at once),
//! keeps one conversation on one server so its prompt cache is reused, and retries on another
//! server when one fails before it has said anything.
//!
//! Servers on other devices are reached through the encrypted mesh tunnel, so they never need
//! to be open to the network.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri};
use axum::routing::any;
use axum::Router;
use futures_util::Stream;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{oneshot, Notify};

use crate::node::Node;
use crate::proto::InstanceState;

const MAX_BODY: usize = 64 * 1024 * 1024;
const ATTEMPTS: usize = 3;
const PENALTY: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct GatewayConfig {
    pub port: u16,
    /// Listen on the whole network instead of this machine only.
    pub network: bool,
    /// Keys that may use the gateway. Empty is only allowed when listening on this machine only.
    pub api_keys: Vec<String>,
    /// How long a request may wait for a free server before it is refused.
    pub queue_wait: Duration,
    /// How many requests may wait at once.
    pub max_queue: usize,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        GatewayConfig {
            port: 8080,
            network: false,
            api_keys: vec![],
            queue_wait: Duration::from_secs(120),
            max_queue: 256,
        }
    }
}

/// A model server the gateway may use.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Backend {
    pub instance: String,
    pub node_id: String,
    pub node_name: String,
    pub model: String,
    pub slots: u32,
    /// Requests that server reports it is working on or has waiting.
    pub busy: u32,
    pub tok_s: f64,
}

#[derive(Default)]
struct Counters {
    total: AtomicU64,
    failed: AtomicU64,
    retried: AtomicU64,
    refused: AtomicU64,
    waited_ms: AtomicU64,
    per_backend: Mutex<HashMap<String, u64>>,
}

struct Shared {
    node: Node,
    cfg: GatewayConfig,
    http: reqwest::Client,
    inflight: Mutex<HashMap<String, u32>>,
    bad: Mutex<HashMap<String, Instant>>,
    queued: std::sync::atomic::AtomicUsize,
    freed: Notify,
    counters: Counters,
}

pub struct Gateway {
    pub addr: SocketAddr,
    shared: Arc<Shared>,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    beat: tokio::task::JoinHandle<()>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub running: bool,
    pub url: String,
    pub backends: Vec<Backend>,
    pub inflight: u32,
    pub queued: usize,
    pub total: u64,
    pub failed: u64,
    pub retried: u64,
    pub refused: u64,
}

// ───────────────────────────── choosing a server ─────────────────────────────

fn hash64(s: &str) -> u64 {
    let d = Sha256::digest(s.as_bytes());
    u64::from_be_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]])
}

/// Which server a conversation prefers: the same one every time, so its cache is reused, and a
/// stable choice when servers come and go (rendezvous hashing).
pub fn preferred(key: &str, backends: &[Backend]) -> Option<usize> {
    if key.is_empty() {
        return None;
    }
    backends
        .iter()
        .enumerate()
        .max_by_key(|(_, b)| hash64(&format!("{key}|{}", b.instance)))
        .map(|(i, _)| i)
}

/// How full a server is: 0 is idle, 1 is every slot taken, above 1 means requests are waiting.
pub fn load(b: &Backend, local_inflight: u32) -> f64 {
    let used = b.busy.max(local_inflight) as f64;
    used / b.slots.max(1) as f64
}

/// Picks the server for a request, or `None` when every server is full.
pub fn choose(
    backends: &[Backend],
    inflight: &HashMap<String, u32>,
    affinity: &str,
    skip: &[String],
) -> Option<usize> {
    let loads: Vec<(usize, f64)> = backends
        .iter()
        .enumerate()
        .filter(|(_, b)| !skip.contains(&b.instance))
        .map(|(i, b)| (i, load(b, inflight.get(&b.instance).copied().unwrap_or(0))))
        .filter(|(_, l)| *l < 1.0)
        .collect();
    let best = loads.iter().min_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Equal load: the faster server.
            .then_with(|| {
                backends[b.0]
                    .tok_s
                    .partial_cmp(&backends[a.0].tok_s)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    })?;
    // Stay on the conversation's own server when it has room and is not much busier.
    if let Some(p) = preferred(affinity, backends) {
        if let Some((_, l)) = loads.iter().find(|(i, _)| *i == p) {
            if *l <= best.1 + 0.5 {
                return Some(p);
            }
        }
    }
    Some(best.0)
}

/// A name a client may use for a model matches the model file with or without `.gguf`.
pub fn model_matches(wanted: &str, file: &str) -> bool {
    let w = wanted.trim().to_ascii_lowercase();
    let f = file.to_ascii_lowercase();
    w == f || format!("{w}.gguf") == f || f.strip_suffix(".gguf") == Some(w.as_str())
}

/// The key that keeps one conversation on one server.
pub fn affinity_key(headers: &HeaderMap, body: &Value) -> String {
    for h in ["x-session-id", "x-conversation-id"] {
        if let Some(v) = headers.get(h).and_then(|v| v.to_str().ok()) {
            return v.to_string();
        }
    }
    if let Some(u) = body["user"].as_str() {
        return u.to_string();
    }
    let mut text = String::new();
    if let Some(msgs) = body["messages"].as_array() {
        // The start of a conversation (system prompt and first question) is what stays the same.
        for m in msgs.iter().take(2) {
            if let Some(c) = m["content"].as_str() {
                text.push_str(c);
            }
        }
    } else if let Some(p) = body["prompt"].as_str() {
        text.push_str(p);
    }
    text.chars().take(2000).collect()
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn authorized(cfg: &GatewayConfig, headers: &HeaderMap) -> bool {
    if cfg.api_keys.is_empty() {
        return true;
    }
    let given = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    cfg.api_keys.iter().any(|k| constant_time_eq(k, given))
}

// ───────────────────────────── serving ─────────────────────────────

fn error(status: StatusCode, message: &str) -> Response<Body> {
    let body = json!({ "error": { "message": message, "type": "lanlm_gateway" } });
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Hands a server back when the response body is finished or dropped.
struct Guard {
    shared: Arc<Shared>,
    instance: String,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut m = self.shared.inflight.lock().unwrap();
        if let Some(n) = m.get_mut(&self.instance) {
            *n = n.saturating_sub(1);
        }
        drop(m);
        self.shared.freed.notify_waiters();
    }
}

struct Guarded<S> {
    inner: S,
    _guard: Guard,
}

impl<S: Stream + Unpin> Stream for Guarded<S> {
    type Item = S::Item;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

fn is_hop(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
    )
}

impl Shared {
    fn backends(&self) -> Vec<Backend> {
        let bad = self.bad.lock().unwrap();
        let now = Instant::now();
        self.node.mesh().with_registry(|reg, t| {
            let mut v: Vec<Backend> = reg
                .instances(t)
                .into_iter()
                .filter(|(_, i)| i.state == InstanceState::Ready)
                .filter(|(r, _)| !bad.get(&r.node_id).is_some_and(|until| *until > now))
                .map(|(r, i)| Backend {
                    instance: i.id.clone(),
                    node_id: r.node_id.clone(),
                    node_name: r.name.clone(),
                    model: i.model.clone(),
                    slots: i.slots,
                    busy: i.busy + i.queued,
                    tok_s: i.tok_s,
                })
                .collect();
            v.sort_by(|a, b| a.instance.cmp(&b.instance));
            v
        })
    }

    /// Where to send a request for this server: straight to it here, or through a tunnel.
    async fn target(&self, b: &Backend) -> Result<SocketAddr, String> {
        if b.node_id == self.node.mesh().id() {
            return self
                .node
                .local_api()
                .await
                .map(|(a, _)| a)
                .ok_or_else(|| "the local model server is not running".to_string());
        }
        self.node.mesh().forward(&b.node_id, "api").await
    }
}

async fn handle(State(sh): State<Arc<Shared>>, req: Request<Body>) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let path = parts.uri.path().to_string();

    if path == "/health" {
        let n = sh.backends().len();
        return Response::builder()
            .status(if n > 0 {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            })
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "status": if n > 0 { "ok" } else { "no model running" }, "servers": n })
                    .to_string(),
            ))
            .unwrap();
    }
    if !authorized(&sh.cfg, &parts.headers) {
        return error(
            StatusCode::UNAUTHORIZED,
            "A valid API key is required (Authorization: Bearer ...)",
        );
    }
    if path == "/cluster" {
        return Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&status_of(&sh)).unwrap_or_default(),
            ))
            .unwrap();
    }
    if path == "/v1/models" && parts.method == Method::GET {
        let mut seen = Vec::new();
        for b in sh.backends() {
            if !seen.contains(&b.model) {
                seen.push(b.model);
            }
        }
        let data: Vec<Value> = seen
            .iter()
            .map(|m| json!({ "id": m.trim_end_matches(".gguf"), "object": "model", "owned_by": "lanlm" }))
            .collect();
        return Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "object": "list", "data": data }).to_string(),
            ))
            .unwrap();
    }
    let proxied = path.starts_with("/v1/")
        || matches!(
            path.as_str(),
            "/completion" | "/infill" | "/tokenize" | "/detokenize" | "/props"
        );
    if !proxied {
        return error(StatusCode::NOT_FOUND, "Not found");
    }

    let bytes = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "The request is too large"),
    };
    let json_body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let wanted = json_body["model"].as_str().unwrap_or("").to_string();
    let affinity = affinity_key(&parts.headers, &json_body);
    sh.counters.total.fetch_add(1, Ordering::Relaxed);

    let mut skip: Vec<String> = Vec::new();
    for attempt in 0..ATTEMPTS {
        let (backend, guard) = match acquire(&sh, &wanted, &affinity, &skip).await {
            Ok(x) => x,
            Err(resp) => return resp,
        };
        let addr = match sh.target(&backend).await {
            Ok(a) => a,
            Err(e) => {
                drop(guard);
                sh.bad
                    .lock()
                    .unwrap()
                    .insert(backend.node_id.clone(), Instant::now() + PENALTY);
                skip.push(backend.instance.clone());
                sh.counters.retried.fetch_add(1, Ordering::Relaxed);
                if attempt + 1 == ATTEMPTS {
                    sh.counters.failed.fetch_add(1, Ordering::Relaxed);
                    return error(
                        StatusCode::BAD_GATEWAY,
                        &format!("No model server could be reached: {e}"),
                    );
                }
                continue;
            }
        };
        match send(
            &sh,
            addr,
            &parts.method,
            &parts.uri,
            &parts.headers,
            bytes.clone(),
        )
        .await
        {
            Ok(resp) => {
                // A server that is still loading or overloaded answers 503: try another.
                if resp.status() == StatusCode::SERVICE_UNAVAILABLE && attempt + 1 < ATTEMPTS {
                    drop(guard);
                    skip.push(backend.instance.clone());
                    sh.counters.retried.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                *sh.counters
                    .per_backend
                    .lock()
                    .unwrap()
                    .entry(backend.instance.clone())
                    .or_insert(0) += 1;
                return stream_back(resp, guard);
            }
            Err(e) => {
                drop(guard);
                sh.bad
                    .lock()
                    .unwrap()
                    .insert(backend.node_id.clone(), Instant::now() + PENALTY);
                skip.push(backend.instance.clone());
                sh.counters.retried.fetch_add(1, Ordering::Relaxed);
                if attempt + 1 == ATTEMPTS {
                    sh.counters.failed.fetch_add(1, Ordering::Relaxed);
                    return error(
                        StatusCode::BAD_GATEWAY,
                        &format!("The model server failed: {e}"),
                    );
                }
            }
        }
    }
    sh.counters.failed.fetch_add(1, Ordering::Relaxed);
    error(StatusCode::BAD_GATEWAY, "No model server answered")
}

/// Waits for a server with a free slot (up to the queue limit) and takes a slot on it.
#[allow(clippy::result_large_err)] // the error is the HTTP response to send back as is
async fn acquire(
    sh: &Arc<Shared>,
    wanted: &str,
    affinity: &str,
    skip: &[String],
) -> Result<(Backend, Guard), Response<Body>> {
    let start = Instant::now();
    let deadline = start + sh.cfg.queue_wait;
    let mut counted = false;
    loop {
        let all = sh.backends();
        let mut candidates: Vec<Backend> =
            if wanted.is_empty() || matches!(wanted, "auto" | "default" | "local" | "any") {
                all.clone()
            } else {
                all.iter()
                    .filter(|b| model_matches(wanted, &b.model))
                    .cloned()
                    .collect()
            };
        if candidates.is_empty() && !wanted.is_empty() {
            // An unknown name: if only one model is running, that is the one they mean.
            let models: std::collections::BTreeSet<&str> =
                all.iter().map(|b| b.model.as_str()).collect();
            if models.len() == 1 {
                candidates = all.clone();
            } else if !all.is_empty() {
                let list: Vec<&str> = models.into_iter().collect();
                return Err(error(
                    StatusCode::NOT_FOUND,
                    &format!(
                        "No running model matches '{wanted}'. Running: {}",
                        list.join(", ")
                    ),
                ));
            }
        }
        if candidates.is_empty() {
            if start.elapsed() > Duration::from_secs(5) && all.is_empty() {
                sh.counters.refused.fetch_add(1, Ordering::Relaxed);
                return Err(error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "No model is running in the cluster. Deploy one first.",
                ));
            }
        } else {
            let mut inflight = sh.inflight.lock().unwrap();
            if let Some(i) = choose(&candidates, &inflight, affinity, skip) {
                let b = candidates[i].clone();
                *inflight.entry(b.instance.clone()).or_insert(0) += 1;
                drop(inflight);
                if counted {
                    sh.queued.fetch_sub(1, Ordering::Relaxed);
                    sh.counters
                        .waited_ms
                        .fetch_add(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                }
                let guard = Guard {
                    shared: sh.clone(),
                    instance: b.instance.clone(),
                };
                return Ok((b, guard));
            }
        }
        // Everything is full (or nothing is running yet): wait in line.
        if !counted {
            if sh.queued.load(Ordering::Relaxed) >= sh.cfg.max_queue {
                sh.counters.refused.fetch_add(1, Ordering::Relaxed);
                return Err(error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "The cluster is full. Try again shortly.",
                ));
            }
            sh.queued.fetch_add(1, Ordering::Relaxed);
            counted = true;
        }
        if Instant::now() >= deadline {
            sh.queued.fetch_sub(1, Ordering::Relaxed);
            sh.counters.refused.fetch_add(1, Ordering::Relaxed);
            return Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Timed out waiting for a free model server",
            ));
        }
        // Woken when a request finishes; also re-check now and then, since other gateways and
        // direct users change a server's load without telling us.
        let _ = tokio::time::timeout(Duration::from_millis(400), sh.freed.notified()).await;
    }
}

async fn send(
    sh: &Shared,
    addr: SocketAddr,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: axum::body::Bytes,
) -> Result<reqwest::Response, String> {
    let url = format!(
        "http://{addr}{}",
        uri.path_and_query().map(|p| p.as_str()).unwrap_or("/")
    );
    let mut rb = sh.http.request(method.clone(), url);
    for (k, v) in headers {
        let name = k.as_str();
        if is_hop(name) || name == "authorization" {
            continue;
        }
        rb = rb.header(k, v);
    }
    rb.body(body).send().await.map_err(|e| e.to_string())
}

fn stream_back(resp: reqwest::Response, guard: Guard) -> Response<Body> {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut out = Response::builder().status(status);
    for (k, v) in resp.headers() {
        if is_hop(k.as_str()) {
            continue;
        }
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_str().as_bytes()),
            HeaderValue::from_bytes(v.as_bytes()),
        ) {
            out = out.header(name, val);
        }
    }
    let stream = Guarded {
        inner: Box::pin(resp.bytes_stream()),
        _guard: guard,
    };
    out.body(Body::from_stream(stream)).unwrap()
}

fn status_of(sh: &Shared) -> GatewayStatus {
    let inflight: u32 = sh.inflight.lock().unwrap().values().sum();
    GatewayStatus {
        running: true,
        url: String::new(),
        backends: sh.backends(),
        inflight,
        queued: sh.queued.load(Ordering::Relaxed),
        total: sh.counters.total.load(Ordering::Relaxed),
        failed: sh.counters.failed.load(Ordering::Relaxed),
        retried: sh.counters.retried.load(Ordering::Relaxed),
        refused: sh.counters.refused.load(Ordering::Relaxed),
    }
}

impl Gateway {
    /// Starts listening. Listening beyond this machine needs at least one API key.
    pub async fn start(node: Node, cfg: GatewayConfig) -> Result<Gateway, String> {
        if cfg.network && cfg.api_keys.is_empty() {
            return Err("Sharing the gateway on the network needs an API key".into());
        }
        let host = if cfg.network { "0.0.0.0" } else { "127.0.0.1" };
        let listener = tokio::net::TcpListener::bind((host, cfg.port))
            .await
            .map_err(|e| format!("Could not open port {}: {e}", cfg.port))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        let shared = Arc::new(Shared {
            node: node.clone(),
            cfg,
            http,
            inflight: Mutex::new(HashMap::new()),
            bad: Mutex::new(HashMap::new()),
            queued: std::sync::atomic::AtomicUsize::new(0),
            freed: Notify::new(),
            counters: Counters::default(),
        });
        let app = Router::new()
            .fallback(any(handle))
            .with_state(shared.clone());
        let (tx, rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
        });
        // Tell the cluster where this gateway is and how busy it is.
        let port = addr.port();
        let sh2 = shared.clone();
        let beat = tokio::spawn(async move {
            loop {
                let inflight: u32 = sh2.inflight.lock().unwrap().values().sum();
                sh2.node.mesh().update_self(|r| {
                    let c = r.gateway_port != port || r.gateway_inflight != inflight;
                    r.gateway_port = port;
                    r.gateway_inflight = inflight;
                    c
                });
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
        Ok(Gateway {
            addr,
            shared,
            stop: Some(tx),
            task,
            beat,
        })
    }

    pub fn status(&self) -> GatewayStatus {
        let mut s = status_of(&self.shared);
        s.url = format!("http://{}/v1", self.addr);
        s
    }

    pub async fn stop(mut self) {
        self.beat.abort();
        if let Some(t) = self.stop.take() {
            let _ = t.send(());
        }
        self.shared.node.mesh().update_self(|r| {
            let c = r.gateway_port != 0;
            r.gateway_port = 0;
            r.gateway_inflight = 0;
            c
        });
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

#[cfg(test)]
mod tests;

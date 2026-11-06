//! A small local HTTP API for running a node without a screen (`lanlm-node`), so servers can be
//! set up and watched from scripts. It listens on this machine only and needs the token that is
//! written next to the node's settings.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, Response, StatusCode};
use axum::routing::any;
use axum::Router;
use rand::RngCore;
use serde_json::{json, Value};

use crate::control::{Controller, GatewaySettings};
use crate::proto::{Command, Deployment};

/// Reads the admin token, creating it the first time.
pub fn token(data_dir: &Path) -> String {
    let p = data_dir.join("admin.token");
    if let Ok(t) = std::fs::read_to_string(&p) {
        let t = t.trim().to_string();
        if t.len() >= 32 {
            return t;
        }
    }
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    let t = hex::encode(b);
    let _ = std::fs::create_dir_all(data_dir);
    let _ = std::fs::write(&p, &t);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    t
}

fn reply(code: StatusCode, v: Value) -> Response<Body> {
    Response::builder()
        .status(code)
        .header("content-type", "application/json")
        .body(Body::from(v.to_string()))
        .unwrap()
}

fn ok(v: Value) -> Response<Body> {
    reply(StatusCode::OK, v)
}

fn bad(msg: impl Into<String>) -> Response<Body> {
    reply(StatusCode::BAD_REQUEST, json!({ "error": msg.into() }))
}

fn query_of(uri: &axum::http::Uri, key: &str) -> Option<String> {
    uri.query()?
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.replace('+', " "))
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |x, (p, q)| x | (p ^ q)) == 0
}

#[derive(Clone)]
struct Shared {
    ctl: Controller,
    token: String,
}

/// Starts the admin API. The returned task runs until dropped or aborted.
pub async fn serve(
    ctl: Controller,
    addr: SocketAddr,
    token: String,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>), String> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Could not open the admin port {addr}: {e}"))?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?;
    let app = Router::new()
        .fallback(any(handle))
        .with_state(Shared { ctl, token });
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((bound, task))
}

async fn handle(State(sh): State<Shared>, req: Request<Body>) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let given = parts
        .headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !same(given, &sh.token) {
        return reply(
            StatusCode::UNAUTHORIZED,
            json!({ "error": "bad admin token" }),
        );
    }
    let bytes = axum::body::to_bytes(body, 4 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let b: Value = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => return bad(format!("bad JSON: {e}")),
        }
    };
    let c = &sh.ctl;
    let s = |k: &str| b[k].as_str().unwrap_or("").to_string();
    let node = s("node");
    match (&parts.method, parts.uri.path()) {
        (&Method::GET, "/api/status") => {
            ok(serde_json::to_value(c.status().await).unwrap_or_default())
        }
        (&Method::GET, "/api/logs") => ok(json!({ "logs": c.node().logs() })),
        (&Method::GET, "/api/preview") => {
            let q = |k: &str| query_of(&parts.uri, k).unwrap_or_default();
            let n = |k: &str, d: u32| q(k).parse().unwrap_or(d);
            let r = c.preview(
                &q("model"),
                &q("mode"),
                n("ctx", 4096),
                n("parallel", 2),
                n("max", 0),
            );
            ok(serde_json::to_value(r).unwrap_or_default())
        }
        (&Method::POST, "/api/cluster/create") => {
            let key = c.create_cluster(&s("name"));
            ok(json!({ "key": key }))
        }
        (&Method::POST, "/api/cluster/key") => match c.cluster_key() {
            Some(k) => ok(json!({ "key": k })),
            None => bad("this device is not in a cluster"),
        },
        (&Method::POST, "/api/cluster/join") => match c.join_cluster(&s("key")) {
            Ok(()) => ok(json!({})),
            Err(e) => bad(e),
        },
        (&Method::POST, "/api/cluster/leave") => {
            c.leave_cluster().await;
            ok(json!({}))
        }
        (&Method::POST, "/api/peer") => {
            c.mesh().add_peer(&s("addr"));
            ok(json!({}))
        }
        (&Method::POST, "/api/pair/open") => {
            c.mesh().open_pairing();
            ok(json!({}))
        }
        (&Method::POST, "/api/pair/close") => {
            c.mesh().close_pairing();
            ok(json!({}))
        }
        (&Method::POST, "/api/pair/decide") => {
            let found = c.mesh().pair_decide(
                b["id"].as_u64().unwrap_or(0),
                b["allow"].as_bool().unwrap_or(false),
            );
            ok(json!({ "found": found }))
        }
        (&Method::POST, "/api/pair/start") => {
            c.mesh().pair_start(s("addr"));
            ok(json!({}))
        }
        (&Method::POST, "/api/deploy") => match serde_json::from_value::<Deployment>(b.clone()) {
            Ok(d) => match c.deploy(d) {
                Ok(id) => ok(json!({ "id": id })),
                Err(e) => bad(e),
            },
            Err(e) => bad(format!("bad deployment: {e}")),
        },
        (&Method::POST, "/api/undeploy") => match c.undeploy(&s("id")).await {
            Ok(()) => ok(json!({})),
            Err(e) => bad(e),
        },
        (&Method::POST, "/api/fetch") => {
            let url = Some(s("url")).filter(|u| !u.is_empty());
            let sha = Some(s("sha256")).filter(|u| !u.is_empty());
            match c.fetch_model(&node, &s("file"), url, sha).await {
                Ok(()) => ok(json!({})),
                Err(e) => bad(e),
            }
        }
        (&Method::POST, "/api/delete") => {
            match c
                .command(
                    &node,
                    Command::DeleteModel { file: s("file") },
                    Duration::from_secs(60),
                )
                .await
            {
                Ok(_) => ok(json!({})),
                Err(e) => bad(e),
            }
        }
        (&Method::POST, "/api/bench") => {
            match c
                .command(&node, Command::Bench, Duration::from_secs(400))
                .await
            {
                Ok(v) => ok(v),
                Err(e) => bad(e),
            }
        }
        (&Method::POST, "/api/drain") => {
            let on = b["on"].as_bool().unwrap_or(true);
            match c
                .command(&node, Command::SetDraining { on }, Duration::from_secs(30))
                .await
            {
                Ok(_) => ok(json!({})),
                Err(e) => bad(e),
            }
        }
        (&Method::POST, "/api/gateway") => {
            let gs: GatewaySettings = serde_json::from_value(b.clone()).unwrap_or_default();
            if b["enabled"].as_bool().unwrap_or(true) {
                match c.gateway_start(gs).await {
                    Ok(st) => ok(serde_json::to_value(st).unwrap_or_default()),
                    Err(e) => bad(e),
                }
            } else {
                let _ = c.gateway_stop().await;
                ok(json!({}))
            }
        }
        (&Method::POST, "/api/engine") => {
            let v = Some(s("variant")).filter(|v| !v.is_empty());
            match c.install_engine(v.as_deref(), |_| {}).await {
                Ok(i) => ok(json!({ "tag": i.tag, "variant": i.variant })),
                Err(e) => bad(e),
            }
        }
        _ => reply(StatusCode::NOT_FOUND, json!({ "error": "unknown request" })),
    }
}

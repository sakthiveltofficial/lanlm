//! `lanlm-node`: run a LanLM node without a screen (servers, headless boxes), and control it
//! from the command line.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use lanlm_mesh::admin;
use lanlm_mesh::control::{Controller, ControllerConfig, GatewaySettings};
use lanlm_mesh::engine::Step;
use lanlm_mesh::proto::Deployment;
use lanlm_mesh::Event;
use serde_json::{json, Value};

const HELP: &str = "\
lanlm-node: a LanLM cluster node without a screen

USAGE
  lanlm-node run [options]          start this device and keep it in the cluster
  lanlm-node install-engine [options]   download the llama.cpp engine that fits this machine
  lanlm-node <command> [options]    control a running node on this machine

RUN OPTIONS
  --data-dir DIR        settings, identity and models (default ~/.lanlm, or $LANLM_DATA)
  --engine-dir DIR      llama.cpp programs (default DATA/engine)
  --models-dir DIR      model files (default DATA/models)
  --name NAME           this device's name (default: the computer name)
  --port N              cluster port (default 50500)
  --admin-port N        local control port (default 50501)
  --no-discovery        do not announce or look for devices on the local network
  --create-cluster NAME start a new cluster and print its key
  --join KEY            join the cluster with this key
  --peer HOST:PORT      also connect to this address (when discovery is blocked)
  --gateway PORT        serve the OpenAI API for the whole cluster on this port
  --gateway-key KEY     an API key for the gateway (repeatable)
  --gateway-network     let other computers use the gateway (needs a key)
  --deploy MODEL.gguf   keep this model running (with --mode, --ctx, --parallel, --url, --max)

COMMANDS (talk to the node started with `run`)
  status                   show devices, models and what is running where
  deploy MODEL.gguf        --mode auto|replicas|pipeline --ctx N --parallel N --url URL --max N
  undeploy ID
  preview MODEL.gguf       show where a model would go, without starting it
  fetch FILE               --url URL --sha256 HEX --node ID    copy a model to a device
  invite                   let a new device ask to join (shows requests; use `allow`)
  allow ID | deny ID       answer a join request
  pair HOST:PORT           ask a device to let this one in
  join KEY | leave | key
  logs
  bench                    measure this device's speed

Control commands accept --data-dir and --admin-port (to find the node and its token).
";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter(|w| w[0] == name)
        .map(|w| w[1].clone())
        .collect()
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn data_dir(args: &[String]) -> PathBuf {
    if let Some(d) = flag(args, "--data-dir") {
        return PathBuf::from(d);
    }
    if let Ok(d) = std::env::var("LANLM_DATA") {
        return PathBuf::from(d);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".lanlm")
}

fn admin_port(args: &[String]) -> u16 {
    flag(args, "--admin-port")
        .and_then(|p| p.parse().ok())
        .unwrap_or(50501)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    let code = match cmd {
        "run" => run(&args[1..]).await,
        "install-engine" => install_engine(&args[1..]).await,
        "help" | "--help" | "-h" => {
            print!("{HELP}");
            Ok(())
        }
        _ => client(cmd, &args[1..]).await,
    };
    if let Err(e) = code {
        eprintln!("lanlm-node: {e}");
        std::process::exit(1);
    }
}

fn config(args: &[String]) -> ControllerConfig {
    let dir = data_dir(args);
    let mut cfg = ControllerConfig::in_dir(dir);
    if let Some(d) = flag(args, "--engine-dir") {
        cfg.engine_dir = d.into();
    }
    if let Some(d) = flag(args, "--models-dir") {
        cfg.models_dir = d.into();
    }
    cfg.name = flag(args, "--name");
    if let Some(p) = flag(args, "--port").and_then(|p| p.parse().ok()) {
        cfg.port = p;
    }
    cfg.discovery = !has(args, "--no-discovery");
    cfg
}

async fn install_engine(args: &[String]) -> Result<(), String> {
    let cfg = config(args);
    let http = reqwest::Client::new();
    let variant = flag(args, "--variant");
    let mut last = 0u64;
    let r = lanlm_mesh::engine::install(&http, &cfg.engine_dir, variant.as_deref(), |s| match s {
        Step::Status(t) => eprintln!("{t}"),
        Step::Bytes { done, total } => {
            if total > 0 && done / (50 << 20) != last {
                last = done / (50 << 20);
                eprintln!("  {:.0} of {:.0} MB", done as f64 / 1e6, total as f64 / 1e6);
            }
        }
        Step::Done => {}
    })
    .await?;
    println!("Installed llama.cpp {} ({} build)", r.tag, r.variant);
    Ok(())
}

async fn run(args: &[String]) -> Result<(), String> {
    let cfg = config(args);
    let dir = cfg.data_dir.clone();
    let ctl = Controller::start(cfg).await?;
    let mesh = ctl.mesh().clone();

    // Say what happens (subscribed first, so nothing is missed).
    let mut events = mesh.subscribe();
    tokio::spawn(async move {
        while let Ok(e) = events.recv().await {
            match e {
                Event::PeerUp { name, .. } => println!("+ {name} joined the mesh"),
                Event::PeerDown { name, .. } => println!("- {name} left the mesh"),
                Event::PairRequested { name, sas } => {
                    println!("? {name} wants to join. Code {sas}. Run `lanlm-node allow ID` (see `status`)")
                }
                Event::Joined => println!("* joined a cluster"),
                Event::Left => println!("* left the cluster"),
                Event::Log(_) => {}
            }
        }
    });

    println!("LanLM node '{}' ({})", mesh.name(), &mesh.id()[..12]);
    println!("  listening for cluster traffic on port {}", mesh.port());

    if let Some(name) = flag(args, "--create-cluster") {
        if mesh.joined() {
            return Err("this device is already in a cluster (use `leave` first)".into());
        }
        let key = ctl.create_cluster(&name);
        println!("\nCreated cluster '{name}'. To add a device, run on it:\n  lanlm-node run --join {key}\n");
    }
    if let Some(key) = flag(args, "--join") {
        ctl.join_cluster(&key)?;
        println!("  joined the cluster");
    }
    for p in flags(args, "--peer") {
        mesh.add_peer(&p);
    }
    if !mesh.joined() {
        println!("  not in a cluster yet: use --create-cluster NAME or --join KEY");
    }

    // Local control API.
    let token = admin::token(&dir);
    let addr = ([127, 0, 0, 1], admin_port(args)).into();
    let (bound, _task) = admin::serve(ctl.clone(), addr, token).await?;
    println!(
        "  control API on http://{bound} (token in {})",
        dir.join("admin.token").display()
    );

    if let Some(p) = flag(args, "--gateway") {
        let port: u16 = p.parse().map_err(|_| "--gateway needs a port number")?;
        let st = ctl
            .gateway_start(GatewaySettings {
                enabled: true,
                port,
                network: has(args, "--gateway-network"),
                api_keys: flags(args, "--gateway-key"),
            })
            .await?;
        println!("  OpenAI API for the cluster: {}", st.url);
    }
    if let Some(model) = flag(args, "--deploy") {
        let d = Deployment {
            id: flag(args, "--id").unwrap_or_default(),
            model,
            mode: flag(args, "--mode").unwrap_or_else(|| "auto".into()),
            ctx: flag(args, "--ctx")
                .and_then(|v| v.parse().ok())
                .unwrap_or(4096),
            parallel: flag(args, "--parallel")
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            max_instances: flag(args, "--max")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            url: flag(args, "--url").unwrap_or_default(),
            ..Default::default()
        };
        let id = ctl.deploy(d)?;
        println!("  deploying as {id}");
    }

    println!("\nRunning. Press Ctrl+C to stop.");
    let _ = tokio::signal::ctrl_c().await;
    println!("Stopping…");
    ctl.shutdown().await;
    Ok(())
}

// ───────────────────────────── client commands ─────────────────────────────

async fn call(
    args: &[String],
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let dir = data_dir(args);
    let token = std::fs::read_to_string(dir.join("admin.token")).map_err(|_| {
        format!(
            "No node found in {}. Is `lanlm-node run` running?",
            dir.display()
        )
    })?;
    let url = format!("http://127.0.0.1:{}{path}", admin_port(args));
    let c = reqwest::Client::builder()
        .timeout(Duration::from_secs(4 * 3600))
        .build()
        .map_err(|e| e.to_string())?;
    let mut rb = if method == "GET" {
        c.get(url)
    } else {
        c.post(url)
    };
    rb = rb.header("x-admin-token", token.trim());
    if let Some(b) = body {
        rb = rb.json(&b);
    }
    let r = rb
        .send()
        .await
        .map_err(|_| "Could not reach the node. Is `lanlm-node run` running?".to_string())?;
    let ok = r.status().is_success();
    let v: Value = r.json().await.map_err(|e| e.to_string())?;
    if ok {
        Ok(v)
    } else {
        Err(v["error"].as_str().unwrap_or("request failed").to_string())
    }
}

fn gb(mb: f64) -> String {
    format!("{:.1} GB", mb / 1024.0)
}

fn print_status(v: &Value) {
    let m = &v["mesh"];
    if !m["joined"].as_bool().unwrap_or(false) {
        println!("Not in a cluster. Use `join KEY` or start with --create-cluster NAME.");
    } else {
        println!(
            "Cluster '{}' ({}). You are {}.",
            m["clusterName"].as_str().unwrap_or(""),
            m["clusterId"].as_str().unwrap_or(""),
            m["name"].as_str().unwrap_or("")
        );
    }
    println!(
        "{:<18} {:<7} {:<34} {:<24} RUNNING",
        "DEVICE", "STATE", "HARDWARE", "MODELS"
    );
    let none: Vec<Value> = Vec::new();
    for n in m["nodes"].as_array().unwrap_or(&none) {
        let r = &n["record"];
        let gpus: Vec<String> = r["hw"]["gpus"]
            .as_array()
            .unwrap_or(&none)
            .iter()
            .map(|g| {
                format!(
                    "{} {}",
                    g["name"].as_str().unwrap_or(""),
                    gb(g["vramTotalMb"].as_f64().unwrap_or(0.0))
                )
            })
            .collect();
        let hw = format!(
            "{} RAM{}",
            gb(r["hw"]["ramFreeMb"].as_f64().unwrap_or(0.0)),
            if gpus.is_empty() {
                String::new()
            } else {
                format!(" + {}", gpus.join(", "))
            }
        );
        let models: Vec<&str> = r["models"]
            .as_array()
            .unwrap_or(&none)
            .iter()
            .filter_map(|x| x["file"].as_str())
            .collect();
        let running = if let Some(i) = r["instance"].as_object() {
            let layers: u32 = i["stages"]
                .as_array()
                .map(|s| {
                    s.iter()
                        .map(|x| x["layers"].as_u64().unwrap_or(0) as u32)
                        .sum()
                })
                .unwrap_or(0);
            format!(
                "{} [{}] {} {}/{} busy, {:.0} tok/s ({} layers)",
                i["model"].as_str().unwrap_or(""),
                i["kind"].as_str().unwrap_or(""),
                i["state"].as_str().unwrap_or(""),
                i["busy"],
                i["slots"],
                i["tokS"].as_f64().unwrap_or(0.0),
                layers
            )
        } else if let Some(s) = r["share"].as_object() {
            format!(
                "holding layers {}–{} of {}",
                s["layerFrom"],
                s["layerFrom"].as_u64().unwrap_or(0) + s["layers"].as_u64().unwrap_or(0),
                s["model"].as_str().unwrap_or("")
            )
        } else {
            "idle".into()
        };
        let state = if n["isMe"].as_bool().unwrap_or(false) {
            "you"
        } else if n["alive"].as_bool().unwrap_or(false) {
            "up"
        } else {
            "gone"
        };
        println!(
            "{:<18} {:<7} {:<34} {:<24} {}",
            r["name"].as_str().unwrap_or(""),
            state,
            hw,
            models.join(", "),
            running
        );
    }
    let empty = HashMap::new();
    let ds = v["deployStatus"]
        .as_object()
        .map(|o| o.iter().collect::<HashMap<_, _>>())
        .unwrap_or(empty);
    for d in v["deployments"].as_array().unwrap_or(&none) {
        let id = d["id"].as_str().unwrap_or("");
        println!(
            "\nDeployment {} — {} ({}): {}",
            id,
            d["model"].as_str().unwrap_or(""),
            d["mode"].as_str().unwrap_or(""),
            ds.get(&id.to_string())
                .and_then(|s| s["message"].as_str())
                .unwrap_or("")
        );
    }
    if let Some(g) = v["gateway"].as_object() {
        println!(
            "\nGateway {}: {} server(s), {} in flight, {} waiting, {} served, {} failed",
            g["url"].as_str().unwrap_or(""),
            g["backends"].as_array().map(|b| b.len()).unwrap_or(0),
            g["inflight"],
            g["queued"],
            g["total"],
            g["failed"]
        );
    }
    for p in m["pairRequests"].as_array().unwrap_or(&none) {
        println!(
            "\n? Join request #{} from '{}' — code {}  (allow {} / deny {})",
            p["id"],
            p["name"].as_str().unwrap_or(""),
            p["sas"].as_str().unwrap_or(""),
            p["id"],
            p["id"]
        );
    }
}

async fn client(cmd: &str, a: &[String]) -> Result<(), String> {
    let first = a
        .first()
        .filter(|x| !x.starts_with("--"))
        .cloned()
        .unwrap_or_default();
    let get_u32 = |n: &str, d: u32| flag(a, n).and_then(|v| v.parse().ok()).unwrap_or(d);
    match cmd {
        "status" => {
            let v = call(a, "GET", "/api/status", None).await?;
            print_status(&v);
        }
        "logs" => {
            let v = call(a, "GET", "/api/logs", None).await?;
            for l in v["logs"].as_array().unwrap_or(&Vec::new()) {
                println!("{}", l.as_str().unwrap_or(""));
            }
        }
        "key" => {
            let k = call(a, "POST", "/api/cluster/key", Some(json!({}))).await?;
            println!("{}", k["key"].as_str().unwrap_or(""));
        }
        "deploy" => {
            if first.is_empty() {
                return Err("deploy needs a model file name".into());
            }
            let v = call(
                a,
                "POST",
                "/api/deploy",
                Some(json!({
                    "model": first,
                    "mode": flag(a, "--mode").unwrap_or_else(|| "auto".into()),
                    "ctx": get_u32("--ctx", 4096),
                    "parallel": get_u32("--parallel", 2),
                    "maxInstances": get_u32("--max", 0),
                    "url": flag(a, "--url").unwrap_or_default(),
                    "sha256": flag(a, "--sha256").unwrap_or_default(),
                })),
            )
            .await?;
            println!(
                "Deploying as {}. Watch it with `status`.",
                v["id"].as_str().unwrap_or("")
            );
        }
        "undeploy" => {
            call(a, "POST", "/api/undeploy", Some(json!({ "id": first }))).await?;
            println!("Stopped {first}");
        }
        "preview" => {
            let q = format!(
                "/api/preview?model={}&mode={}&ctx={}&parallel={}&max={}",
                first,
                flag(a, "--mode").unwrap_or_else(|| "auto".into()),
                get_u32("--ctx", 4096),
                get_u32("--parallel", 2),
                get_u32("--max", 0)
            );
            let v = call(a, "GET", &q, None).await?;
            if let Some(e) = v["error"].as_str() {
                println!("Cannot place it: {e}");
            }
            println!("Needs about {}", gb(v["needMb"].as_f64().unwrap_or(0.0)));
            for p in v["instances"].as_array().unwrap_or(&Vec::new()) {
                println!(
                    "- {} (about {:.0} tokens/s): {}",
                    p["kind"].as_str().unwrap_or(""),
                    p["estTokS"].as_f64().unwrap_or(0.0),
                    p["note"].as_str().unwrap_or("")
                );
                for s in p["stages"].as_array().unwrap_or(&Vec::new()) {
                    println!(
                        "    {:<16} layers {}–{} on {}",
                        s["name"].as_str().unwrap_or(""),
                        s["layerFrom"],
                        s["layerFrom"].as_u64().unwrap_or(0) + s["layers"].as_u64().unwrap_or(0),
                        s["device"].as_str().unwrap_or("")
                    );
                }
            }
            for n in v["notes"].as_array().unwrap_or(&Vec::new()) {
                println!("note: {}", n.as_str().unwrap_or(""));
            }
        }
        "fetch" => {
            if first.is_empty() {
                return Err("fetch needs a model file name".into());
            }
            call(
                a,
                "POST",
                "/api/fetch",
                Some(json!({
                    "node": flag(a, "--node").unwrap_or_default(),
                    "file": first,
                    "url": flag(a, "--url").unwrap_or_default(),
                    "sha256": flag(a, "--sha256").unwrap_or_default(),
                })),
            )
            .await?;
            println!("Done.");
        }
        "invite" => {
            call(a, "POST", "/api/pair/open", Some(json!({}))).await?;
            println!("Pairing is open for 2 minutes. On the new device run `lanlm-node pair THIS-ADDRESS:PORT`,");
            println!("then check that both screens show the same code and run `lanlm-node allow ID` here.");
        }
        "allow" | "deny" => {
            let id: u64 = first
                .parse()
                .map_err(|_| "give the request number from `status`")?;
            let v = call(
                a,
                "POST",
                "/api/pair/decide",
                Some(json!({ "id": id, "allow": cmd == "allow" })),
            )
            .await?;
            if v["found"] == true {
                println!(
                    "{}",
                    if cmd == "allow" {
                        "Allowed."
                    } else {
                        "Refused."
                    }
                );
            } else {
                println!("No such request (it may have expired).");
            }
        }
        "pair" => {
            call(a, "POST", "/api/pair/start", Some(json!({ "addr": first }))).await?;
            for _ in 0..140 {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let v = call(a, "GET", "/api/status", None).await?;
                let st = &v["mesh"]["pairStatus"];
                match st["state"].as_str().unwrap_or("") {
                    "waiting" => println!(
                        "Code {} — it must match the other screen. Waiting for it to be allowed…",
                        st["sas"].as_str().unwrap_or("")
                    ),
                    "joined" => {
                        println!("Joined the cluster.");
                        return Ok(());
                    }
                    "denied" => return Err("The other device refused.".into()),
                    "failed" => return Err(st["error"].as_str().unwrap_or("failed").to_string()),
                    _ => {}
                }
            }
            return Err("timed out".into());
        }
        "join" => {
            call(
                a,
                "POST",
                "/api/cluster/join",
                Some(json!({ "key": first })),
            )
            .await?;
            println!("Joined.");
        }
        "leave" => {
            call(a, "POST", "/api/cluster/leave", Some(json!({}))).await?;
            println!("Left the cluster.");
        }
        "bench" => {
            let v = call(
                a,
                "POST",
                "/api/bench",
                Some(json!({ "node": flag(a, "--node").unwrap_or_default() })),
            )
            .await?;
            println!(
                "{:.1} tokens/s with {}",
                v["tokS"].as_f64().unwrap_or(0.0),
                v["model"].as_str().unwrap_or("")
            );
        }
        _ => {
            print!("{HELP}");
            return Err(format!("unknown command '{cmd}'"));
        }
    }
    Ok(())
}

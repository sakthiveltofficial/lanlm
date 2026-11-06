//! The one place that runs a LanLM node: used by the desktop app and by the headless daemon.
//! Everything the screen or the command line can do is a method here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::{self, Step};
use crate::gateway::{Gateway, GatewayConfig, GatewayStatus};
use crate::mesh::{Mesh, MeshConfig, Snapshot};
use crate::node::{Node, NodeConfig};
use crate::orchestrator::DeployStatus;
use crate::planner::PlanResult;
use crate::proto::{Command, Deployment};

pub struct ControllerConfig {
    pub data_dir: PathBuf,
    pub engine_dir: PathBuf,
    pub models_dir: PathBuf,
    pub name: Option<String>,
    pub port: u16,
    pub discovery: bool,
    pub version: String,
}

impl ControllerConfig {
    /// Everything under one folder: `engine/`, `models/`, `rpc-cache/` and the mesh settings.
    pub fn in_dir(dir: PathBuf) -> ControllerConfig {
        ControllerConfig {
            engine_dir: dir.join("engine"),
            models_dir: dir.join("models"),
            data_dir: dir,
            name: None,
            port: crate::mesh::DEFAULT_PORT,
            discovery: true,
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// What a person wants from the gateway. Saved, so it comes back after a restart.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct GatewaySettings {
    pub enabled: bool,
    pub port: u16,
    pub network: bool,
    pub api_keys: Vec<String>,
}

impl Default for GatewaySettings {
    fn default() -> Self {
        GatewaySettings {
            enabled: false,
            port: 8080,
            network: false,
            api_keys: vec![],
        }
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub mesh: Snapshot,
    pub deployments: Vec<Deployment>,
    pub deploy_status: HashMap<String, DeployStatus>,
    pub gateway: Option<GatewayStatus>,
    pub gateway_settings: GatewaySettings,
    pub engine_dir: String,
    pub models_dir: String,
}

#[derive(Clone)]
pub struct Controller(Arc<Inner>);

struct Inner {
    mesh: Mesh,
    node: Node,
    http: reqwest::Client,
    cfg: ControllerConfig,
    gateway: tokio::sync::Mutex<Option<Gateway>>,
    /// A key only this app knows, so its own chat can use a gateway that asks for keys.
    internal_key: String,
}

impl Controller {
    pub async fn start(cfg: ControllerConfig) -> Result<Controller, String> {
        let mut mc = MeshConfig::new(cfg.data_dir.clone());
        mc.name = cfg.name.clone();
        mc.port = cfg.port;
        mc.discovery = cfg.discovery;
        mc.version = cfg.version.clone();
        let mesh = Mesh::start(mc).await?;
        let http = reqwest::Client::builder()
            .user_agent("LanLM")
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| e.to_string())?;
        let node = Node::start(
            mesh.clone(),
            NodeConfig {
                engine_dir: cfg.engine_dir.clone(),
                models_dir: cfg.models_dir.clone(),
                cache_dir: cfg.data_dir.join("rpc-cache"),
            },
            http.clone(),
        );
        let c = Controller(Arc::new(Inner {
            mesh,
            node,
            http,
            cfg,
            gateway: tokio::sync::Mutex::new(None),
            internal_key: {
                use rand::RngCore;
                let mut b = [0u8; 18];
                rand::thread_rng().fill_bytes(&mut b);
                format!("lanlm-internal-{}", hex::encode(b))
            },
        }));
        // The gateway comes back the way it was left.
        let saved: GatewaySettings = c.0.mesh.setting("gateway").unwrap_or_default();
        if saved.enabled {
            let _ = c.gateway_start(saved).await;
        }
        Ok(c)
    }

    pub fn mesh(&self) -> &Mesh {
        &self.0.mesh
    }

    pub fn node(&self) -> &Node {
        &self.0.node
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.0.http
    }

    pub fn engine_dir(&self) -> &std::path::Path {
        &self.0.cfg.engine_dir
    }

    pub fn models_dir(&self) -> &std::path::Path {
        &self.0.cfg.models_dir
    }

    pub async fn status(&self) -> Status {
        let gateway = self.0.gateway.lock().await.as_ref().map(|g| g.status());
        Status {
            mesh: self.0.mesh.snapshot(),
            deployments: self.0.node.deployments(),
            deploy_status: self.0.node.deploy_status(),
            gateway,
            gateway_settings: self.0.mesh.setting("gateway").unwrap_or_default(),
            engine_dir: self.0.cfg.engine_dir.display().to_string(),
            models_dir: self.0.cfg.models_dir.display().to_string(),
        }
    }

    // ───────────── membership ─────────────

    pub fn create_cluster(&self, name: &str) -> String {
        self.0.mesh.create_cluster(name)
    }

    pub fn join_cluster(&self, key: &str) -> Result<(), String> {
        self.0.mesh.join_cluster(key, "")
    }

    pub async fn leave_cluster(&self) {
        let _ = self.gateway_stop().await;
        self.0.node.shutdown().await;
        self.0.mesh.leave_cluster();
    }

    pub fn cluster_key(&self) -> Option<String> {
        self.0.mesh.cluster_key_text()
    }

    // ───────────── deployments ─────────────

    pub fn deploy(&self, d: Deployment) -> Result<String, String> {
        self.0.node.deploy(d)
    }

    pub async fn undeploy(&self, id: &str) -> Result<(), String> {
        self.0.node.undeploy(id).await
    }

    pub fn preview(
        &self,
        model: &str,
        mode: &str,
        ctx: u32,
        parallel: u32,
        max: u32,
    ) -> PlanResult {
        self.0.node.preview(model, mode, ctx, parallel, max)
    }

    // ───────────── per-device actions ─────────────

    /// Runs a command on a device (this one, or another member).
    pub async fn command(&self, node: &str, cmd: Command, wait: Duration) -> Result<Value, String> {
        let target = if node.is_empty() {
            self.0.mesh.id()
        } else {
            node.to_string()
        };
        self.0.mesh.call(&target, cmd, wait).await
    }

    /// Gets a model onto a device (this one by default): from a neighbour if one has it.
    pub async fn fetch_model(
        &self,
        node: &str,
        file: &str,
        url: Option<String>,
        sha256: Option<String>,
    ) -> Result<(), String> {
        self.command(
            node,
            Command::Fetch {
                file: file.to_string(),
                url,
                sha256,
                size: None,
            },
            Duration::from_secs(4 * 3600),
        )
        .await
        .map(|_| ())
    }

    // ───────────── engine ─────────────

    pub async fn install_engine(
        &self,
        variant: Option<&str>,
        on: impl FnMut(Step),
    ) -> Result<engine::Installed, String> {
        engine::install(&self.0.http, &self.0.cfg.engine_dir, variant, on).await
    }

    // ───────────── gateway ─────────────

    pub async fn gateway_start(&self, s: GatewaySettings) -> Result<GatewayStatus, String> {
        self.gateway_stop().await.ok();
        let mut keys = s.api_keys.clone();
        if !keys.is_empty() {
            keys.push(self.0.internal_key.clone());
        }
        let cfg = GatewayConfig {
            port: s.port,
            network: s.network,
            api_keys: keys,
            ..Default::default()
        };
        let g = Gateway::start(self.0.node.clone(), cfg).await?;
        let status = g.status();
        *self.0.gateway.lock().await = Some(g);
        self.0
            .mesh
            .set_setting("gateway", &GatewaySettings { enabled: true, ..s });
        Ok(status)
    }

    pub async fn gateway_stop(&self) -> Result<(), String> {
        if let Some(g) = self.0.gateway.lock().await.take() {
            g.stop().await;
        }
        let mut s: GatewaySettings = self.0.mesh.setting("gateway").unwrap_or_default();
        if s.enabled {
            s.enabled = false;
            self.0.mesh.set_setting("gateway", &s);
        }
        Ok(())
    }

    /// Where this app's own chat should send requests so they spread over the whole cluster:
    /// the gateway (started on this machine only if it is not already running) and the key it
    /// needs, if any.
    pub async fn chat_endpoint(&self) -> Result<(std::net::SocketAddr, Option<String>), String> {
        let mut g = self.0.gateway.lock().await;
        if g.is_none() {
            let gw = Gateway::start(
                self.0.node.clone(),
                GatewayConfig {
                    port: 0,
                    ..Default::default()
                },
            )
            .await?;
            *g = Some(gw);
        }
        let addr = g
            .as_ref()
            .map(|g| g.addr)
            .ok_or("the gateway did not start")?;
        let saved: GatewaySettings = self.0.mesh.setting("gateway").unwrap_or_default();
        let key =
            (saved.enabled && !saved.api_keys.is_empty()).then(|| self.0.internal_key.clone());
        Ok((addr, key))
    }

    pub async fn shutdown(&self) {
        if let Some(g) = self.0.gateway.lock().await.take() {
            g.stop().await;
        }
        self.0.node.shutdown().await;
    }
}

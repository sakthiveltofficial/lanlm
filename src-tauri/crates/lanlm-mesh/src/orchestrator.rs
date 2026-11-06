//! Keeping the cluster running what you asked for.
//!
//! A *deployment* says "run this model, like this". The node that owns it plans where it goes,
//! starts the pieces in the right order (fetch the model, start the workers, start the server),
//! and then keeps watching: if a device vanishes or a server fails it is cleaned up and the
//! model is placed again on the devices that are left, and when a new device joins it is used.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use rand::Rng;
use serde::Serialize;
use tokio::sync::Notify;

use crate::node::Node;
use crate::planner::{self, Mode, ModelSpec, PlanResult, Planned, Want};
use crate::proto::{Command, Deployment, InstanceState, NodeRecord, Share};

const RECONCILE_EVERY: Duration = Duration::from_secs(5);
const FETCH_WAIT: Duration = Duration::from_secs(4 * 3600);
const START_WAIT: Duration = Duration::from_secs(50 * 60);
const QUICK: Duration = Duration::from_secs(60);

/// What the screen shows about one deployment.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeployStatus {
    pub message: String,
    pub is_error: bool,
    pub updated_ms: u64,
}

#[derive(Default)]
pub struct OrchState {
    pub status: HashMap<String, DeployStatus>,
    /// Shares seen without an instance once; removed if still orphaned next time.
    orphans: HashSet<String>,
    /// Models this node is fetching on behalf of a deployment.
    fetching: HashSet<String>,
}

pub struct Orch {
    pub state: std::sync::Mutex<OrchState>,
    pub kick: Notify,
}

impl Orch {
    pub fn new() -> Orch {
        Orch {
            state: std::sync::Mutex::new(OrchState::default()),
            kick: Notify::new(),
        }
    }
}

impl Default for Orch {
    fn default() -> Self {
        Orch::new()
    }
}

fn short_id() -> String {
    let n: u32 = rand::thread_rng().gen();
    format!("{n:08x}")
}

impl Node {
    fn set_status(&self, dep: &str, message: impl Into<String>, is_error: bool) {
        let message = message.into();
        let mut st = self.0.orch.state.lock().unwrap();
        st.status.insert(
            dep.to_string(),
            DeployStatus {
                message,
                is_error,
                updated_ms: crate::proto::now_ms(),
            },
        );
    }

    pub fn deploy_status(&self) -> HashMap<String, DeployStatus> {
        self.0.orch.state.lock().unwrap().status.clone()
    }

    pub fn deployments(&self) -> Vec<Deployment> {
        self.0.deployments.lock().unwrap().clone()
    }

    fn save_deployments(&self) {
        let list = self.deployments();
        self.0.mesh.set_setting("deployments", &list);
        self.0.mesh.update_self(|r| {
            if r.deployments == list {
                return false;
            }
            r.deployments = list.clone();
            true
        });
    }

    /// Starts keeping a model running. Replaces a deployment with the same id.
    pub fn deploy(&self, mut d: Deployment) -> Result<String, String> {
        crate::store::safe_name(&d.model)?;
        if !self.0.mesh.joined() {
            return Err("Join or create a cluster first".into());
        }
        if d.id.is_empty() {
            d.id = format!("d{}", short_id());
        }
        if d.ctx == 0 {
            d.ctx = 4096;
        }
        if d.parallel == 0 {
            d.parallel = 2;
        }
        if !d.url.is_empty() && !crate::store::link_allowed(&d.url) {
            return Err("Only https:// links can be downloaded".into());
        }
        let id = d.id.clone();
        {
            let mut list = self.0.deployments.lock().unwrap();
            list.retain(|x| x.id != d.id);
            list.push(d);
        }
        self.save_deployments();
        self.set_status(&id, "Planning…", false);
        self.0.orch.kick.notify_one();
        Ok(id)
    }

    /// Stops a deployment and everything it started on any device.
    pub async fn undeploy(&self, id: &str) -> Result<(), String> {
        {
            let mut list = self.0.deployments.lock().unwrap();
            if !list.iter().any(|d| d.id == id) {
                return Err("No such deployment on this device".into());
            }
            list.retain(|d| d.id != id);
        }
        self.save_deployments();
        self.0.orch.state.lock().unwrap().status.remove(id);
        self.stop_everything_of(id).await;
        Ok(())
    }

    /// Stops all instances and shares that belong to deployment `dep`, wherever they run.
    async fn stop_everything_of(&self, dep: &str) {
        let prefix = format!("{dep}-");
        let targets: Vec<(String, Vec<Command>)> = self.0.mesh.with_registry(|reg, now| {
            reg.live(now)
                .into_iter()
                .filter_map(|e| {
                    let mut cmds = Vec::new();
                    if let Some(i) = &e.record.instance {
                        if i.deployment == dep {
                            cmds.push(Command::StopInstance {
                                instance: i.id.clone(),
                            });
                        }
                    }
                    if let Some(s) = &e.record.share {
                        if s.instance.starts_with(&prefix) {
                            cmds.push(Command::StopShare {
                                instance: s.instance.clone(),
                            });
                        }
                    }
                    (!cmds.is_empty()).then(|| (e.record.node_id.clone(), cmds))
                })
                .collect()
        });
        for (node, cmds) in targets {
            for c in cmds {
                let _ = self.0.mesh.call(&node, c, START_WAIT).await;
            }
        }
    }

    /// Looks up the model's size and layer count from any device that holds it.
    fn model_spec(&self, file: &str) -> Option<ModelSpec> {
        self.0.mesh.with_registry(|reg, now| {
            reg.holders(file, now)
                .into_iter()
                .filter_map(|e| e.record.models.iter().find(|m| m.file == file))
                .find(|m| m.n_layer > 0 && m.size > 0)
                .map(|m| ModelSpec {
                    file: file.to_string(),
                    size_mb: m.size as f64 / 1048576.0,
                    n_layer: m.n_layer,
                })
        })
    }

    fn caps_for(&self, d: &Deployment, reserved: &HashSet<String>) -> Vec<planner::NodeCaps> {
        self.0.mesh.with_registry(|reg, now| {
            reg.live(now)
                .into_iter()
                .map(|e| {
                    let r: &NodeRecord = &e.record;
                    let free = r.lend
                        && !r.draining
                        && r.instance.is_none()
                        && r.share.is_none()
                        && !reserved.contains(&r.node_id)
                        && (d.nodes.is_empty() || d.nodes.contains(&r.node_id));
                    planner::caps_from_record(r, free, &d.model, &HashMap::new())
                })
                .collect()
        })
    }

    /// What a deployment would do right now, without doing it.
    pub fn preview(
        &self,
        model: &str,
        mode: &str,
        ctx: u32,
        parallel: u32,
        max: u32,
    ) -> PlanResult {
        let d = Deployment {
            model: model.to_string(),
            mode: mode.to_string(),
            ctx,
            parallel,
            max_instances: max,
            ..Default::default()
        };
        let Some(spec) = self.model_spec(model) else {
            return PlanResult {
                error: Some(
                    "No device in the cluster has this model yet (or its details are still being read)."
                        .into(),
                ),
                ..Default::default()
            };
        };
        let reserved = self.0.launching.lock().unwrap().clone();
        let nodes = self.caps_for(&d, &reserved);
        planner::plan(
            &nodes,
            &spec,
            &Want {
                mode: Mode::parse(mode),
                ctx: ctx.max(512),
                parallel: parallel.max(1),
                max_instances: max,
            },
        )
    }

    // ───────────── one pass over one deployment ─────────────

    async fn reconcile(&self, d: &Deployment) {
        // 1. Clean up what is broken.
        self.heal(d).await;

        // 2. How many are running or starting?
        let (active, starting_here) = {
            let launching = self.0.launching.lock().unwrap().clone();
            let live: usize = self.0.mesh.with_registry(|reg, now| {
                reg.instances(now)
                    .into_iter()
                    .filter(|(_, i)| {
                        i.deployment == d.id
                            && matches!(i.state, InstanceState::Loading | InstanceState::Ready)
                    })
                    .count()
            });
            let starting = launching
                .iter()
                .filter(|x| x.starts_with(&format!("{}-", d.id)))
                .count();
            (live, starting)
        };
        let have = active.max(starting_here);
        if d.max_instances > 0 && have as u32 >= d.max_instances {
            return;
        }

        // 3. Is the model somewhere?
        let Some(spec) = self.model_spec(&d.model) else {
            self.ensure_model(d).await;
            return;
        };

        // 4. Plan on the free devices and launch what the plan says.
        let reserved = self.0.launching.lock().unwrap().clone();
        let nodes = self.caps_for(d, &reserved);
        let want = Want {
            mode: Mode::parse(&d.mode),
            ctx: d.ctx.max(512),
            parallel: d.parallel.max(1),
            max_instances: if d.max_instances > 0 {
                d.max_instances - have as u32
            } else {
                0
            },
        };
        let plan = planner::plan(&nodes, &spec, &want);
        if plan.instances.is_empty() {
            if have == 0 {
                let msg = plan
                    .error
                    .unwrap_or_else(|| "Waiting for a free device".into());
                self.set_status(&d.id, msg, true);
            }
            return;
        }
        for p in plan.instances {
            let me = self.clone();
            let dep = d.clone();
            let want = want.clone();
            // Reserve before spawning so the next pass cannot plan onto the same devices.
            let inst = format!("{}-{}", d.id, short_id());
            {
                let mut l = self.0.launching.lock().unwrap();
                l.insert(inst.clone());
                for s in &p.stages {
                    l.insert(s.node_id.clone());
                }
            }
            tokio::spawn(async move {
                me.launch(&dep, &want, p, inst).await;
            });
        }
    }

    /// Makes sure the model exists somewhere in the cluster: this node fetches it.
    async fn ensure_model(&self, d: &Deployment) {
        if self.model_present_without_header(&d.model) {
            self.set_status(&d.id, "Reading the model…", false);
            return;
        }
        if d.url.is_empty() {
            self.set_status(
                &d.id,
                "No device has this model and no download link was given. Download it on one device first.",
                true,
            );
            return;
        }
        {
            let mut st = self.0.orch.state.lock().unwrap();
            if !st.fetching.insert(d.model.clone()) {
                return;
            }
        }
        self.set_status(&d.id, "Downloading the model for the cluster…", false);
        let me = self.clone();
        let d = d.clone();
        tokio::spawn(async move {
            let sha = (!d.sha256.is_empty()).then_some(d.sha256.as_str());
            let r = me.0.store.fetch(&d.model, Some(&d.url), sha, None).await;
            me.0.orch.state.lock().unwrap().fetching.remove(&d.model);
            if let Err(e) = r {
                me.set_status(&d.id, format!("Download failed: {e}"), true);
            }
        });
    }

    fn model_present_without_header(&self, file: &str) -> bool {
        self.0
            .mesh
            .with_registry(|reg, now| !reg.holders(file, now).is_empty())
    }

    /// Stops instances that failed or lost a device, and shares nobody uses any more.
    async fn heal(&self, d: &Deployment) {
        let prefix = format!("{}-", d.id);
        let launching = self.0.launching.lock().unwrap().clone();

        struct Broken {
            master: String,
            instance: String,
            others: Vec<String>,
            why: String,
        }
        let (broken, orphan_shares, live_ids): (
            Vec<Broken>,
            Vec<(String, String)>,
            HashSet<String>,
        ) = self.0.mesh.with_registry(|reg, now| {
            let mut broken = Vec::new();
            let mut live_ids = HashSet::new();
            for e in reg.live(now) {
                let Some(i) = &e.record.instance else {
                    continue;
                };
                if i.deployment != d.id {
                    continue;
                }
                live_ids.insert(i.id.clone());
                let others: Vec<String> = i
                    .stages
                    .iter()
                    .filter(|s| !s.is_master)
                    .map(|s| s.node_id.clone())
                    .collect();
                let lost = others.iter().find(|n| !reg.is_alive(n, now));
                if i.state == InstanceState::Failed {
                    broken.push(Broken {
                        master: e.record.node_id.clone(),
                        instance: i.id.clone(),
                        others,
                        why: format!("failed: {}", i.error.lines().next().unwrap_or("")),
                    });
                } else if let Some(n) = lost {
                    let name = reg
                        .get(n)
                        .map(|x| x.record.name.clone())
                        .unwrap_or_else(|| "a device".into());
                    broken.push(Broken {
                        master: e.record.node_id.clone(),
                        instance: i.id.clone(),
                        others,
                        why: format!("{name} went away"),
                    });
                }
            }
            let orphans: Vec<(String, String)> = reg
                .live(now)
                .into_iter()
                .filter_map(|e| {
                    let s = e.record.share.as_ref()?;
                    s.instance
                        .starts_with(&prefix)
                        .then(|| (e.record.node_id.clone(), s.instance.clone()))
                })
                .collect();
            (broken, orphans, live_ids)
        });

        for b in broken {
            self.set_status(&d.id, format!("Restarting: {}", b.why), false);
            let _ = self
                .0
                .mesh
                .call(
                    &b.master,
                    Command::StopInstance {
                        instance: b.instance.clone(),
                    },
                    QUICK,
                )
                .await;
            for n in b.others {
                let _ = self
                    .0
                    .mesh
                    .call(
                        &n,
                        Command::StopShare {
                            instance: b.instance.clone(),
                        },
                        QUICK,
                    )
                    .await;
            }
        }

        // A share whose instance no longer exists (and is not just starting) is dead weight.
        let mut now_orphans = HashSet::new();
        for (node, inst) in orphan_shares {
            if live_ids.contains(&inst) || launching.contains(&inst) {
                continue;
            }
            now_orphans.insert(format!("{node}|{inst}"));
        }
        let confirmed: Vec<String> = {
            let mut st = self.0.orch.state.lock().unwrap();
            let c = now_orphans
                .iter()
                .filter(|k| st.orphans.contains(*k))
                .cloned()
                .collect();
            st.orphans = now_orphans;
            c
        };
        for key in confirmed {
            if let Some((node, inst)) = key.split_once('|') {
                let _ = self
                    .0
                    .mesh
                    .call(
                        node,
                        Command::StopShare {
                            instance: inst.to_string(),
                        },
                        QUICK,
                    )
                    .await;
            }
        }
    }

    // ───────────── starting one instance ─────────────

    async fn launch(&self, d: &Deployment, want: &Want, p: Planned, inst: String) {
        let reserved: Vec<String> = p.stages.iter().map(|s| s.node_id.clone()).collect();
        let result = self.launch_steps(d, want, &p, &inst).await;
        match &result {
            Ok(()) => {
                let tok = p.est_tok_s;
                self.set_status(
                    &d.id,
                    format!(
                        "Running ({} on {} device(s), about {:.0} tokens/s alone)",
                        p.kind,
                        p.stages.len(),
                        tok
                    ),
                    false,
                );
            }
            Err(e) => {
                self.set_status(&d.id, format!("Could not start: {e}"), true);
                // Undo whatever got started.
                let _ = self
                    .0
                    .mesh
                    .call(
                        &p.master,
                        Command::StopInstance {
                            instance: inst.clone(),
                        },
                        QUICK,
                    )
                    .await;
                for s in p.stages.iter().filter(|s| !s.is_master) {
                    let _ = self
                        .0
                        .mesh
                        .call(
                            &s.node_id,
                            Command::StopShare {
                                instance: inst.clone(),
                            },
                            QUICK,
                        )
                        .await;
                }
            }
        }
        {
            let mut l = self.0.launching.lock().unwrap();
            l.remove(&inst);
            for n in &reserved {
                l.remove(n);
            }
        }
        // If the deployment was removed while this was starting, don't leave it running.
        if result.is_ok() && !self.deployments().iter().any(|x| x.id == d.id) {
            self.stop_everything_of(&d.id).await;
        }
        self.0.orch.kick.notify_one();
    }

    async fn launch_steps(
        &self,
        d: &Deployment,
        want: &Want,
        p: &Planned,
        inst: &str,
    ) -> Result<(), String> {
        let mesh = &self.0.mesh;
        let names: Vec<String> = p.stages.iter().map(|s| s.name.clone()).collect();
        self.set_status(&d.id, format!("Starting on {}…", names.join(", ")), false);

        // 1. The device that runs the server needs the model file.
        if p.needs_fetch {
            self.set_status(&d.id, format!("Copying the model to {}…", names[0]), false);
            let sha = (!d.sha256.is_empty()).then(|| d.sha256.clone());
            mesh.call(
                &p.master,
                Command::Fetch {
                    file: d.model.clone(),
                    url: (!d.url.is_empty()).then(|| d.url.clone()),
                    sha256: sha,
                    size: None,
                },
                FETCH_WAIT,
            )
            .await
            .map_err(|e| format!("copying the model: {e}"))?;
        }

        // 2. Workers start holding their part.
        for s in p.stages.iter().filter(|s| !s.is_master) {
            self.set_status(&d.id, format!("Starting the worker on {}…", s.name), false);
            mesh.call(
                &s.node_id,
                Command::StartShare(Share {
                    instance: inst.to_string(),
                    model: d.model.clone(),
                    master: p.master.clone(),
                    layer_from: s.layer_from,
                    layers: s.layers,
                    mem_mb: s.mem_mb,
                    running: false,
                }),
                QUICK,
            )
            .await
            .map_err(|e| format!("{}: {e}", s.name))?;
        }

        // 3. The master loads the model (for a pipeline this sends each worker its layers).
        self.set_status(
            &d.id,
            if p.kind == "pipeline" {
                "Loading the model and sending layers to the other devices…"
            } else {
                "Loading the model…"
            },
            false,
        );
        let threads = mesh_threads(self, &p.master);
        let spec = p.to_spec(inst, &d.id, &d.model, want, threads);
        mesh.call(
            &p.master,
            Command::StartInstance(Box::new(spec)),
            START_WAIT,
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn mesh_threads(node: &Node, id: &str) -> u32 {
    node.0
        .mesh
        .with_registry(|reg, _| reg.get(id).map(|e| e.record.hw.cores).unwrap_or(4))
        .max(1)
}

/// Runs forever: a pass over every owned deployment every few seconds, or sooner when kicked.
pub async fn reconcile_loop(node: Node) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(RECONCILE_EVERY) => {}
            _ = node.0.orch.kick.notified() => {}
        }
        if !node.0.mesh.joined() {
            continue;
        }
        for d in node.deployments() {
            node.reconcile(&d).await;
        }
    }
}

//! Deciding where a model runs.
//!
//! Two shapes of placement:
//!
//! * **Replica**: the whole model on one device (GPU, GPU + CPU, or CPU). No network in the
//!   token loop, so it is the fastest, and every extra replica is extra throughput.
//! * **Pipeline**: the layers split over several devices through llama.cpp's RPC backend, for a
//!   model that fits on no single device. Every token visits every device, so a device is only
//!   included when it pays for its own network hop.
//!
//! The planner is a pure function of numbers, so it is tested without models or machines.

use std::collections::HashMap;

use serde::Serialize;

use crate::proto::{InstanceSpec, NodeRecord, Stage};

/// Memory the KV cache needs per token, per layer, in KB (a typical grouped-query model).
const KV_KB: f64 = 4.0;
/// Scratch memory the engine allocates for computing.
const COMPUTE_MB: f64 = 300.0;
/// Weights are a little bigger than the file once loaded.
const WEIGHT_OVERHEAD: f64 = 1.03;
/// Share of free GPU memory a model may take.
const GPU_USABLE: f64 = 0.92;
/// Share of free RAM a model may take (the rest is for the system and the app).
const RAM_USABLE: f64 = 0.75;
/// Latency assumed between two devices nobody has measured.
const DEFAULT_RTT_MS: f64 = 2.0;
/// A pipeline hop costs a few round trips per token (compute request, then tensor copies).
const RTTS_PER_HOP: f64 = 1.5;
/// Per-token cost that does not depend on the model (sampling, scheduling), in seconds.
const TOKEN_OVERHEAD_S: f64 = 0.002;
/// How many devices are searched exhaustively when forming a pipeline.
const MAX_SEARCH: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Auto,
    Replicas,
    Pipeline,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s {
            "replicas" => Mode::Replicas,
            "pipeline" => Mode::Pipeline,
            _ => Mode::Auto,
        }
    }
}

/// One device as the planner sees it.
#[derive(Clone, Debug, Default)]
pub struct NodeCaps {
    pub id: String,
    pub name: String,
    /// Alive, lending itself, not being drained and not already busy.
    pub available: bool,
    pub engine_ok: bool,
    pub has_rpc: bool,
    /// RAM a model may use, in MB.
    pub ram_mb: f64,
    /// VRAM a model may use on each GPU, in MB.
    pub gpus_mb: Vec<f64>,
    pub gpu_device0: String,
    /// Effective speed on the GPU and the CPU, in GB of weights per second.
    pub gpu_speed: f64,
    pub cpu_speed: f64,
    pub has_model: bool,
    /// Round-trip times to other devices, in ms.
    pub rtt: HashMap<String, f64>,
}

impl NodeCaps {
    fn vram(&self) -> f64 {
        self.gpus_mb.iter().sum()
    }
}

#[derive(Clone, Debug)]
pub struct ModelSpec {
    pub file: String,
    pub size_mb: f64,
    pub n_layer: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Want {
    pub mode: Mode,
    pub ctx: u32,
    pub parallel: u32,
    /// 0 means as many as fit.
    pub max_instances: u32,
}

/// One planned model server.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Planned {
    pub kind: String,
    pub master: String,
    pub stages: Vec<Stage>,
    pub ngl: Option<u32>,
    pub tensor_split: Vec<f64>,
    pub rpc_nodes: Vec<String>,
    pub pin_output: String,
    /// The master does not have the model file yet.
    pub needs_fetch: bool,
    pub est_tok_s: f64,
    pub note: String,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanResult {
    pub instances: Vec<Planned>,
    pub notes: Vec<String>,
    pub error: Option<String>,
    /// Memory the model needs at this context size, in MB.
    pub need_mb: f64,
}

// ───────────────────────────── sizes and speeds ─────────────────────────────

pub fn kv_mb(m: &ModelSpec, ctx: u32) -> f64 {
    m.n_layer as f64 * ctx as f64 * KV_KB / 1024.0
}

/// Memory the whole model needs on one device.
pub fn need_mb(m: &ModelSpec, ctx: u32) -> f64 {
    m.size_mb * WEIGHT_OVERHEAD + kv_mb(m, ctx) + COMPUTE_MB
}

fn per_layer_mb(m: &ModelSpec, ctx: u32) -> f64 {
    (m.size_mb * WEIGHT_OVERHEAD + kv_mb(m, ctx)) / m.n_layer.max(1) as f64
}

/// Memory the embedding and output matrices take on the master.
fn head_mb(m: &ModelSpec) -> f64 {
    (m.size_mb * 0.04).max(120.0) + COMPUTE_MB
}

/// A rough GPU memory bandwidth guess (GB/s) when nothing has been measured.
pub fn guess_gpu_gbps(name: &str, vram_mb: u64) -> f64 {
    let n = name.to_ascii_uppercase();
    let table: &[(&str, f64)] = &[
        ("H100", 2500.0),
        ("A100", 1500.0),
        ("5090", 1500.0),
        ("4090", 1000.0),
        ("3090", 900.0),
        ("5080", 950.0),
        ("4080", 700.0),
        ("5070 TI", 850.0),
        ("5070", 650.0),
        ("4070", 500.0),
        ("5060", 430.0),
        ("4060", 280.0),
        ("3060", 330.0),
        ("7900", 900.0),
        ("MI", 1000.0),
        ("ARC", 300.0),
    ];
    for (k, v) in table {
        if n.contains(k) {
            return *v;
        }
    }
    match vram_mb {
        24_000.. => 700.0,
        12_000.. => 400.0,
        8_000.. => 250.0,
        _ => 150.0,
    }
}

/// Builds the planner's view of a node from its published record.
pub fn caps_from_record(
    r: &NodeRecord,
    available: bool,
    model_file: &str,
    master_rtt: &HashMap<String, f64>,
) -> NodeCaps {
    let gpus_mb: Vec<f64> =
        r.hw.gpus
            .iter()
            .map(|g| g.vram_free_mb as f64 * GPU_USABLE)
            .collect();
    let vram_total: u64 = r.hw.gpus.iter().map(|g| g.vram_total_mb).sum();
    let measured = r
        .bench
        .as_ref()
        .filter(|b| b.tok_s > 0.0 && b.model_gb > 0.0)
        .map(|b| b.tok_s * b.model_gb);
    let has_gpu = !r.hw.gpus.is_empty();
    let gpu_speed = if has_gpu {
        measured.unwrap_or_else(|| {
            guess_gpu_gbps(
                r.hw.gpus.first().map(|g| g.name.as_str()).unwrap_or(""),
                vram_total,
            ) * 0.7
        })
    } else {
        0.0
    };
    let cpu_speed = if has_gpu {
        // The CPU part of a hybrid run: bandwidth based, not the (GPU) benchmark.
        (r.hw.mem_bw_gbps * 0.55).max(4.0)
    } else {
        measured.unwrap_or((r.hw.mem_bw_gbps * 0.55).max(4.0))
    };
    let mut rtt = master_rtt.clone();
    for l in &r.links {
        rtt.entry(l.node_id.clone()).or_insert(l.rtt_ms);
    }
    NodeCaps {
        id: r.node_id.clone(),
        name: r.name.clone(),
        available,
        engine_ok: r.engine.installed,
        has_rpc: r.engine.has_rpc,
        ram_mb: r.hw.ram_free_mb as f64 * RAM_USABLE,
        gpus_mb,
        gpu_device0: r
            .hw
            .gpus
            .first()
            .map(|g| g.device.clone())
            .unwrap_or_default(),
        gpu_speed,
        cpu_speed,
        has_model: r.models.iter().any(|m| m.file == model_file),
        rtt,
    }
}

// ───────────────────────────── replicas ─────────────────────────────

/// Where the whole model would run on one device.
#[derive(Clone, Debug)]
struct ReplicaFit {
    device: &'static str,
    ngl: u32,
    gpu_layers: u32,
    tok_s: f64,
    mem_mb: f64,
}

fn secs_for(gb: f64, speed: f64) -> f64 {
    if speed <= 0.0 {
        f64::INFINITY
    } else {
        gb / speed
    }
}

fn replica_fit(n: &NodeCaps, m: &ModelSpec, ctx: u32) -> Option<ReplicaFit> {
    let need = need_mb(m, ctx);
    let gb = m.size_mb / 1024.0;
    let vram = n.vram();
    if vram >= need && n.gpu_speed > 0.0 {
        return Some(ReplicaFit {
            device: "gpu",
            ngl: m.n_layer + 1,
            gpu_layers: m.n_layer,
            tok_s: 1.0 / (secs_for(gb, n.gpu_speed) + TOKEN_OVERHEAD_S),
            mem_mb: need,
        });
    }
    if vram + n.ram_mb < need {
        return None;
    }
    if vram > head_mb(m) && n.gpu_speed > 0.0 {
        let pl = per_layer_mb(m, ctx);
        let gpu_layers = (((vram - head_mb(m)) / pl).floor() as u32).min(m.n_layer);
        if gpu_layers > 0 {
            let cpu_layers = m.n_layer - gpu_layers;
            let cpu_need = pl * cpu_layers as f64 + head_mb(m);
            if cpu_need <= n.ram_mb {
                let t = secs_for(gb * gpu_layers as f64 / m.n_layer as f64, n.gpu_speed)
                    + secs_for(gb * cpu_layers as f64 / m.n_layer as f64, n.cpu_speed);
                return Some(ReplicaFit {
                    device: "hybrid",
                    ngl: gpu_layers,
                    gpu_layers,
                    tok_s: 1.0 / (t + TOKEN_OVERHEAD_S),
                    mem_mb: need,
                });
            }
        }
    }
    if n.ram_mb >= need {
        return Some(ReplicaFit {
            device: "cpu",
            ngl: 0,
            gpu_layers: 0,
            tok_s: 1.0 / (secs_for(gb, n.cpu_speed) + TOKEN_OVERHEAD_S),
            mem_mb: need,
        });
    }
    None
}

fn replica_plan(n: &NodeCaps, m: &ModelSpec, want: &Want) -> Option<Planned> {
    let fit = replica_fit(n, m, want.ctx)?;
    Some(Planned {
        kind: "replica".into(),
        master: n.id.clone(),
        stages: vec![Stage {
            node_id: n.id.clone(),
            name: n.name.clone(),
            layer_from: 0,
            layers: m.n_layer,
            mem_mb: fit.mem_mb as u64,
            device: fit.device.into(),
            is_master: true,
        }],
        ngl: Some(fit.ngl),
        tensor_split: if n.gpus_mb.len() > 1 && fit.gpu_layers > 0 {
            normalize(&n.gpus_mb)
        } else {
            vec![]
        },
        rpc_nodes: vec![],
        pin_output: String::new(),
        needs_fetch: !n.has_model,
        est_tok_s: fit.tok_s,
        note: match fit.device {
            "gpu" => "Whole model on the GPU".into(),
            "hybrid" => format!(
                "{} of {} layers on the GPU, the rest on the CPU",
                fit.gpu_layers, m.n_layer
            ),
            _ => "Whole model on the CPU".into(),
        },
    })
}

fn normalize(v: &[f64]) -> Vec<f64> {
    let s: f64 = v.iter().sum();
    if s <= 0.0 {
        return vec![0.0; v.len()];
    }
    v.iter()
        .map(|x| (x / s * 1000.0).round() / 1000.0)
        .collect()
}

// ───────────────────────────── pipelines ─────────────────────────────

/// One memory pool that can hold layers, with a speed.
#[derive(Clone, Debug)]
struct Pool {
    node: usize,
    cap_layers: u32,
    speed: f64,
    gpu: bool,
}

/// Puts layers on the fastest pools first, as many as each can hold. Fewer devices and faster
/// devices mean a faster token, so this is also the cheapest way to use the network.
fn fill(total: u32, pools: &[Pool]) -> Option<Vec<u32>> {
    if pools.iter().map(|p| p.cap_layers as u64).sum::<u64>() < total as u64 {
        return None;
    }
    let mut order: Vec<usize> = (0..pools.len()).collect();
    order.sort_by(|&a, &b| {
        pools[b]
            .speed
            .partial_cmp(&pools[a].speed)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut left = total;
    let mut out = vec![0u32; pools.len()];
    for i in order {
        let take = pools[i].cap_layers.min(left);
        out[i] = take;
        left -= take;
        if left == 0 {
            break;
        }
    }
    Some(out)
}

/// Spreads layers over the pools in proportion to their speed, never past what a pool can hold.
/// Used when the user asks for a pipeline: the layers really are shared out, even if one device
/// could hold them all.
fn spread(total: u32, pools: &[Pool]) -> Option<Vec<u32>> {
    if pools.iter().map(|p| p.cap_layers as u64).sum::<u64>() < total as u64 {
        return None;
    }
    let mut out = vec![0u32; pools.len()];
    let mut left = total;
    let mut active: Vec<usize> = (0..pools.len()).collect();
    while left > 0 && !active.is_empty() {
        let wsum: f64 = active.iter().map(|&i| pools[i].speed.max(0.001)).sum();
        let mut gave = 0;
        for &i in &active {
            let share = (left as f64 * pools[i].speed.max(0.001) / wsum).floor() as u32;
            let g = share.min(pools[i].cap_layers - out[i]);
            out[i] += g;
            gave += g;
        }
        left -= gave;
        active.retain(|&i| out[i] < pools[i].cap_layers);
        if gave == 0 {
            // Rounding left a few layers: hand them out one at a time, fastest first.
            let mut order = active.clone();
            order.sort_by(|&a, &b| {
                pools[b]
                    .speed
                    .partial_cmp(&pools[a].speed)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            for i in order {
                if left == 0 {
                    break;
                }
                out[i] += 1;
                left -= 1;
            }
            active.retain(|&i| out[i] < pools[i].cap_layers);
        }
    }
    (left == 0).then_some(out)
}

fn rtt_between(nodes: &[NodeCaps], a: usize, b: usize) -> f64 {
    nodes[a]
        .rtt
        .get(&nodes[b].id)
        .or_else(|| nodes[b].rtt.get(&nodes[a].id))
        .copied()
        .unwrap_or(DEFAULT_RTT_MS)
}

/// The best pipeline over exactly these nodes (indices into `nodes`), or `None` if the model
/// does not fit or the nodes cannot take part.
fn pipeline_over(nodes: &[NodeCaps], idx: &[usize], m: &ModelSpec, want: &Want) -> Option<Planned> {
    if idx.len() < 2 {
        return None;
    }
    let pl = per_layer_mb(m, want.ctx);
    let head = head_mb(m);
    let gb = m.size_mb / 1024.0;
    let layer_gb = gb / m.n_layer as f64;

    // The master: the node that already has the file, then the fastest.
    let master = *idx.iter().max_by(|&&a, &&b| {
        let ka = (
            nodes[a].has_model,
            nodes[a].gpu_speed.max(nodes[a].cpu_speed),
        );
        let kb = (
            nodes[b].has_model,
            nodes[b].gpu_speed.max(nodes[b].cpu_speed),
        );
        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
    })?;
    let mn = &nodes[master];

    let mut pools: Vec<Pool> = Vec::new();
    // Master: the embedding and output matrices go on its GPU if it has one, else its CPU.
    let m_vram = mn.vram();
    let master_gpu = m_vram > head && mn.gpu_speed > 0.0;
    let mut master_cpu_cap = mn.ram_mb;
    let mut master_gpu_cap = m_vram;
    if master_gpu {
        master_gpu_cap -= head;
    } else {
        master_cpu_cap -= head;
    }
    if master_cpu_cap > 0.0 {
        pools.push(Pool {
            node: master,
            cap_layers: (master_cpu_cap / pl).floor() as u32,
            speed: mn.cpu_speed,
            gpu: false,
        });
    }
    if master_gpu && master_gpu_cap > 0.0 {
        pools.push(Pool {
            node: master,
            cap_layers: (master_gpu_cap / pl).floor() as u32,
            speed: mn.gpu_speed,
            gpu: true,
        });
    }
    // Workers: their GPU if they have one (the RPC server serves one backend), else their CPU.
    let mut workers: Vec<usize> = Vec::new();
    for &i in idx.iter().filter(|&&i| i != master) {
        let n = &nodes[i];
        if !n.has_rpc || !n.engine_ok {
            return None;
        }
        let (mem, speed, gpu) = if n.vram() > 0.0 && n.gpu_speed > 0.0 {
            (n.vram(), n.gpu_speed, true)
        } else {
            (n.ram_mb, n.cpu_speed, false)
        };
        pools.push(Pool {
            node: i,
            cap_layers: (mem / pl).floor() as u32,
            speed,
            gpu,
        });
        workers.push(i);
    }
    let base = fill(m.n_layer, &pools)?;
    let counts = if want.mode == Mode::Pipeline {
        // Forced pipeline: every worker takes part, and layers are shared out by speed. The
        // master's slower memory pool is only used if the fastest-first fill needed it.
        let active: Vec<usize> = (0..pools.len())
            .filter(|&i| base[i] > 0 || pools[i].node != master)
            .collect();
        let sub: Vec<Pool> = active.iter().map(|&i| pools[i].clone()).collect();
        match spread(m.n_layer, &sub) {
            Some(c) => {
                let mut out = vec![0u32; pools.len()];
                for (k, &i) in active.iter().enumerate() {
                    out[i] = c[k];
                }
                out
            }
            None => base,
        }
    } else {
        base
    };

    // Anyone who ended up with no layers is not part of this pipeline (so not a cost either).
    let used_workers: Vec<usize> = workers
        .iter()
        .copied()
        .filter(|&w| {
            pools
                .iter()
                .zip(&counts)
                .any(|(p, &c)| p.node == w && c > 0)
        })
        .collect();
    if used_workers.is_empty() {
        return None; // everything fits on the master: that is a replica, not a pipeline
    }

    let mut t = TOKEN_OVERHEAD_S;
    for (p, &c) in pools.iter().zip(&counts) {
        t += secs_for(layer_gb * c as f64, p.speed);
    }
    for &w in &used_workers {
        t += rtt_between(nodes, master, w) * RTTS_PER_HOP / 1000.0;
    }

    // Positions: the master's CPU layers first, then its GPU layers, then each worker in turn.
    let m_cpu: u32 = pools
        .iter()
        .zip(&counts)
        .filter(|(p, _)| p.node == master && !p.gpu)
        .map(|(_, &c)| c)
        .sum();
    let m_gpu: u32 = pools
        .iter()
        .zip(&counts)
        .filter(|(p, _)| p.node == master && p.gpu)
        .map(|(_, &c)| c)
        .sum();
    let mut stages = vec![Stage {
        node_id: mn.id.clone(),
        name: mn.name.clone(),
        layer_from: 0,
        layers: m_cpu + m_gpu,
        mem_mb: ((m_cpu + m_gpu) as f64 * pl + head) as u64,
        device: match (m_cpu > 0, m_gpu > 0) {
            (true, true) => "hybrid",
            (false, true) => "gpu",
            _ => "cpu",
        }
        .into(),
        is_master: true,
    }];
    let mut next = m_cpu + m_gpu;
    // Workers, fastest first (their order in the chain does not change the cost).
    let mut ordered = used_workers.clone();
    ordered.sort_by(|&a, &b| {
        let sa = pools
            .iter()
            .find(|p| p.node == a)
            .map(|p| p.speed)
            .unwrap_or(0.0);
        let sb = pools
            .iter()
            .find(|p| p.node == b)
            .map(|p| p.speed)
            .unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut ts: Vec<f64> = Vec::new();
    // llama.cpp lists the master's own GPUs first, then the remote devices.
    let n_master_gpus = mn.gpus_mb.len();
    for g in 0..n_master_gpus {
        let share = if m_gpu > 0 && m_vram > 0.0 {
            m_gpu as f64 * mn.gpus_mb[g] / m_vram
        } else {
            0.0
        };
        ts.push(share);
    }
    for &w in &ordered {
        let c = pools
            .iter()
            .zip(&counts)
            .find(|(p, _)| p.node == w)
            .map(|(p, &c)| (p, c))
            .unwrap();
        stages.push(Stage {
            node_id: nodes[w].id.clone(),
            name: nodes[w].name.clone(),
            layer_from: next,
            layers: c.1,
            mem_mb: (c.1 as f64 * pl) as u64,
            device: if c.0.gpu { "gpu" } else { "cpu" }.into(),
            is_master: false,
        });
        next += c.1;
        ts.push(c.1 as f64);
    }
    let offloaded = m_gpu + (next - m_cpu - m_gpu);
    let est = 1.0 / t;
    Some(Planned {
        kind: "pipeline".into(),
        master: mn.id.clone(),
        note: format!(
            "{} layers on {} and {} on {} other device(s)",
            m_cpu + m_gpu,
            mn.name,
            next - (m_cpu + m_gpu),
            ordered.len()
        ),
        stages,
        ngl: Some(offloaded + 1),
        tensor_split: normalize(&ts),
        rpc_nodes: ordered.iter().map(|&w| nodes[w].id.clone()).collect(),
        pin_output: if master_gpu && m_gpu > 0 {
            mn.gpu_device0.clone()
        } else {
            "CPU".into()
        },
        needs_fetch: !mn.has_model,
        est_tok_s: est,
        // A master without GPU layers still needs a GPU-less run: nothing special.
    })
}

/// The best pipeline that can be made from `pool` (indices into `nodes`).
fn best_pipeline(
    nodes: &[NodeCaps],
    pool: &[usize],
    m: &ModelSpec,
    want: &Want,
) -> Option<Planned> {
    let mut cand: Vec<usize> = pool.to_vec();
    if cand.len() > MAX_SEARCH {
        // Too many to try every combination: keep the ones that hold the most.
        cand.sort_by(|&a, &b| {
            let ca =
                nodes[a].vram().max(nodes[a].ram_mb) * nodes[a].gpu_speed.max(nodes[a].cpu_speed);
            let cb =
                nodes[b].vram().max(nodes[b].ram_mb) * nodes[b].gpu_speed.max(nodes[b].cpu_speed);
            cb.partial_cmp(&ca).unwrap_or(std::cmp::Ordering::Equal)
        });
        cand.truncate(MAX_SEARCH);
    }
    let n = cand.len();
    let mut best: Option<Planned> = None;
    for mask in 1u32..(1u32 << n) {
        if mask.count_ones() < 2 {
            continue;
        }
        let subset: Vec<usize> = (0..n)
            .filter(|b| mask & (1 << b) != 0)
            .map(|b| cand[b])
            .collect();
        if let Some(p) = pipeline_over(nodes, &subset, m, want) {
            let better = match &best {
                None => true,
                Some(b) => {
                    p.est_tok_s > b.est_tok_s * 1.02
                        // Equal speed: use fewer devices.
                        || ((p.est_tok_s - b.est_tok_s).abs() <= b.est_tok_s * 0.02
                            && p.stages.len() < b.stages.len())
                }
            };
            if better {
                best = Some(p);
            }
        }
    }
    best
}

// ───────────────────────────── the whole plan ─────────────────────────────

fn gb(mb: f64) -> String {
    format!("{:.1} GB", mb / 1024.0)
}

/// Plans where to run `model` on `nodes`.
pub fn plan(nodes: &[NodeCaps], model: &ModelSpec, want: &Want) -> PlanResult {
    let need = need_mb(model, want.ctx);
    let mut res = PlanResult {
        need_mb: need,
        ..Default::default()
    };
    if model.n_layer == 0 {
        res.error = Some("Could not read this model's layer count".into());
        return res;
    }
    let usable: Vec<usize> = (0..nodes.len())
        .filter(|&i| nodes[i].available && nodes[i].engine_ok)
        .collect();
    if usable.is_empty() {
        res.error = Some(
            "No device is free to take this model. Each one needs the engine installed, to be \
             lending itself to the cluster, and not already running something."
                .into(),
        );
        return res;
    }

    let mut left: Vec<usize> = usable.clone();
    let mut out: Vec<Planned> = Vec::new();

    if want.mode != Mode::Pipeline {
        let mut rest = Vec::new();
        for &i in &left {
            match replica_plan(&nodes[i], model, want) {
                Some(p) => out.push(p),
                None => rest.push(i),
            }
        }
        left = rest;
    }
    if want.mode != Mode::Replicas {
        while left.len() >= 2 {
            let Some(p) = best_pipeline(nodes, &left, model, want) else {
                break;
            };
            let used: Vec<&str> = p.stages.iter().map(|s| s.node_id.as_str()).collect();
            left.retain(|&i| !used.contains(&nodes[i].id.as_str()));
            out.push(p);
        }
    }

    if out.is_empty() {
        let best_one = usable
            .iter()
            .map(|&i| nodes[i].vram().max(0.0) + nodes[i].ram_mb)
            .fold(0.0, f64::max);
        let total: f64 = usable
            .iter()
            .map(|&i| nodes[i].vram() + nodes[i].ram_mb)
            .sum();
        res.error = Some(match want.mode {
            Mode::Replicas => format!(
                "This model needs about {} but the largest free device can give {}. \
                 Use pipeline mode to spread it over several devices.",
                gb(need),
                gb(best_one)
            ),
            _ if total < need => format!(
                "This model needs about {} at this context size, but all free devices can lend \
                 about {} together. Add a device, lower the context size, or pick a smaller model.",
                gb(need),
                gb(total)
            ),
            _ => "The devices have enough memory in total, but a device in the chain cannot take \
                  part (it needs the engine's RPC server). Update the engine on every device."
                .to_string(),
        });
        return res;
    }

    out.sort_by(|a, b| {
        b.est_tok_s
            .partial_cmp(&a.est_tok_s)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if want.max_instances > 0 {
        out.truncate(want.max_instances as usize);
    }
    if !left.is_empty() && want.mode == Mode::Auto && want.max_instances == 0 {
        let names: Vec<&str> = left.iter().map(|&i| nodes[i].name.as_str()).collect();
        res.notes.push(format!(
            "{} left unused: too small to run the model alone, and not enough together to run another copy.",
            names.join(", ")
        ));
    }
    res.instances = out;
    res
}

impl Planned {
    /// Turns a plan into the instruction the master executes.
    pub fn to_spec(
        &self,
        id: &str,
        deployment: &str,
        model: &str,
        want: &Want,
        threads: u32,
    ) -> InstanceSpec {
        InstanceSpec {
            id: id.into(),
            deployment: deployment.into(),
            model: model.into(),
            kind: self.kind.clone(),
            ctx: want.ctx,
            parallel: want.parallel.max(1),
            threads,
            ngl: self.ngl,
            tensor_split: self.tensor_split.clone(),
            rpc_nodes: self.rpc_nodes.clone(),
            pin_output: self.pin_output.clone(),
            stages: self.stages.clone(),
        }
    }
}

#[cfg(test)]
mod tests;

//! What nodes say to each other, and what each node publishes about itself.
//!
//! Every field has a default so an older node can read a newer node's record (and the other way
//! round) without failing. Bump `PROTO_VERSION` only for a change that cannot be made that way.

use serde::{Deserialize, Serialize};

pub const PROTO_VERSION: u32 = 1;

/// One GPU as the engine sees it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct GpuInfo {
    /// llama.cpp's device name, such as `CUDA0` or `Vulkan0`.
    pub device: String,
    pub name: String,
    /// `cuda`, `vulkan`, `rocm`, `metal`, `sycl`.
    pub backend: String,
    pub vram_total_mb: u64,
    pub vram_free_mb: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct HwSpec {
    pub cpu: String,
    pub cores: u32,
    pub ram_total_mb: u64,
    pub ram_free_mb: u64,
    pub gpus: Vec<GpuInfo>,
    /// Measured memory bandwidth, in GB/s. Token speed is bound by it, so the planner uses it.
    pub mem_bw_gbps: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct EngineInfo {
    pub installed: bool,
    pub version: String,
    /// `cpu`, `cuda`, `vulkan`, ...
    pub variant: String,
    /// The engine can run the RPC worker role.
    pub has_rpc: bool,
}

/// A model file this node has on disk.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct HeldModel {
    pub file: String,
    pub size: u64,
    /// Content hash; empty until computed.
    pub sha256: String,
    pub n_layer: u32,
}

/// A model download or copy in progress.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Transfer {
    pub file: String,
    pub done: u64,
    pub total: u64,
    /// Where the bytes come from: "internet", or peer names.
    pub from: Vec<String>,
    /// Size of one piece, and which pieces are done (hex bitmap, lowest piece first), so other
    /// devices can take finished pieces from this one while it is still downloading.
    pub chunk_size: u64,
    pub have: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstanceState {
    #[default]
    Loading,
    Ready,
    Failed,
    Stopping,
}

/// One device's part of a model: a block of consecutive layers.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Stage {
    pub node_id: String,
    pub name: String,
    /// First layer on this device (0-based) and how many it holds.
    pub layer_from: u32,
    pub layers: u32,
    pub mem_mb: u64,
    /// `gpu` or `cpu`.
    pub device: String,
    /// Includes the embedding / output layers kept on the master.
    pub is_master: bool,
}

/// A running (or starting) model server.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct InstanceInfo {
    pub id: String,
    pub deployment: String,
    pub model: String,
    /// `replica` (whole model on one device) or `pipeline` (layers spread over devices).
    pub kind: String,
    pub master: String,
    pub stages: Vec<Stage>,
    pub state: InstanceState,
    pub error: String,
    pub ctx: u32,
    /// Parallel request slots (continuous batching lanes).
    pub slots: u32,
    pub busy: u32,
    pub queued: u32,
    pub requests_total: u64,
    /// Recent generation speed.
    pub tok_s: f64,
    pub started_ms: u64,
}

/// The part of someone else's model this node is holding.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Share {
    pub instance: String,
    pub model: String,
    pub master: String,
    pub layer_from: u32,
    pub layers: u32,
    pub mem_mb: u64,
    pub running: bool,
}

/// What an owner wants running, kept so the cluster can heal itself.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Deployment {
    pub id: String,
    pub model: String,
    /// `auto`, `replicas` or `pipeline`.
    pub mode: String,
    pub ctx: u32,
    pub parallel: u32,
    /// Devices this deployment may use; empty means any device that is lending itself.
    pub nodes: Vec<String>,
    /// Upper bound on running instances; 0 means as many as the devices allow.
    pub max_instances: u32,
    /// Where to download the model if no device in the cluster has it yet.
    pub url: String,
    /// Expected content hash of the model file, if known.
    pub sha256: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Bench {
    /// Generation speed of a reference model on this node alone.
    pub tok_s: f64,
    pub model: String,
    /// Size of that model in GB, so speed can be turned into memory bandwidth.
    pub model_gb: f64,
    pub at_ms: u64,
}

/// Round-trip time from one node to another, as measured by the first.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct LinkInfo {
    pub node_id: String,
    pub rtt_ms: f64,
}

/// Everything a node publishes about itself. Peers keep the newest record per node, so every
/// device can show the same table: who is here, what they hold, and what is running where.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct NodeRecord {
    pub node_id: String,
    pub name: String,
    pub cluster_id: String,
    /// Grows with every change; the highest wins when records are merged.
    pub seq: u64,
    pub version: String,
    pub os: String,
    /// `ip:port` addresses other nodes can dial.
    pub addrs: Vec<String>,
    pub hw: HwSpec,
    pub engine: EngineInfo,
    pub models: Vec<HeldModel>,
    pub transfers: Vec<Transfer>,
    /// Disk used by the RPC tensor cache (weights this node was sent, kept for next time).
    pub tensor_cache_mb: u64,
    /// This node accepts model parts and replicas from the cluster.
    pub lend: bool,
    /// This node is being emptied (no new work is placed on it).
    pub draining: bool,
    pub instance: Option<InstanceInfo>,
    pub share: Option<Share>,
    pub deployments: Vec<Deployment>,
    pub bench: Option<Bench>,
    /// Latency to the other nodes, so anyone can plan a chain that avoids slow hops.
    pub links: Vec<LinkInfo>,
    /// Requests this node's gateway is forwarding right now.
    pub gateway_inflight: u32,
    /// A gateway is accepting API requests on this node.
    pub gateway_port: u16,
}

/// How a model is to be started on a node.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct InstanceSpec {
    pub id: String,
    pub deployment: String,
    pub model: String,
    pub kind: String,
    pub ctx: u32,
    pub parallel: u32,
    pub threads: u32,
    /// Layers to put on accelerators (local GPU and remote devices). `None` lets llama.cpp decide.
    pub ngl: Option<u32>,
    /// Fraction of the offloaded layers for each device, in llama.cpp's device order.
    pub tensor_split: Vec<f64>,
    /// Remote devices, in order: the master opens an encrypted tunnel to each one's RPC server.
    pub rpc_nodes: Vec<String>,
    /// Where the embedding and output matrices live: `CPU` or the master's first GPU (`CUDA0`).
    /// Keeping them on the master stops a vocabulary-sized result crossing the network per token.
    pub pin_output: String,
    pub stages: Vec<Stage>,
}

/// Work one node asks another to do.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Start the RPC server and hold part of a model for `master`.
    StartShare(Share),
    StopShare {
        instance: String,
    },
    StartInstance(Box<InstanceSpec>),
    StopInstance {
        instance: String,
    },
    /// Get a model file: from peers that have it, otherwise from `url`.
    Fetch {
        file: String,
        url: Option<String>,
        sha256: Option<String>,
        size: Option<u64>,
    },
    CancelFetch {
        file: String,
    },
    DeleteModel {
        file: String,
    },
    Bench,
    SetDraining {
        on: bool,
    },
    SetLend {
        on: bool,
    },
    /// Ask a node what is in its tensor cache or to drop it.
    ClearTensorCache,
}

/// A message on a control link.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Msg {
    Hello {
        proto: u32,
        record: Box<NodeRecord>,
    },
    Gossip {
        records: Vec<NodeRecord>,
    },
    Ping {
        t: u64,
    },
    Pong {
        t: u64,
    },
    Req {
        id: u64,
        cmd: Command,
    },
    Res {
        id: u64,
        ok: bool,
        #[serde(default)]
        value: serde_json::Value,
        #[serde(default)]
        error: String,
    },
    Bye,
}

/// The first message on every link, saying what the link is for.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "open", rename_all = "snake_case")]
pub enum Open {
    Control,
    /// A byte pipe to a service on the other node (`rpc`, `api`, `files`).
    Tunnel {
        service: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct OpenReply {
    pub ok: bool,
    #[serde(default)]
    pub error: String,
}

/// What a joining device sends first while pairing.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PairHello {
    pub name: String,
}

/// The host's answer to a pairing request.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PairAnswer {
    pub allowed: bool,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub cluster_name: String,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_survive_a_round_trip() {
        let mut r = NodeRecord {
            node_id: "ab".into(),
            name: "Desk".into(),
            seq: 7,
            ..Default::default()
        };
        r.hw.gpus.push(GpuInfo {
            device: "CUDA0".into(),
            name: "RTX".into(),
            backend: "cuda".into(),
            vram_total_mb: 16000,
            vram_free_mb: 15000,
        });
        r.instance = Some(InstanceInfo {
            id: "i1".into(),
            state: InstanceState::Ready,
            ..Default::default()
        });
        let text = serde_json::to_string(&Msg::Hello {
            proto: PROTO_VERSION,
            record: Box::new(r.clone()),
        })
        .unwrap();
        match serde_json::from_str::<Msg>(&text).unwrap() {
            Msg::Hello { record, .. } => assert_eq!(*record, r),
            _ => panic!("wrong message"),
        }
    }

    #[test]
    fn an_older_record_without_new_fields_still_parses() {
        let r: NodeRecord = serde_json::from_str(r#"{"nodeId":"x","name":"Old"}"#).unwrap();
        assert_eq!(r.node_id, "x");
        assert!(r.models.is_empty());
        assert!(r.instance.is_none());
    }

    #[test]
    fn commands_have_stable_wire_names() {
        let c = Command::StopInstance {
            instance: "i".into(),
        };
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            r#"{"cmd":"stop_instance","instance":"i"}"#
        );
        let m = Msg::Req { id: 1, cmd: c };
        let v: serde_json::Value = serde_json::to_value(&m).unwrap();
        assert_eq!(v["type"], "req");
    }
}

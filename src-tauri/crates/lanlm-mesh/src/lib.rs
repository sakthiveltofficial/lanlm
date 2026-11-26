//! LanLM mesh: secure device discovery, shared cluster state, scheduling and a
//! load-balancing gateway for llama.cpp.
pub mod admin;
pub mod control;
pub mod crypto;
pub mod discovery;
pub mod dl;
pub mod engine;
pub mod gateway;
pub mod gguf;
pub mod hw;
mod link;
pub mod mesh;
pub mod node;
pub mod orchestrator;
pub mod planner;
pub mod proto;
pub mod registry;
pub mod store;

pub use mesh::{Event, Mesh, MeshConfig, PairStatus, Snapshot};

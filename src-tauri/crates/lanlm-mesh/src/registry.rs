//! The cluster table: the newest record from every node, and who is still alive.
//!
//! Pure data and rules, no sockets, so the liveness behaviour can be tested exactly.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::proto::{InstanceInfo, InstanceState, NodeRecord};

/// A node that has said nothing for this long is gone.
pub const LIVE_MS: u64 = 8_000;
/// A node that is loading a model or copying a file is busy, not gone: give it longer.
pub const LIVE_BUSY_MS: u64 = 45_000;
/// After a direct link drops, how long a node gets to reconnect before it counts as gone.
pub const GRACE_MS: u64 = 3_000;
/// Nodes gone this long are forgotten.
pub const FORGET_MS: u64 = 10 * 60_000;

#[derive(Clone, Debug)]
pub struct Entry {
    pub record: NodeRecord,
    pub last_seen: Instant,
    pub rtt_ms: Option<f64>,
    /// We hold a direct link to this node right now.
    pub linked: bool,
}

/// What the UI and the CLI show for one node.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    pub record: NodeRecord,
    pub alive: bool,
    pub linked: bool,
    pub is_me: bool,
    pub rtt_ms: Option<f64>,
    pub age_ms: u64,
}

pub struct Registry {
    me: String,
    map: HashMap<String, Entry>,
}

fn limit_for(r: &NodeRecord) -> u64 {
    let loading = r
        .instance
        .as_ref()
        .is_some_and(|i| i.state == InstanceState::Loading);
    if loading || !r.transfers.is_empty() {
        LIVE_BUSY_MS
    } else {
        LIVE_MS
    }
}

impl Registry {
    pub fn new(me: NodeRecord, now: Instant) -> Registry {
        let id = me.node_id.clone();
        let mut map = HashMap::new();
        map.insert(
            id.clone(),
            Entry {
                record: me,
                last_seen: now,
                rtt_ms: Some(0.0),
                linked: true,
            },
        );
        Registry { me: id, map }
    }

    pub fn me(&self) -> &str {
        &self.me
    }

    pub fn my_record(&self) -> &NodeRecord {
        &self.map[&self.me].record
    }

    /// Replaces our own record (the caller has already bumped its `seq`).
    pub fn set_self(&mut self, record: NodeRecord, now: Instant) {
        if let Some(e) = self.map.get_mut(&self.me) {
            e.record = record;
            e.last_seen = now;
        }
    }

    /// Takes a record heard from a peer. Returns true when it was news.
    /// Anyone can claim anything, so a record about *us* is never accepted, and a record from a
    /// different cluster is dropped.
    pub fn merge(&mut self, record: NodeRecord, now: Instant) -> bool {
        if record.node_id == self.me || record.node_id.is_empty() {
            return false;
        }
        if record.cluster_id != self.my_record().cluster_id {
            return false;
        }
        match self.map.get_mut(&record.node_id) {
            Some(e) => {
                if record.seq < e.record.seq {
                    return false;
                }
                // Only a *newer* record counts as a sign of life. Hearing the same old record again
                // (a peer echoing its table) must not keep a dead node on the list.
                let news = record.seq > e.record.seq;
                if news {
                    e.record = record;
                    e.last_seen = now;
                }
                news
            }
            None => {
                self.map.insert(
                    record.node_id.clone(),
                    Entry {
                        record,
                        last_seen: now,
                        rtt_ms: None,
                        linked: false,
                    },
                );
                true
            }
        }
    }

    /// We heard from this node directly (a ping, a pong, any message).
    pub fn touch(&mut self, id: &str, now: Instant) {
        if let Some(e) = self.map.get_mut(id) {
            e.last_seen = now;
        }
    }

    pub fn set_rtt(&mut self, id: &str, ms: f64) {
        if let Some(e) = self.map.get_mut(id) {
            e.rtt_ms = Some(match e.rtt_ms {
                Some(old) if old > 0.0 => old * 0.7 + ms * 0.3,
                _ => ms,
            });
        }
    }

    /// A direct link came up or went down. When it goes down the node keeps only a short grace
    /// period to reconnect, so a crashed device disappears in seconds, not tens of seconds.
    pub fn set_linked(&mut self, id: &str, linked: bool, now: Instant) {
        if id == self.me {
            return;
        }
        if let Some(e) = self.map.get_mut(id) {
            e.linked = linked;
            if linked {
                e.last_seen = now;
            } else {
                let limit = Duration::from_millis(limit_for(&e.record));
                let keep = Duration::from_millis(GRACE_MS);
                if let Some(floor) = now.checked_sub(limit.saturating_sub(keep)) {
                    if e.last_seen > floor {
                        e.last_seen = floor;
                    }
                }
            }
        }
    }

    pub fn is_alive(&self, id: &str, now: Instant) -> bool {
        self.map
            .get(id)
            .is_some_and(|e| entry_alive(e, &self.me, now))
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.map.get(id)
    }

    /// Live nodes, ours included, in a stable order (by name, then id).
    pub fn live(&self, now: Instant) -> Vec<&Entry> {
        let mut v: Vec<&Entry> = self
            .map
            .values()
            .filter(|e| entry_alive(e, &self.me, now))
            .collect();
        v.sort_by(|a, b| {
            (&a.record.name, &a.record.node_id).cmp(&(&b.record.name, &b.record.node_id))
        });
        v
    }

    pub fn views(&self, now: Instant) -> Vec<NodeView> {
        let mut v: Vec<NodeView> = self
            .map
            .values()
            .map(|e| NodeView {
                record: e.record.clone(),
                alive: entry_alive(e, &self.me, now),
                linked: e.linked,
                is_me: e.record.node_id == self.me,
                rtt_ms: e.rtt_ms,
                age_ms: now.saturating_duration_since(e.last_seen).as_millis() as u64,
            })
            .collect();
        v.sort_by(|a, b| {
            b.is_me
                .cmp(&a.is_me)
                .then_with(|| a.record.name.cmp(&b.record.name))
                .then_with(|| a.record.node_id.cmp(&b.record.node_id))
        });
        v
    }

    /// Every record we know (for gossip to a peer that may have missed some).
    pub fn all_records(&self) -> Vec<NodeRecord> {
        self.map.values().map(|e| e.record.clone()).collect()
    }

    /// Drops nodes that have been gone for a long time.
    pub fn forget_stale(&mut self, now: Instant) {
        let me = self.me.clone();
        self.map.retain(|id, e| {
            *id == me
                || now.saturating_duration_since(e.last_seen) < Duration::from_millis(FORGET_MS)
        });
    }

    /// Live nodes that hold a complete copy of `file`.
    pub fn holders(&self, file: &str, now: Instant) -> Vec<&Entry> {
        self.live(now)
            .into_iter()
            .filter(|e| e.record.models.iter().any(|m| m.file == file))
            .collect()
    }

    /// Every instance on a live node, with the node that runs it.
    pub fn instances(&self, now: Instant) -> Vec<(&NodeRecord, &InstanceInfo)> {
        self.live(now)
            .into_iter()
            .filter_map(|e| e.record.instance.as_ref().map(|i| (&e.record, i)))
            .collect()
    }
}

fn entry_alive(e: &Entry, me: &str, now: Instant) -> bool {
    if e.record.node_id == me {
        return true;
    }
    now.saturating_duration_since(e.last_seen) < Duration::from_millis(limit_for(&e.record))
}

#[cfg(test)]
mod tests;

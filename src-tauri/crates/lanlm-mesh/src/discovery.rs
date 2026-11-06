//! Finding other devices on the local network (mDNS / Bonjour).
//!
//! * Members of a cluster announce `_lanlm._tcp` with a public label of their cluster (never the
//!   key). Devices that see an announcement for their own cluster dial it; the encrypted
//!   handshake then proves the other side really holds the key, so a stranger who copies the
//!   label gets nowhere.
//! * While a member has pairing open, it also announces `_lanlm-invite._tcp`. A device that is
//!   not in a cluster yet listens for those, so "devices nearby" can be listed and joined with
//!   one click and an Allow on the other screen, with nothing to type.
//!
//! Where multicast is blocked (some Wi-Fi, VPNs), add a device by address instead.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Serialize;

use crate::mesh::Mesh;

const CLUSTER_SERVICE: &str = "_lanlm._tcp.local.";
const INVITE_SERVICE: &str = "_lanlm-invite._tcp.local.";
/// A nearby device that has not been heard from for this long is no longer listed.
const NEARBY_FOR: Duration = Duration::from_secs(45);

/// A device that is open to new members, found on the local network.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Nearby {
    pub node_id: String,
    pub name: String,
    pub addrs: Vec<String>,
}

#[derive(Default)]
pub(crate) struct NearbyList {
    seen: HashMap<String, (Nearby, Instant)>,
}

impl NearbyList {
    pub fn add(&mut self, n: Nearby) {
        self.seen.insert(n.node_id.clone(), (n, Instant::now()));
    }

    pub fn remove(&mut self, id: &str) {
        self.seen.remove(id);
    }

    pub fn list(&self) -> Vec<Nearby> {
        let mut v: Vec<Nearby> = self
            .seen
            .values()
            .filter(|(_, t)| t.elapsed() < NEARBY_FOR)
            .map(|(n, _)| n.clone())
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn clear(&mut self) {
        self.seen.clear();
    }
}

pub(crate) fn spawn(mesh: &Mesh) {
    tokio::spawn(run(mesh.clone()));
}

/// What should be announced and listened for right now.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Want {
    cluster: String,
    invite: bool,
}

struct Session {
    daemon: ServiceDaemon,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        let _ = self.daemon.shutdown();
    }
}

async fn run(mesh: Mesh) {
    let mut membership = mesh.0.membership.subscribe();
    let mut current: Option<(Want, Session)> = None;
    loop {
        let want = Want {
            cluster: mesh.cluster_id(),
            invite: mesh.pairing_is_open(),
        };
        if current.as_ref().map(|(w, _)| w) != Some(&want) {
            drop(current.take()); // stop the old session first
            mesh.0.nearby.lock().unwrap().clear();
            current = start(&mesh, &want).map(|s| (want, s));
        }
        // Wake on a change of cluster or pairing, or every so often to notice an expired window.
        tokio::select! {
            r = membership.changed() => { if r.is_err() { break; } }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
    }
}

fn start(mesh: &Mesh, want: &Want) -> Option<Session> {
    let daemon = ServiceDaemon::new().ok()?;
    let mut tasks = Vec::new();
    let id = mesh.id();
    let short = id[..12.min(id.len())].to_string();

    let ips: Vec<IpAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .map(|i| i.ip())
        .filter(|ip| matches!(ip, IpAddr::V4(v) if !v.is_link_local()))
        .collect();
    let announce = |service: &str, instance: String, props: HashMap<String, String>| {
        let host = format!("{instance}.local.");
        let info = if ips.is_empty() {
            ServiceInfo::new(service, &instance, &host, "", mesh.port(), props)
                .ok()?
                .enable_addr_auto()
        } else {
            ServiceInfo::new(
                service,
                &instance,
                &host,
                ips.as_slice(),
                mesh.port(),
                props,
            )
            .ok()?
        };
        daemon.register(info).ok()
    };

    if !want.cluster.is_empty() {
        let mut props = HashMap::new();
        props.insert("cid".to_string(), want.cluster.clone());
        props.insert("id".to_string(), id.clone());
        props.insert("name".to_string(), mesh.name());
        announce(CLUSTER_SERVICE, format!("lanlm-{short}"), props)?;
        let rx = daemon.browse(CLUSTER_SERVICE).ok()?;
        let m = mesh.clone();
        let cid = want.cluster.clone();
        tasks.push(tokio::spawn(async move {
            while let Ok(ev) = rx.recv_async().await {
                let ServiceEvent::ServiceResolved(info) = ev else {
                    continue;
                };
                if info.get_property_val_str("cid") != Some(cid.as_str())
                    || info.get_property_val_str("id") == Some(m.id().as_str())
                {
                    continue;
                }
                let port = info.get_port();
                m.add_candidates(
                    info.get_addresses()
                        .iter()
                        .filter(|a| a.is_ipv4())
                        .map(|a| format!("{a}:{port}")),
                );
            }
        }));
    }

    if want.invite {
        let mut props = HashMap::new();
        props.insert("id".to_string(), id.clone());
        props.insert("name".to_string(), mesh.name());
        announce(INVITE_SERVICE, format!("lanlm-invite-{short}"), props)?;
    }

    if want.cluster.is_empty() {
        let rx = daemon.browse(INVITE_SERVICE).ok()?;
        let m = mesh.clone();
        tasks.push(tokio::spawn(async move {
            while let Ok(ev) = rx.recv_async().await {
                match ev {
                    ServiceEvent::ServiceResolved(info) => {
                        let Some(nid) = info.get_property_val_str("id").map(|s| s.to_string())
                        else {
                            continue;
                        };
                        if nid == m.id() {
                            continue;
                        }
                        let port = info.get_port();
                        let addrs: Vec<String> = info
                            .get_addresses()
                            .iter()
                            .filter(|a| a.is_ipv4())
                            .map(|a| format!("{a}:{port}"))
                            .collect();
                        let name = info
                            .get_property_val_str("name")
                            .unwrap_or("A device")
                            .to_string();
                        m.0.nearby.lock().unwrap().add(Nearby {
                            node_id: nid,
                            name,
                            addrs,
                        });
                    }
                    ServiceEvent::ServiceRemoved(_, full) => {
                        // "lanlm-invite-<short>._lanlm-invite._tcp.local."
                        let short = full
                            .split('.')
                            .next()
                            .and_then(|s| s.strip_prefix("lanlm-invite-"))
                            .unwrap_or("")
                            .to_string();
                        let mut list = m.0.nearby.lock().unwrap();
                        let ids: Vec<String> = list
                            .list()
                            .into_iter()
                            .filter(|n| n.node_id.starts_with(&short))
                            .map(|n| n.node_id)
                            .collect();
                        for i in ids {
                            list.remove(&i);
                        }
                    }
                    _ => {}
                }
            }
        }));
    }
    Some(Session { daemon, tasks })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: &str, name: &str) -> Nearby {
        Nearby {
            node_id: id.into(),
            name: name.into(),
            addrs: vec!["10.0.0.2:50500".into()],
        }
    }

    #[test]
    fn nearby_devices_are_listed_by_name_and_can_be_removed() {
        let mut l = NearbyList::default();
        l.add(n("b", "Zed"));
        l.add(n("a", "Ann"));
        l.add(n("a", "Ann again")); // same device announcing again replaces the entry
        let names: Vec<String> = l.list().into_iter().map(|x| x.name).collect();
        assert_eq!(names, ["Ann again", "Zed"]);
        l.remove("a");
        assert_eq!(l.list().len(), 1);
        l.clear();
        assert!(l.list().is_empty());
    }
}

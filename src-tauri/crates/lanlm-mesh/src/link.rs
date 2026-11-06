//! The network side of the mesh: the listener, dialling other nodes, control sessions that keep
//! the shared table in sync, encrypted tunnels for RPC and API traffic, and pairing.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use futures_util::future::select_ok;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Semaphore};

use crate::crypto::{self, SecureStream, MAX_PLAIN};
use crate::mesh::{Event, Mesh, PairRequest, PairStatus, PeerLink, PAIR_WINDOW};
use crate::proto::{now_ms, Msg, Open, OpenReply, PairAnswer, PairHello, PROTO_VERSION};

const HANDSHAKE_WAIT: Duration = Duration::from_secs(10);
const DIAL_WAIT: Duration = Duration::from_millis(2500);
/// A control link that says nothing for this long is dead (pings go out every 2 s).
const SILENCE: Duration = Duration::from_secs(12);
const MAX_PENDING_PAIRS: usize = 8;

// ───────────────────────────── addresses ─────────────────────────────

/// Binds the mesh port, trying the next few ports when it is taken.
pub(crate) async fn bind(first: u16) -> Result<TcpListener, String> {
    let mut last = String::new();
    for p in first..first.saturating_add(20) {
        match TcpListener::bind(("0.0.0.0", p)).await {
            Ok(l) => return Ok(l),
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!("Could not open a port for the cluster: {last}"))
}

/// This machine's addresses on real networks (not loopback or link-local), as `ip:port`.
pub fn local_addrs(port: u16) -> Vec<String> {
    let mut v: Vec<String> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(ip) if !ip.is_link_local() => Some(format!("{ip}:{port}")),
            _ => None,
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

async fn connect_any(addrs: &[String]) -> Result<TcpStream, String> {
    if addrs.is_empty() {
        return Err("no address to connect to".into());
    }
    let attempts = addrs.iter().map(|a| {
        let a = a.clone();
        Box::pin(async move {
            let s = tokio::time::timeout(DIAL_WAIT, TcpStream::connect(&a))
                .await
                .map_err(|_| format!("{a}: timed out"))?
                .map_err(|e| format!("{a}: {e}"))?;
            let _ = s.set_nodelay(true);
            Ok::<TcpStream, String>(s)
        })
    });
    select_ok(attempts).await.map(|(s, _)| s)
}

// ───────────────────────────── background loops ─────────────────────────────

pub(crate) fn spawn_all(mesh: &Mesh, listener: TcpListener) {
    tokio::spawn(accept_loop(mesh.clone(), listener));
    tokio::spawn(dial_loop(mesh.clone()));
    tokio::spawn(housekeeping(mesh.clone()));
}

async fn accept_loop(mesh: Mesh, listener: TcpListener) {
    let gate = std::sync::Arc::new(Semaphore::new(64));
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        let _ = stream.set_nodelay(true);
        let Ok(permit) = gate.clone().try_acquire_owned() else {
            continue; // too many half-open handshakes: drop this one
        };
        let m = mesh.clone();
        tokio::spawn(async move {
            handle_incoming(m, stream, permit).await;
        });
    }
}

/// Every few seconds: publish a heartbeat so that nodes that only hear about us second-hand can
/// tell we are alive, and forget nodes that have been gone a long time.
async fn housekeeping(mesh: Mesh) {
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    loop {
        tick.tick().await;
        mesh.update_self(|_| true);
        mesh.0.reg.lock().unwrap().forget_stale(Instant::now());
        // Old, unanswered requests (a node vanished) are cleaned by their own timeouts.
    }
}

/// Keeps a link to every known member: dial the ones we are not connected to, with back-off.
async fn dial_loop(mesh: Mesh) {
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        if !mesh.joined() {
            continue;
        }
        let port = mesh.port();
        let mine: Vec<String> = local_addrs(port);
        let is_mine = |a: &str| {
            mine.iter().any(|m| m == a)
                || a == format!("127.0.0.1:{port}")
                || a == format!("localhost:{port}")
        };

        // (key for back-off, expected node id, addresses)
        let mut jobs: Vec<(String, Option<String>, Vec<String>)> = Vec::new();
        let mut covered: Vec<String> = Vec::new();
        {
            let reg = mesh.0.reg.lock().unwrap();
            let peers = mesh.0.peers.lock().unwrap();
            for e in reg.all_records() {
                if e.node_id == mesh.id() {
                    continue;
                }
                covered.extend(e.addrs.iter().cloned());
                if peers.contains_key(&e.node_id) {
                    continue;
                }
                let addrs: Vec<String> = e.addrs.iter().filter(|a| !is_mine(a)).cloned().collect();
                if !addrs.is_empty() {
                    jobs.push((e.node_id.clone(), Some(e.node_id.clone()), addrs));
                }
            }
        }
        let linked_addrs: Vec<String> = covered;
        let known: Vec<String> = mesh.0.known_addrs.lock().unwrap().iter().cloned().collect();
        for a in known {
            if is_mine(&a) || linked_addrs.contains(&a) {
                continue;
            }
            // An address we already reached, whose device we are still linked to.
            let owner = mesh.0.addr_owner.lock().unwrap().get(&a).cloned();
            if owner.is_some_and(|o| mesh.0.peers.lock().unwrap().contains_key(&o)) {
                continue;
            }
            jobs.push((a.clone(), None, vec![a]));
        }

        let now = Instant::now();
        for (key, expect, addrs) in jobs {
            {
                let mut st = mesh.0.dial_state.lock().unwrap();
                if let Some((next, _)) = st.get(&key) {
                    if *next > now {
                        continue;
                    }
                }
                let fails = st.get(&key).map(|s| s.1).unwrap_or(0);
                // Mark as in progress so the next tick does not start a second attempt.
                st.insert(key.clone(), (now + Duration::from_secs(15), fails));
            }
            let m = mesh.clone();
            tokio::spawn(async move {
                match dial(&m, &addrs, expect).await {
                    Ok(()) => {
                        m.0.dial_state.lock().unwrap().remove(&key);
                    }
                    Err(_) => {
                        let mut st = m.0.dial_state.lock().unwrap();
                        let fails = st.get(&key).map(|s| s.1).unwrap_or(0) + 1;
                        let wait = (1u64 << fails.min(5)).min(30);
                        st.insert(key, (Instant::now() + Duration::from_secs(wait), fails));
                    }
                }
            });
        }
    }
}

/// Connects to a node, runs the control session until it ends.
async fn dial(mesh: &Mesh, addrs: &[String], expect: Option<String>) -> Result<(), String> {
    let psk = mesh.psk().ok_or("not in a cluster")?;
    let stream = connect_any(addrs).await?;
    let mut sec = tokio::time::timeout(
        HANDSHAKE_WAIT,
        crypto::connect(stream, &mesh.0.id, Some(psk)),
    )
    .await
    .map_err(|_| "handshake timed out".to_string())?
    .map_err(|e| e.to_string())?;
    if let Some(e) = expect {
        if sec.remote_id != e {
            return Err("that address belongs to a different device".into());
        }
    }
    // Remember who lives at these addresses, so we do not keep dialling a device we already
    // have a link to.
    {
        let mut owners = mesh.0.addr_owner.lock().unwrap();
        for a in addrs {
            owners.insert(a.clone(), sec.remote_id.clone());
        }
    }
    if mesh.0.peers.lock().unwrap().contains_key(&sec.remote_id) {
        // Already linked (the other side dialled us at the same time). Not an error.
        let _ = send(&mut sec.writer, &Open::Control).await;
        sec.writer.shutdown().await;
        return Ok(());
    }
    send(&mut sec.writer, &Open::Control).await?;
    let reply: OpenReply = recv(&mut sec.reader, HANDSHAKE_WAIT).await?;
    if !reply.ok {
        return Err(reply.error);
    }
    run_control(mesh.clone(), sec, true).await;
    Ok(())
}

// ───────────────────────────── message helpers ─────────────────────────────

async fn send<W, T>(w: &mut crypto::SecureWriter<W>, v: &T) -> Result<(), String>
where
    W: tokio::io::AsyncWrite + Unpin,
    T: serde::Serialize,
{
    let bytes = serde_json::to_vec(v).map_err(|e| e.to_string())?;
    w.send_msg(&bytes).await.map_err(|e| e.to_string())
}

async fn recv<R, T>(r: &mut crypto::SecureReader<R>, wait: Duration) -> Result<T, String>
where
    R: tokio::io::AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    let bytes = tokio::time::timeout(wait, r.recv_msg())
        .await
        .map_err(|_| "timed out waiting for the other device".to_string())?
        .map_err(|e| e.to_string())?
        .ok_or("the other device closed the link")?;
    serde_json::from_slice(&bytes).map_err(|e| format!("bad message: {e}"))
}

// ───────────────────────────── incoming ─────────────────────────────

async fn handle_incoming(
    mesh: Mesh,
    mut stream: TcpStream,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    let Ok(Ok(mode)) =
        tokio::time::timeout(Duration::from_secs(5), crypto::read_mode(&mut stream)).await
    else {
        return;
    };
    match mode {
        crypto::MODE_CLUSTER => {
            let Some(psk) = mesh.psk() else { return };
            let Ok(Ok(mut sec)) = tokio::time::timeout(
                HANDSHAKE_WAIT,
                crypto::accept(stream, &mesh.0.id, Some(psk)),
            )
            .await
            else {
                return;
            };
            let Ok(open) = recv::<_, Open>(&mut sec.reader, HANDSHAKE_WAIT).await else {
                return;
            };
            match open {
                Open::Control => {
                    if send(
                        &mut sec.writer,
                        &OpenReply {
                            ok: true,
                            error: String::new(),
                        },
                    )
                    .await
                    .is_ok()
                    {
                        drop(_permit);
                        run_control(mesh, sec, false).await;
                    }
                }
                Open::Tunnel { service } => {
                    drop(_permit);
                    serve_tunnel(&mesh, sec, &service).await;
                }
            }
        }
        crypto::MODE_PAIR if mesh.pairing_is_open() && mesh.joined() => {
            host_pairing(mesh, stream).await;
        }
        _ => {}
    }
}

/// The host side of pairing: show the code, wait for a person to say yes.
async fn host_pairing(mesh: Mesh, stream: TcpStream) {
    let Ok(Ok(mut sec)) =
        tokio::time::timeout(HANDSHAKE_WAIT, crypto::accept(stream, &mesh.0.id, None)).await
    else {
        return;
    };
    let Ok(hello) = recv::<_, PairHello>(&mut sec.reader, HANDSHAKE_WAIT).await else {
        return;
    };
    // A name is only a claim: keep it short and printable.
    let name: String = hello
        .name
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    let sas = crypto::sas(&sec.handshake_hash);
    let (tx, rx) = oneshot::channel();
    let req_id = mesh.0.next_req.fetch_add(1, Ordering::Relaxed);
    {
        let mut list = mesh.0.pair_in.lock().unwrap();
        list.retain(|r| r.created.elapsed() < PAIR_WINDOW);
        if list.len() >= MAX_PENDING_PAIRS {
            return;
        }
        list.push(PairRequest {
            id: req_id,
            remote_id: sec.remote_id.clone(),
            name: name.clone(),
            sas: sas.clone(),
            created: Instant::now(),
            decide: Some(tx),
        });
    }
    mesh.emit(Event::PairRequested {
        name: name.clone(),
        sas,
    });
    let allowed = matches!(tokio::time::timeout(PAIR_WINDOW, rx).await, Ok(Ok(true)));
    mesh.0.pair_in.lock().unwrap().retain(|r| r.id != req_id);
    let answer = match (allowed, mesh.key_for_pairing()) {
        (true, Some((key, cluster_name))) => PairAnswer {
            allowed: true,
            key,
            cluster_name,
        },
        _ => PairAnswer {
            allowed: false,
            key: String::new(),
            cluster_name: String::new(),
        },
    };
    let _ = send(&mut sec.writer, &answer).await;
    sec.writer.shutdown().await;
}

impl Mesh {
    /// Asks the device at `addr` to let us into its cluster. Progress shows up in the snapshot's
    /// `pair_status`: both screens display the same six digits, and the host presses Allow.
    pub fn pair_start(&self, addr: String) {
        *self.0.pair_out.lock().unwrap() = PairStatus::Connecting;
        let mesh = self.clone();
        tokio::spawn(async move {
            let result = pair_flow(&mesh, &addr).await;
            let mut st = mesh.0.pair_out.lock().unwrap();
            *st = match result {
                Ok(true) => PairStatus::Joined,
                Ok(false) => PairStatus::Denied,
                Err(error) => PairStatus::Failed { error },
            };
        });
    }

    pub fn pair_reset(&self) {
        *self.0.pair_out.lock().unwrap() = PairStatus::Idle;
    }
}

async fn pair_flow(mesh: &Mesh, addr: &str) -> Result<bool, String> {
    let stream = connect_any(&[addr.to_string()]).await?;
    let mut sec = tokio::time::timeout(HANDSHAKE_WAIT, crypto::connect(stream, &mesh.0.id, None))
        .await
        .map_err(|_| "the other device did not answer".to_string())?
        .map_err(|e| e.to_string())?;
    send(&mut sec.writer, &PairHello { name: mesh.name() }).await?;
    *mesh.0.pair_out.lock().unwrap() = PairStatus::Waiting {
        sas: crypto::sas(&sec.handshake_hash),
    };
    let answer: PairAnswer = recv(&mut sec.reader, PAIR_WINDOW + Duration::from_secs(10))
        .await
        .map_err(|e| format!("{e}. Is pairing open on the other PC? (Cluster → Invite a PC)"))?;
    if !answer.allowed {
        return Ok(false);
    }
    mesh.join_cluster(&answer.key, &answer.cluster_name)?;
    mesh.add_peer(addr);
    Ok(true)
}

// ───────────────────────────── control session ─────────────────────────────

/// A link that has said nothing for this long is treated as dead when a new one arrives.
const STALE_LINK: Duration = Duration::from_secs(5);

fn register_link(mesh: &Mesh, peer: &str, link: PeerLink) -> bool {
    // How long the current link has been quiet (read before taking the links lock, so the two
    // locks are never held in the opposite order of the dial loop).
    let quiet = mesh
        .0
        .reg
        .lock()
        .unwrap()
        .get(peer)
        .map(|e| Instant::now().saturating_duration_since(e.last_seen))
        .unwrap_or_default();
    let mut peers = mesh.0.peers.lock().unwrap();
    if let Some(old) = peers.get(peer) {
        if quiet > STALE_LINK {
            // The old link is half-dead (the other side never said goodbye): take the new one.
            let _ = old.tx.send(Msg::Bye);
            peers.insert(peer.to_string(), link);
            return true;
        }
        // Two nodes can dial each other at the same moment. Both must keep the same one: the
        // connection opened by the node with the smaller id. A second connection made the same
        // way as a working one is simply refused; the old one is closed only when the new one
        // is the preferred kind.
        let keep = std::cmp::min(mesh.id(), peer.to_string());
        if link.initiator != keep || old.initiator == keep {
            return false;
        }
        let _ = old.tx.send(Msg::Bye);
    }
    peers.insert(peer.to_string(), link);
    true
}

/// Runs one control link until it ends: exchanges records, answers pings, runs commands.
pub(crate) async fn run_control(mesh: Mesh, sec: SecureStream<TcpStream>, initiated: bool) {
    let SecureStream {
        mut reader,
        mut writer,
        remote_id,
        ..
    } = sec;

    let hello = Msg::Hello {
        proto: PROTO_VERSION,
        record: Box::new(mesh.my_record()),
    };
    if send(&mut writer, &hello).await.is_err() {
        return;
    }
    let first: Result<Msg, String> = recv(&mut reader, Duration::from_secs(5)).await;
    let Ok(Msg::Hello { record, .. }) = first else {
        return;
    };
    // The handshake proved who is on the other end; a record that says otherwise is a lie.
    if record.node_id != remote_id || record.cluster_id != mesh.cluster_id() {
        return;
    }
    let peer_name = record.name.clone();
    {
        let mut reg = mesh.0.reg.lock().unwrap();
        reg.merge(*record, Instant::now());
    }

    let conn = mesh.0.next_conn.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    let link = PeerLink {
        tx: tx.clone(),
        initiator: if initiated {
            mesh.id()
        } else {
            remote_id.clone()
        },
        conn,
    };
    if !register_link(&mesh, &remote_id, link) {
        return;
    }
    mesh.0
        .reg
        .lock()
        .unwrap()
        .set_linked(&remote_id, true, Instant::now());
    mesh.emit(Event::PeerUp {
        id: remote_id.clone(),
        name: peer_name,
    });
    let _ = tx.send(Msg::Gossip {
        records: mesh.0.reg.lock().unwrap().all_records(),
    });

    // Writer: everything we send goes through here, in order.
    let wtask = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let bye = matches!(m, Msg::Bye);
            let Ok(bytes) = serde_json::to_vec(&m) else {
                continue;
            };
            if writer.send_msg(&bytes).await.is_err() || bye {
                break;
            }
        }
        writer.shutdown().await;
    });

    // Ticker: pings, our record whenever it changes, and the whole table now and then.
    let ticker = {
        let mesh = mesh.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut changed = mesh.0.changed.subscribe();
            let mut ping = tokio::time::interval(Duration::from_secs(2));
            let mut full = tokio::time::interval(Duration::from_secs(10));
            loop {
                tokio::select! {
                    _ = ping.tick() => {
                        if tx.send(Msg::Ping { t: now_ms() }).is_err() { break; }
                    }
                    _ = full.tick() => {
                        let records = mesh.0.reg.lock().unwrap().all_records();
                        if tx.send(Msg::Gossip { records }).is_err() { break; }
                    }
                    r = changed.changed() => {
                        if r.is_err() { break; }
                        let mine = mesh.my_record();
                        if tx.send(Msg::Gossip { records: vec![mine] }).is_err() { break; }
                    }
                }
            }
        })
    };

    // Reader.
    let mut epoch = mesh.0.epoch.subscribe();
    epoch.borrow_and_update();
    loop {
        let bytes = tokio::select! {
            r = tokio::time::timeout(SILENCE, reader.recv_msg()) => match r {
                Ok(Ok(Some(b))) => b,
                _ => break,
            },
            // The cluster key changed (this device left or joined another cluster).
            _ = epoch.changed() => {
                let _ = tx.send(Msg::Bye);
                break;
            }
        };
        let Ok(msg) = serde_json::from_slice::<Msg>(&bytes) else {
            continue;
        };
        mesh.0.reg.lock().unwrap().touch(&remote_id, Instant::now());
        match msg {
            Msg::Hello { .. } => {}
            Msg::Gossip { records } => {
                let now = Instant::now();
                let mut reg = mesh.0.reg.lock().unwrap();
                for r in records.into_iter().take(512) {
                    reg.merge(r, now);
                }
            }
            Msg::Ping { t } => {
                let _ = tx.send(Msg::Pong { t });
            }
            Msg::Pong { t } => {
                let rtt = now_ms().saturating_sub(t) as f64;
                mesh.0.reg.lock().unwrap().set_rtt(&remote_id, rtt);
            }
            Msg::Req { id, cmd } => {
                let tx = tx.clone();
                let from = remote_id.clone();
                let ex = mesh.executor();
                tokio::spawn(async move {
                    let res = match ex {
                        Some(ex) => ex(from, cmd).await,
                        None => Err("this device cannot run commands".to_string()),
                    };
                    let _ = tx.send(match res {
                        Ok(value) => Msg::Res {
                            id,
                            ok: true,
                            value,
                            error: String::new(),
                        },
                        Err(error) => Msg::Res {
                            id,
                            ok: false,
                            value: serde_json::Value::Null,
                            error,
                        },
                    });
                });
            }
            Msg::Res {
                id,
                ok,
                value,
                error,
            } => {
                if let Some(w) = mesh.0.pending.lock().unwrap().remove(&id) {
                    let _ = w.send(if ok { Ok(value) } else { Err(error) });
                }
            }
            Msg::Bye => break,
        }
    }

    ticker.abort();
    // Take this link out of the table first. The table holds a sender for the writer, so the
    // writer only finishes once the link is gone from it: waiting for the writer before removing
    // the link would wait forever and leave a dead link that blocks the peer's return.
    let was_current = {
        let mut peers = mesh.0.peers.lock().unwrap();
        match peers.get(&remote_id) {
            Some(l) if l.conn == conn => {
                peers.remove(&remote_id);
                true
            }
            _ => false,
        }
    };
    drop(tx);
    // Give a goodbye a moment to go out, then stop the writer whatever it is doing.
    let mut wtask = wtask;
    let _ = tokio::time::timeout(Duration::from_millis(300), &mut wtask).await;
    wtask.abort();
    if was_current {
        let name = {
            let mut reg = mesh.0.reg.lock().unwrap();
            reg.set_linked(&remote_id, false, Instant::now());
            reg.get(&remote_id)
                .map(|e| e.record.name.clone())
                .unwrap_or_default()
        };
        mesh.emit(Event::PeerDown {
            id: remote_id,
            name,
        });
    }
}

// ───────────────────────────── tunnels ─────────────────────────────

/// Copies bytes both ways between an encrypted link and a plain TCP connection.
pub(crate) async fn pipe(sec: SecureStream<TcpStream>, tcp: TcpStream) {
    let _ = tcp.set_nodelay(true);
    let SecureStream {
        mut reader,
        mut writer,
        ..
    } = sec;
    let (mut tr, mut tw) = tcp.into_split();
    let up = async {
        let mut buf = vec![0u8; MAX_PLAIN.min(60_000)];
        loop {
            let n = tr.read(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            writer
                .send_all(&buf[..n])
                .await
                .map_err(|e| e.to_string())?;
        }
        writer.shutdown().await;
        Ok::<(), String>(())
    };
    let down = async {
        while let Some(chunk) = reader.recv_chunk().await.map_err(|e| e.to_string())? {
            tw.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
        let _ = tw.shutdown().await;
        Ok::<(), String>(())
    };
    let _ = tokio::try_join!(up, down);
}

async fn serve_tunnel(mesh: &Mesh, mut sec: SecureStream<TcpStream>, service: &str) {
    let stream_handler = mesh.0.streams.lock().unwrap().get(service).cloned();
    if let Some(h) = stream_handler {
        if send(
            &mut sec.writer,
            &OpenReply {
                ok: true,
                error: String::new(),
            },
        )
        .await
        .is_ok()
        {
            let caller = sec.remote_id.clone();
            h(caller, sec).await;
        }
        return;
    }
    let addr = mesh.0.services.lock().unwrap().get(service).copied();
    let Some(addr) = addr else {
        let _ = send(
            &mut sec.writer,
            &OpenReply {
                ok: false,
                error: format!("this device does not offer '{service}' right now"),
            },
        )
        .await;
        return;
    };
    let tcp = match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await {
        Ok(Ok(t)) => t,
        _ => {
            let _ = send(
                &mut sec.writer,
                &OpenReply {
                    ok: false,
                    error: format!("'{service}' is not answering on that device"),
                },
            )
            .await;
            return;
        }
    };
    if send(
        &mut sec.writer,
        &OpenReply {
            ok: true,
            error: String::new(),
        },
    )
    .await
    .is_err()
    {
        return;
    }
    pipe(sec, tcp).await;
}

impl Mesh {
    /// Opens an encrypted byte pipe to `service` on another member.
    pub async fn open_tunnel(
        &self,
        node: &str,
        service: &str,
    ) -> Result<SecureStream<TcpStream>, String> {
        let psk = self.psk().ok_or("not in a cluster")?;
        let addrs = {
            let reg = self.0.reg.lock().unwrap();
            reg.get(node)
                .map(|e| e.record.addrs.clone())
                .ok_or("unknown device")?
        };
        // Prefer the address we already reached it on.
        let stream = connect_any(&addrs).await?;
        let mut sec = tokio::time::timeout(
            HANDSHAKE_WAIT,
            crypto::connect(stream, &self.0.id, Some(psk)),
        )
        .await
        .map_err(|_| "handshake timed out".to_string())?
        .map_err(|e| e.to_string())?;
        if sec.remote_id != node {
            return Err("that address belongs to a different device".into());
        }
        send(
            &mut sec.writer,
            &Open::Tunnel {
                service: service.to_string(),
            },
        )
        .await?;
        let reply: OpenReply = recv(&mut sec.reader, HANDSHAKE_WAIT).await?;
        if !reply.ok {
            return Err(reply.error);
        }
        Ok(sec)
    }

    /// A local address (127.0.0.1:port) that reaches `service` on `node` through an encrypted
    /// tunnel. Programs that only speak plain TCP, like llama.cpp's `--rpc`, connect here.
    pub async fn forward(&self, node: &str, service: &str) -> Result<SocketAddr, String> {
        if node == self.id() {
            return self
                .0
                .services
                .lock()
                .unwrap()
                .get(service)
                .copied()
                .ok_or_else(|| format!("'{service}' is not running here"));
        }
        let key = (node.to_string(), service.to_string());
        if let Some((addr, handle)) = self.0.forwards.lock().unwrap().get(&key) {
            if !handle.is_finished() {
                return Ok(*addr);
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| e.to_string())?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let mesh = self.clone();
        let (n, s) = key.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    break;
                };
                let mesh = mesh.clone();
                let (n, s) = (n.clone(), s.clone());
                tokio::spawn(async move {
                    if let Ok(sec) = mesh.open_tunnel(&n, &s).await {
                        pipe(sec, tcp).await;
                    }
                });
            }
        });
        self.0.forwards.lock().unwrap().insert(key, (addr, handle));
        Ok(addr)
    }

    /// Closes the local doorway made by `forward`.
    pub fn stop_forward(&self, node: &str, service: &str) {
        if let Some((_, handle)) = self
            .0
            .forwards
            .lock()
            .unwrap()
            .remove(&(node.to_string(), service.to_string()))
        {
            handle.abort();
        }
    }

    /// Closes every doorway to `node`.
    pub fn stop_forwards_to(&self, node: &str) {
        let mut f: HashMap<_, _> = std::mem::take(&mut *self.0.forwards.lock().unwrap());
        f.retain(|(n, _), (_, h)| {
            if n == node {
                h.abort();
                false
            } else {
                true
            }
        });
        *self.0.forwards.lock().unwrap() = f;
    }
}

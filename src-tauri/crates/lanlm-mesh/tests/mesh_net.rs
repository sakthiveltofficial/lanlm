//! Several nodes in one process, talking over real sockets on this machine.

use std::time::{Duration, Instant};

use lanlm_mesh::proto::{Command, HeldModel};
use lanlm_mesh::{Mesh, MeshConfig, PairStatus};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "lanlm-test-{tag}-{}-{}",
        std::process::id(),
        rand::random::<u32>()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn node(name: &str) -> Mesh {
    let mut cfg = MeshConfig::new(temp_dir(name));
    cfg.name = Some(name.to_string());
    cfg.port = 0;
    cfg.discovery = false;
    Mesh::start(cfg).await.unwrap()
}

fn addr(m: &Mesh) -> String {
    format!("127.0.0.1:{}", m.port())
}

async fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn alive_count(m: &Mesh) -> usize {
    m.snapshot().nodes.iter().filter(|n| n.alive).count()
}

/// Three members that all know each other's address.
async fn cluster3() -> (Mesh, Mesh, Mesh) {
    let (a, b, c) = (
        node("alpha").await,
        node("bravo").await,
        node("charlie").await,
    );
    let key = a.create_cluster("test");
    b.join_cluster(&key, "test").unwrap();
    c.join_cluster(&key, "test").unwrap();
    for (m, others) in [(&a, [&b, &c]), (&b, [&a, &c]), (&c, [&a, &b])] {
        for o in others {
            m.add_peer(&addr(o));
        }
    }
    for m in [&a, &b, &c] {
        wait_for("3 nodes", 15, || alive_count(m) == 3).await;
    }
    (a, b, c)
}

#[tokio::test]
async fn members_find_each_other_and_share_their_records() {
    let (a, b, c) = cluster3().await;
    // Everyone sees the same three names.
    for m in [&a, &b, &c] {
        let mut names: Vec<String> = m
            .snapshot()
            .nodes
            .iter()
            .map(|n| n.record.name.clone())
            .collect();
        names.sort();
        assert_eq!(names, ["alpha", "bravo", "charlie"]);
    }
    // A change on one node reaches the others quickly.
    b.update_self(|r| {
        r.models.push(HeldModel {
            file: "tiny.gguf".into(),
            size: 123,
            ..Default::default()
        });
        true
    });
    for m in [&a, &c] {
        wait_for("model list to spread", 6, || {
            m.with_registry(|r, now| r.holders("tiny.gguf", now).len() == 1)
        })
        .await;
    }
    // Duplicate dials were collapsed: every node keeps one live link per peer.
    tokio::time::sleep(Duration::from_secs(4)).await;
    for m in [&a, &b, &c] {
        assert_eq!(
            m.snapshot()
                .nodes
                .iter()
                .filter(|n| n.linked && !n.is_me)
                .count(),
            2
        );
        assert_eq!(alive_count(m), 3);
    }
}

#[tokio::test]
async fn links_stay_up_once_the_cluster_has_formed() {
    let (a, b, c) = cluster3().await;
    // Let any duplicate dials from start-up settle, then watch for a while.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let mut rx = [a.subscribe(), b.subscribe(), c.subscribe()];
    tokio::time::sleep(Duration::from_secs(9)).await;
    for r in rx.iter_mut() {
        while let Ok(e) = r.try_recv() {
            assert!(
                matches!(e, lanlm_mesh::Event::Log(_)),
                "a healthy link was dropped or rebuilt: {e:?}"
            );
        }
    }
}

#[tokio::test]
async fn commands_run_on_the_other_node_and_errors_come_back() {
    let (a, b, _c) = cluster3().await;
    b.set_executor(std::sync::Arc::new(|from, cmd| {
        Box::pin(async move {
            match cmd {
                Command::Bench => Ok(serde_json::json!({ "from": from, "ok": true })),
                Command::StopInstance { instance } => Err(format!("no instance {instance}")),
                _ => Err("unsupported".into()),
            }
        })
    }));
    let v = a
        .call(&b.id(), Command::Bench, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(v["from"], a.id());
    let e = a
        .call(
            &b.id(),
            Command::StopInstance {
                instance: "x".into(),
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
    assert_eq!(e, "no instance x");
    // A node with no executor refuses politely.
    let e = b
        .call(&a.id(), Command::Bench, Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(e.contains("cannot run"), "{e}");
}

#[tokio::test]
async fn a_tunnel_carries_plain_tcp_between_members_encrypted() {
    let (a, b, _c) = cluster3().await;
    // B runs a little echo service and offers it to the cluster.
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = echo.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                loop {
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    if s.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    b.register_service("echo", echo_addr);

    let local = a.forward(&b.id(), "echo").await.unwrap();
    let s = TcpStream::connect(local).await.unwrap();
    let payload: Vec<u8> = (0..1_500_000u32).map(|i| (i % 253) as u8).collect();
    let p2 = payload.clone();
    let (mut r, mut w) = s.into_split();
    let writer = tokio::spawn(async move {
        w.write_all(&p2).await.unwrap();
        w
    });
    let mut got = vec![0u8; payload.len()];
    r.read_exact(&mut got).await.unwrap();
    assert!(got == payload, "echoed bytes differ");
    let _ = writer.await;

    // A service that is not offered is refused, not hung.
    let refused = a.open_tunnel(&b.id(), "nothing").await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_device_with_another_key_never_joins() {
    let a = node("alpha").await;
    let key = a.create_cluster("one");
    let outsider = node("outsider").await;
    outsider.create_cluster("two"); // a different cluster, a different key
    outsider.add_peer(&addr(&a));
    a.add_peer(&addr(&outsider));
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(alive_count(&a), 1);
    assert_eq!(alive_count(&outsider), 1);
    assert!(a.open_tunnel(&outsider.id(), "x").await.is_err());
    // The right key works.
    let friend = node("friend").await;
    friend.join_cluster(&key, "one").unwrap();
    friend.add_peer(&addr(&a));
    wait_for("friend joins", 10, || alive_count(&a) == 2).await;
}

#[tokio::test]
async fn a_node_that_leaves_disappears_from_the_others() {
    let (a, b, c) = cluster3().await;
    c.leave_cluster();
    wait_for("c to vanish", 12, || {
        alive_count(&a) == 2 && alive_count(&b) == 2
    })
    .await;
    assert!(!c.joined());
    assert_eq!(alive_count(&c), 1);
}

#[tokio::test]
async fn pairing_needs_a_person_to_say_yes_and_both_screens_show_the_same_code() {
    let host = node("host").await;
    host.create_cluster("home");
    let guest = node("guest").await;

    // Pairing closed: the guest cannot even start.
    guest.pair_start(addr(&host));
    wait_for("failure while closed", 15, || {
        matches!(guest.snapshot().pair_status, PairStatus::Failed { .. })
    })
    .await;
    assert!(!guest.joined());

    host.open_pairing();
    guest.pair_start(addr(&host));
    wait_for("code on the guest", 10, || {
        matches!(guest.snapshot().pair_status, PairStatus::Waiting { .. })
    })
    .await;
    wait_for("request on the host", 10, || {
        !host.snapshot().pair_requests.is_empty()
    })
    .await;
    let req = host.snapshot().pair_requests[0].clone();
    let PairStatus::Waiting { sas } = guest.snapshot().pair_status else {
        unreachable!()
    };
    assert_eq!(req.sas, sas, "both screens must show the same code");
    assert_eq!(req.name, "guest");
    assert!(!guest.joined(), "nothing is shared before the host agrees");

    assert!(host.pair_decide(req.id, true));
    wait_for("guest joined", 10, || {
        guest.snapshot().pair_status == PairStatus::Joined
    })
    .await;
    assert_eq!(guest.cluster_id(), host.cluster_id());
    wait_for("guest visible", 10, || alive_count(&host) == 2).await;
}

#[tokio::test]
async fn a_refused_pairing_shares_nothing() {
    let host = node("host").await;
    host.create_cluster("home");
    host.open_pairing();
    let guest = node("guest").await;
    guest.pair_start(addr(&host));
    wait_for("request", 10, || !host.snapshot().pair_requests.is_empty()).await;
    let id = host.snapshot().pair_requests[0].id;
    host.pair_decide(id, false);
    wait_for("denied", 10, || {
        guest.snapshot().pair_status == PairStatus::Denied
    })
    .await;
    assert!(!guest.joined());
}

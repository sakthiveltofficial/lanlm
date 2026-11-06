use super::*;
use crate::proto::{HeldModel, Transfer};

fn rec(id: &str, cluster: &str, seq: u64) -> NodeRecord {
    NodeRecord {
        node_id: id.into(),
        name: id.to_uppercase(),
        cluster_id: cluster.into(),
        seq,
        ..Default::default()
    }
}

fn reg(now: Instant) -> Registry {
    Registry::new(rec("me", "c1", 1), now)
}

#[test]
fn the_newest_record_wins_and_old_ones_are_ignored() {
    let t = Instant::now();
    let mut r = reg(t);
    assert!(r.merge(rec("a", "c1", 5), t));
    assert!(!r.merge(rec("a", "c1", 4), t));
    assert!(!r.merge(rec("a", "c1", 5), t));
    assert!(r.merge(rec("a", "c1", 6), t));
    assert_eq!(r.get("a").unwrap().record.seq, 6);
}

#[test]
fn nobody_can_overwrite_our_own_record_or_join_from_another_cluster() {
    let t = Instant::now();
    let mut r = reg(t);
    let mut fake = rec("me", "c1", 999);
    fake.name = "EVIL".into();
    assert!(!r.merge(fake, t));
    assert_eq!(r.my_record().name, "ME");
    assert!(!r.merge(rec("x", "other-cluster", 1), t));
    assert!(r.get("x").is_none());
}

#[test]
fn silent_nodes_expire_and_busy_ones_get_longer() {
    let t = Instant::now();
    let mut r = reg(t);
    r.merge(rec("quiet", "c1", 1), t);
    let mut loading = rec("loading", "c1", 1);
    loading.instance = Some(InstanceInfo {
        state: InstanceState::Loading,
        ..Default::default()
    });
    r.merge(loading, t);
    let mut copying = rec("copying", "c1", 1);
    copying.transfers.push(Transfer::default());
    r.merge(copying, t);

    let soon = t + Duration::from_millis(LIVE_MS - 100);
    assert!(r.is_alive("quiet", soon));
    let later = t + Duration::from_millis(LIVE_MS + 100);
    assert!(!r.is_alive("quiet", later));
    assert!(r.is_alive("loading", later));
    assert!(r.is_alive("copying", later));
    let much_later = t + Duration::from_millis(LIVE_BUSY_MS + 100);
    assert!(!r.is_alive("loading", much_later));
    // We are always alive to ourselves.
    assert!(r.is_alive("me", much_later));
}

#[test]
fn hearing_from_a_node_keeps_it_alive() {
    let t = Instant::now();
    let mut r = reg(t);
    r.merge(rec("a", "c1", 1), t);
    let t2 = t + Duration::from_millis(7_000);
    r.touch("a", t2);
    assert!(r.is_alive("a", t2 + Duration::from_millis(7_000)));
}

#[test]
fn a_dropped_link_leaves_only_a_short_grace_period() {
    let t = Instant::now();
    let mut r = reg(t);
    r.merge(rec("a", "c1", 1), t);
    r.set_linked("a", true, t);
    r.set_linked("a", false, t);
    assert!(r.is_alive("a", t + Duration::from_millis(GRACE_MS - 200)));
    assert!(!r.is_alive("a", t + Duration::from_millis(GRACE_MS + 200)));
    // Reconnecting in time revives it.
    r.set_linked("a", true, t + Duration::from_millis(1_000));
    assert!(r.is_alive("a", t + Duration::from_millis(5_000)));
}

#[test]
fn a_busy_node_whose_link_drops_still_gets_only_the_grace_period() {
    let t = Instant::now();
    let mut r = reg(t);
    let mut loading = rec("l", "c1", 1);
    loading.instance = Some(InstanceInfo {
        state: InstanceState::Loading,
        ..Default::default()
    });
    r.merge(loading, t);
    r.set_linked("l", true, t);
    r.set_linked("l", false, t);
    assert!(!r.is_alive("l", t + Duration::from_millis(GRACE_MS + 200)));
}

#[test]
fn rtt_is_smoothed() {
    let t = Instant::now();
    let mut r = reg(t);
    r.merge(rec("a", "c1", 1), t);
    r.set_rtt("a", 10.0);
    assert_eq!(r.get("a").unwrap().rtt_ms, Some(10.0));
    r.set_rtt("a", 20.0);
    let v = r.get("a").unwrap().rtt_ms.unwrap();
    assert!(v > 10.0 && v < 20.0);
}

#[test]
fn holders_and_instances_only_list_live_nodes() {
    let t = Instant::now();
    let mut r = reg(t);
    let mut a = rec("a", "c1", 1);
    a.models.push(HeldModel {
        file: "m.gguf".into(),
        ..Default::default()
    });
    a.instance = Some(InstanceInfo {
        id: "i1".into(),
        state: InstanceState::Ready,
        ..Default::default()
    });
    r.merge(a, t);
    assert_eq!(r.holders("m.gguf", t).len(), 1);
    assert_eq!(r.instances(t).len(), 1);
    let gone = t + Duration::from_millis(LIVE_MS + 1);
    assert!(r.holders("m.gguf", gone).is_empty());
    assert!(r.instances(gone).is_empty());
}

#[test]
fn views_put_this_node_first_and_forget_removes_old_nodes() {
    let t = Instant::now();
    let mut r = reg(t);
    r.merge(rec("b", "c1", 1), t);
    r.merge(rec("a", "c1", 1), t);
    let v = r.views(t);
    assert!(v[0].is_me);
    assert_eq!(v[1].record.node_id, "a");
    r.forget_stale(t + Duration::from_millis(FORGET_MS + 1));
    assert_eq!(r.views(t).len(), 1);
}

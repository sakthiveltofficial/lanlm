use super::*;

fn be(id: &str, slots: u32, busy: u32, tok_s: f64) -> Backend {
    Backend {
        instance: id.into(),
        node_id: format!("node-{id}"),
        node_name: id.to_uppercase(),
        model: "m.gguf".into(),
        slots,
        busy,
        tok_s,
    }
}

fn inflight(pairs: &[(&str, u32)]) -> HashMap<String, u32> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn the_least_loaded_server_gets_the_request() {
    let b = vec![
        be("a", 4, 3, 10.0),
        be("b", 4, 1, 10.0),
        be("c", 4, 2, 10.0),
    ];
    assert_eq!(choose(&b, &inflight(&[]), "", &[]), Some(1));
}

#[test]
fn local_counts_are_used_when_the_servers_own_numbers_lag() {
    // The server says idle, but this gateway has just sent it three requests.
    let b = vec![be("a", 4, 0, 10.0), be("b", 4, 1, 10.0)];
    let inf = inflight(&[("a", 3)]);
    assert_eq!(choose(&b, &inf, "", &[]), Some(1));
}

#[test]
fn a_full_cluster_makes_requests_wait() {
    let b = vec![be("a", 2, 2, 10.0), be("b", 1, 1, 10.0)];
    assert_eq!(choose(&b, &inflight(&[]), "", &[]), None);
    // A slot opens on b.
    let b = vec![be("a", 2, 2, 10.0), be("b", 1, 0, 10.0)];
    assert_eq!(choose(&b, &inflight(&[]), "", &[]), Some(1));
}

#[test]
fn queued_requests_on_a_server_count_as_load() {
    // a has all slots busy plus two waiting: it is over 1.0 and must not be chosen.
    let b = vec![be("a", 2, 4, 10.0), be("b", 2, 1, 10.0)];
    assert_eq!(choose(&b, &inflight(&[]), "", &[]), Some(1));
}

#[test]
fn equal_load_goes_to_the_faster_server() {
    let b = vec![be("slow", 4, 0, 8.0), be("fast", 4, 0, 40.0)];
    assert_eq!(choose(&b, &inflight(&[]), "", &[]), Some(1));
}

#[test]
fn servers_that_just_failed_are_skipped() {
    let b = vec![be("a", 4, 0, 50.0), be("b", 4, 1, 10.0)];
    assert_eq!(choose(&b, &inflight(&[]), "", &["a".to_string()]), Some(1));
    assert_eq!(
        choose(&b, &inflight(&[]), "", &["a".to_string(), "b".to_string()]),
        None
    );
}

#[test]
fn a_conversation_sticks_to_its_server_while_it_has_room() {
    let b = vec![
        be("a", 4, 0, 10.0),
        be("b", 4, 0, 10.0),
        be("c", 4, 0, 10.0),
    ];
    let key = "system: you are helpful";
    let first = choose(&b, &inflight(&[]), key, &[]).unwrap();
    for _ in 0..20 {
        assert_eq!(choose(&b, &inflight(&[]), key, &[]), Some(first));
    }
    // A little busier than the others is still fine (the cache is worth it)...
    let mut busier = b.clone();
    busier[first].busy = 1;
    assert_eq!(choose(&busier, &inflight(&[]), key, &[]), Some(first));
    // ...but a much busier one is not worth waiting for.
    busier[first].busy = 3;
    assert_ne!(choose(&busier, &inflight(&[]), key, &[]), Some(first));
}

#[test]
fn different_conversations_spread_over_the_servers() {
    let b: Vec<Backend> = (0..4).map(|i| be(&format!("s{i}"), 4, 0, 10.0)).collect();
    let mut used = std::collections::HashSet::new();
    for i in 0..64 {
        used.insert(preferred(&format!("conversation {i}"), &b).unwrap());
    }
    assert_eq!(used.len(), 4);
}

#[test]
fn removing_a_server_only_moves_the_conversations_that_were_on_it() {
    let b: Vec<Backend> = (0..4).map(|i| be(&format!("s{i}"), 4, 0, 10.0)).collect();
    let fewer: Vec<Backend> = b.iter().filter(|x| x.instance != "s2").cloned().collect();
    let mut moved = 0;
    for i in 0..200 {
        let k = format!("conversation {i}");
        let before = &b[preferred(&k, &b).unwrap()].instance;
        let after = &fewer[preferred(&k, &fewer).unwrap()].instance;
        if before != after {
            assert_eq!(before, "s2", "a conversation moved without needing to");
            moved += 1;
        }
    }
    assert!(moved > 0);
    assert_eq!(preferred("", &b), None);
}

#[test]
fn model_names_match_with_or_without_the_extension() {
    assert!(model_matches("Llama-3.2-1B", "llama-3.2-1b.gguf"));
    assert!(model_matches("llama-3.2-1b.gguf", "Llama-3.2-1B.gguf"));
    assert!(!model_matches("llama", "llama-3.2-1b.gguf"));
    assert!(!model_matches("", "llama.gguf"));
}

#[test]
fn the_affinity_key_comes_from_the_start_of_the_conversation() {
    let h = HeaderMap::new();
    let a = json!({ "messages": [
        { "role": "system", "content": "Be brief." },
        { "role": "user", "content": "Hi" },
        { "role": "assistant", "content": "Hello" },
        { "role": "user", "content": "And now?" },
    ]});
    let b = json!({ "messages": [
        { "role": "system", "content": "Be brief." },
        { "role": "user", "content": "Hi" },
        { "role": "assistant", "content": "Hello" },
    ]});
    assert_eq!(affinity_key(&h, &a), affinity_key(&h, &b));
    assert_ne!(
        affinity_key(&h, &a),
        affinity_key(
            &h,
            &json!({ "messages": [{ "role": "user", "content": "other" }] })
        )
    );
    assert_eq!(affinity_key(&h, &json!({ "user": "u-1" })), "u-1");
    let mut hs = HeaderMap::new();
    hs.insert("x-session-id", HeaderValue::from_static("sess"));
    assert_eq!(affinity_key(&hs, &a), "sess");
    assert_eq!(
        affinity_key(&h, &json!({ "prompt": "once upon" })),
        "once upon"
    );
}

#[test]
fn api_keys_are_checked() {
    let mut cfg = GatewayConfig::default();
    let mut h = HeaderMap::new();
    assert!(authorized(&cfg, &h)); // no keys configured: open (loopback only)
    cfg.api_keys = vec!["secret-1".into(), "secret-2".into()];
    assert!(!authorized(&cfg, &h));
    h.insert("authorization", HeaderValue::from_static("Bearer nope"));
    assert!(!authorized(&cfg, &h));
    h.insert("authorization", HeaderValue::from_static("Bearer secret-2"));
    assert!(authorized(&cfg, &h));
    h.insert("authorization", HeaderValue::from_static("secret-2"));
    assert!(!authorized(&cfg, &h));
}

use super::*;

fn cpu_node(id: &str, ram_mb: f64) -> NodeCaps {
    NodeCaps {
        id: id.into(),
        name: id.to_uppercase(),
        available: true,
        engine_ok: true,
        has_rpc: true,
        ram_mb,
        cpu_speed: 20.0,
        ..Default::default()
    }
}

fn gpu_node(id: &str, vram_mb: f64, ram_mb: f64) -> NodeCaps {
    NodeCaps {
        gpus_mb: vec![vram_mb],
        gpu_device0: "CUDA0".into(),
        gpu_speed: 300.0,
        ..cpu_node(id, ram_mb)
    }
}

/// A 7B model at Q4: about 4.4 GB, 32 layers.
fn small() -> ModelSpec {
    ModelSpec {
        file: "small.gguf".into(),
        size_mb: 4400.0,
        n_layer: 32,
    }
}

/// A 70B model at Q4: about 40 GB, 80 layers.
fn big() -> ModelSpec {
    ModelSpec {
        file: "big.gguf".into(),
        size_mb: 40000.0,
        n_layer: 80,
    }
}

fn want(mode: Mode) -> Want {
    Want {
        mode,
        ctx: 4096,
        parallel: 2,
        max_instances: 0,
    }
}

fn layers_total(p: &Planned) -> u32 {
    p.stages.iter().map(|s| s.layers).sum()
}

#[test]
fn every_device_that_fits_the_model_gets_its_own_replica() {
    let nodes = vec![
        cpu_node("a", 12000.0),
        cpu_node("b", 12000.0),
        gpu_node("c", 9000.0, 4000.0),
    ];
    let r = plan(&nodes, &small(), &want(Mode::Auto));
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.instances.len(), 3);
    assert!(r
        .instances
        .iter()
        .all(|p| p.kind == "replica" && layers_total(p) == 32));
    // The GPU box is fastest and listed first.
    assert_eq!(r.instances[0].master, "c");
    assert_eq!(r.instances[0].stages[0].device, "gpu");
    assert_eq!(r.instances[0].ngl, Some(33));
    assert_eq!(r.instances[2].stages[0].device, "cpu");
    assert_eq!(r.instances[2].ngl, Some(0));
    assert!(r.instances[0].est_tok_s > r.instances[2].est_tok_s * 3.0);
}

#[test]
fn a_gpu_too_small_for_the_model_shares_it_with_the_cpu() {
    let nodes = vec![gpu_node("g", 3000.0, 20000.0)];
    let r = plan(&nodes, &small(), &want(Mode::Auto));
    let p = &r.instances[0];
    assert_eq!(p.stages[0].device, "hybrid");
    let ngl = p.ngl.unwrap();
    assert!(ngl > 0 && ngl < 32, "{ngl}");
    // Faster than CPU alone, slower than all-GPU.
    let cpu_only = plan(&[cpu_node("c", 20000.0)], &small(), &want(Mode::Auto));
    let all_gpu = plan(
        &[gpu_node("g", 9000.0, 20000.0)],
        &small(),
        &want(Mode::Auto),
    );
    assert!(p.est_tok_s > cpu_only.instances[0].est_tok_s);
    assert!(p.est_tok_s < all_gpu.instances[0].est_tok_s);
}

#[test]
fn a_model_too_big_for_one_device_is_split_across_several() {
    let mut nodes: Vec<NodeCaps> = (0..4)
        .map(|i| cpu_node(&format!("n{i}"), 12000.0))
        .collect();
    nodes[2].has_model = true;
    let r = plan(&nodes, &big(), &want(Mode::Auto));
    assert!(r.error.is_none(), "{:?}", r.error);
    assert_eq!(r.instances.len(), 1);
    let p = &r.instances[0];
    assert_eq!(p.kind, "pipeline");
    assert_eq!(layers_total(p), 80);
    // Layers are contiguous, starting at the master.
    let mut next = 0;
    for s in &p.stages {
        assert_eq!(s.layer_from, next);
        next += s.layers;
    }
    // The device that already has the file is the master, so nothing needs copying.
    assert_eq!(p.master, "n2");
    assert!(!p.needs_fetch);
    assert!(p.stages[0].is_master);
    assert_eq!(p.rpc_nodes.len(), p.stages.len() - 1);
    // No device is asked to hold more than it has.
    for s in &p.stages {
        assert!(s.mem_mb as f64 <= 12000.0, "{} holds {}", s.name, s.mem_mb);
    }
    assert_eq!(p.pin_output, "CPU");
    // ngl counts the offloaded layers plus the output layer.
    assert_eq!(p.ngl, Some(80 - p.stages[0].layers + 1));
}

#[test]
fn a_model_that_fits_nowhere_says_how_much_is_missing() {
    let nodes = vec![cpu_node("a", 12000.0), cpu_node("b", 12000.0)];
    let r = plan(&nodes, &big(), &want(Mode::Auto));
    assert!(r.instances.is_empty());
    let e = r.error.unwrap();
    assert!(e.contains("GB") && e.contains("Add a device"), "{e}");
}

#[test]
fn devices_that_are_busy_draining_or_missing_the_engine_are_left_out() {
    let mut busy = cpu_node("busy", 12000.0);
    busy.available = false;
    let mut noeng = cpu_node("noeng", 12000.0);
    noeng.engine_ok = false;
    let r = plan(&[busy, noeng], &small(), &want(Mode::Auto));
    assert!(r.error.unwrap().contains("No device is free"));
}

#[test]
fn leftover_devices_are_reported_not_forced_into_a_pipeline() {
    // Two devices run the small model; a third is too small for it and alone cannot pair up.
    let nodes = vec![
        cpu_node("a", 12000.0),
        cpu_node("b", 12000.0),
        cpu_node("tiny", 2000.0),
    ];
    let r = plan(&nodes, &small(), &want(Mode::Auto));
    assert_eq!(r.instances.len(), 2);
    assert!(r.notes.iter().any(|n| n.contains("TINY")), "{:?}", r.notes);
}

#[test]
fn pipeline_mode_spreads_even_a_small_model_over_devices() {
    let nodes = vec![cpu_node("a", 12000.0), cpu_node("b", 12000.0)];
    let r = plan(&nodes, &small(), &want(Mode::Pipeline));
    assert_eq!(r.instances.len(), 1);
    assert_eq!(r.instances[0].kind, "pipeline");
}

#[test]
fn replica_mode_refuses_a_model_that_needs_a_pipeline() {
    let nodes: Vec<NodeCaps> = (0..4)
        .map(|i| cpu_node(&format!("n{i}"), 12000.0))
        .collect();
    let r = plan(&nodes, &big(), &want(Mode::Replicas));
    assert!(r.error.unwrap().contains("pipeline mode"));
}

#[test]
fn a_slow_far_away_device_is_not_dragged_into_the_chain() {
    // Two GPU boxes can hold the model together. A third, CPU-only box 60 ms away would only slow it.
    let model = ModelSpec {
        file: "m.gguf".into(),
        size_mb: 20000.0,
        n_layer: 40,
    };
    let a = gpu_node("a", 14000.0, 1500.0);
    let b = gpu_node("b", 14000.0, 1500.0);
    let mut c = cpu_node("c", 16000.0);
    c.rtt.insert("a".into(), 60.0);
    let r = plan(&[a, b, c], &model, &want(Mode::Pipeline));
    assert_eq!(r.instances.len(), 1, "{:?}", r.error);
    let ids: Vec<&str> = r.instances[0]
        .stages
        .iter()
        .map(|s| s.node_id.as_str())
        .collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(!ids.contains(&"c"));
    assert!(r.instances[0].stages.iter().all(|s| s.device == "gpu"));
}

#[test]
fn a_gpu_master_keeps_the_output_matrix_and_lists_its_gpu_first_in_the_split() {
    let model = ModelSpec {
        file: "m.gguf".into(),
        size_mb: 20000.0,
        n_layer: 40,
    };
    let mut a = gpu_node("a", 14000.0, 1500.0);
    a.has_model = true;
    let b = gpu_node("b", 14000.0, 1500.0);
    let r = plan(&[a, b], &model, &want(Mode::Auto));
    let p = &r.instances[0];
    assert_eq!(p.kind, "pipeline");
    assert_eq!(p.master, "a");
    assert_eq!(p.pin_output, "CUDA0");
    // One local GPU plus one remote worker.
    assert_eq!(p.tensor_split.len(), 2);
    let sum: f64 = p.tensor_split.iter().sum();
    assert!((sum - 1.0).abs() < 0.01, "{sum}");
    assert_eq!(layers_total(p), 40);
}

#[test]
fn a_fast_device_takes_as_many_layers_as_it_can_hold() {
    let model = ModelSpec {
        file: "m.gguf".into(),
        size_mb: 20000.0,
        n_layer: 40,
    };
    // A fast GPU that holds most of the model, and a slow CPU box for the rest.
    let fast = gpu_node("fast", 16000.0, 500.0);
    let slow = cpu_node("slow", 12000.0);
    let r = plan(&[fast, slow], &model, &want(Mode::Pipeline));
    let p = &r.instances[0];
    let fast_layers = p
        .stages
        .iter()
        .find(|s| s.node_id == "fast")
        .unwrap()
        .layers;
    let slow_layers = p
        .stages
        .iter()
        .find(|s| s.node_id == "slow")
        .unwrap()
        .layers;
    assert!(fast_layers > slow_layers, "{fast_layers} vs {slow_layers}");
    assert_eq!(fast_layers + slow_layers, 40);
}

#[test]
fn the_number_of_instances_can_be_capped() {
    let nodes: Vec<NodeCaps> = (0..4)
        .map(|i| cpu_node(&format!("n{i}"), 12000.0))
        .collect();
    let mut w = want(Mode::Auto);
    w.max_instances = 2;
    let r = plan(&nodes, &small(), &w);
    assert_eq!(r.instances.len(), 2);
}

#[test]
fn context_size_changes_what_fits() {
    // 7B at 4k context fits in ~5.4 GB; at 128k the KV cache alone is ~16 GB.
    let node = cpu_node("a", 8000.0);
    let mut w = want(Mode::Auto);
    assert!(plan(std::slice::from_ref(&node), &small(), &w)
        .error
        .is_none());
    w.ctx = 131_072;
    assert!(plan(&[node], &small(), &w).error.is_some());
}

#[test]
fn a_plan_becomes_an_instance_spec() {
    let nodes = vec![gpu_node("a", 9000.0, 4000.0)];
    let r = plan(&nodes, &small(), &want(Mode::Auto));
    let s = r.instances[0].to_spec("i1", "d1", "small.gguf", &want(Mode::Auto), 8);
    assert_eq!(
        (s.id.as_str(), s.deployment.as_str(), s.model.as_str()),
        ("i1", "d1", "small.gguf")
    );
    assert_eq!((s.ctx, s.parallel, s.threads), (4096, 2, 8));
    assert_eq!(s.kind, "replica");
    assert_eq!(s.stages.len(), 1);
}

#[test]
fn guesses_for_gpu_speed_follow_the_card() {
    assert!(guess_gpu_gbps("NVIDIA GeForce RTX 5060 Ti", 16000) > 300.0);
    assert!(
        guess_gpu_gbps("NVIDIA GeForce RTX 4090", 24000)
            > guess_gpu_gbps("NVIDIA GeForce RTX 3060", 12000)
    );
    assert_eq!(guess_gpu_gbps("Mystery", 6000), 150.0);
}

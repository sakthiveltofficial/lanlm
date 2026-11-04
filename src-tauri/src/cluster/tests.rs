use super::*;
use std::path::PathBuf;

fn tmp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("lanlm-test-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn put_key(b: &mut Vec<u8>, k: &str) {
    b.extend((k.len() as u64).to_le_bytes());
    b.extend(k.as_bytes());
}
fn put_str(b: &mut Vec<u8>, s: &str) {
    b.extend((s.len() as u64).to_le_bytes());
    b.extend(s.as_bytes());
}

/// A minimal GGUF header: optionally a string array first (like a tokenizer), then
/// the architecture and block count in the requested order.
fn gguf_header(arch: &str, blocks: u32, with_array: bool, blocks_first: bool) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(b"GGUF");
    b.extend(3u32.to_le_bytes());
    b.extend(0u64.to_le_bytes());
    b.extend((2u64 + u64::from(with_array)).to_le_bytes());
    if with_array {
        put_key(&mut b, "tokenizer.ggml.tokens");
        b.extend(9u32.to_le_bytes());
        b.extend(8u32.to_le_bytes());
        b.extend(3u64.to_le_bytes());
        for t in ["a", "bb", "ccc"] {
            put_str(&mut b, t);
        }
    }
    let arch_kv = |b: &mut Vec<u8>| {
        put_key(b, "general.architecture");
        b.extend(8u32.to_le_bytes());
        put_str(b, arch);
    };
    let blocks_kv = |b: &mut Vec<u8>| {
        put_key(b, &format!("{arch}.block_count"));
        b.extend(4u32.to_le_bytes());
        b.extend(blocks.to_le_bytes());
    };
    if blocks_first {
        blocks_kv(&mut b);
        arch_kv(&mut b);
    } else {
        arch_kv(&mut b);
        blocks_kv(&mut b);
    }
    b
}

fn worker(endpoint: &str, ram_mb: u64) -> WorkerCfg {
    WorkerCfg {
        endpoint: endpoint.into(),
        ram_mb,
    }
}

/// The planner on plain numbers: no model file (or disk space) needed.
fn plan_for(
    file_mb: f64,
    n_layer: u32,
    ws: &[WorkerCfg],
    force: bool,
    master_cap_mb: f64,
) -> Result<Plan, String> {
    if ws.is_empty() {
        return Ok(Plan {
            args: vec![],
            note: String::new(),
        });
    }
    plan_from_split(split_sizes(file_mb, n_layer, ws, force, master_cap_mb), ws)
}

fn arg_after<'a>(args: &'a [String], flag: &str) -> &'a str {
    let i = args
        .iter()
        .position(|a| a == flag)
        .unwrap_or_else(|| panic!("missing {flag} in {args:?}"));
    &args[i + 1]
}

// ───────── GGUF header parsing ─────────

#[test]
fn reads_block_count_in_either_key_order_and_skips_arrays() {
    let dir = tmp_dir("gguf");
    for (with_array, blocks_first) in [(false, false), (true, false), (true, true), (false, true)] {
        let p = dir.join("m.gguf");
        std::fs::write(&p, gguf_header("qwen3", 28, with_array, blocks_first)).unwrap();
        assert_eq!(
            block_count(&p).unwrap(),
            28,
            "array={with_array} blocks_first={blocks_first}"
        );
    }
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn rejects_files_that_are_not_gguf() {
    let dir = tmp_dir("notgguf");
    let p = dir.join("x.gguf");
    std::fs::write(&p, b"this is not a model").unwrap();
    assert!(block_count(&p).unwrap_err().contains("Not a GGUF"));
    assert!(block_count(&dir.join("missing.gguf")).is_err());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn errors_when_the_header_has_no_block_count() {
    let dir = tmp_dir("noblocks");
    let mut b = Vec::new();
    b.extend(b"GGUF");
    b.extend(3u32.to_le_bytes());
    b.extend(0u64.to_le_bytes());
    b.extend(1u64.to_le_bytes());
    put_key(&mut b, "general.architecture");
    b.extend(8u32.to_le_bytes());
    put_str(&mut b, "llama");
    let p = dir.join("m.gguf");
    std::fs::write(&p, b).unwrap();
    assert!(block_count(&p).is_err());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn split_reads_the_layer_count_from_a_real_file() {
    let dir = tmp_dir("splitfile");
    let p = dir.join("tiny.gguf");
    std::fs::write(&p, gguf_header("llama", 16, true, false)).unwrap();
    let s = split(&p, &[worker("10.0.0.2:50052", 4096)], false, 4000.0).unwrap();
    assert_eq!(s.n_layer, 16);
    assert!(!s.uses_workers, "a tiny file fits on this PC");
    std::fs::remove_dir_all(dir).ok();
}

// ───────── dealing layers ─────────

#[test]
fn spread_always_deals_out_every_layer() {
    for total in [0u32, 1, 7, 16, 33] {
        for weights in [
            vec![1.0],
            vec![1.0, 1.0],
            vec![1536.0, 3072.0],
            vec![5.0, 3.0, 2.0],
            vec![0.0, 4.0],
        ] {
            let out = spread(total, &weights);
            assert_eq!(
                out.iter().sum::<u32>(),
                total,
                "total={total} weights={weights:?}"
            );
            assert_eq!(out.len(), weights.len());
        }
    }
}

#[test]
fn spread_is_proportional_and_handles_degenerate_weights() {
    assert_eq!(spread(12, &[1.0, 2.0]), vec![4, 8]);
    assert_eq!(spread(10, &[]), Vec::<u32>::new());
    assert_eq!(spread(10, &[0.0, 0.0]), vec![0, 0]);
}

// ───────── the planner ─────────

#[test]
fn no_workers_means_no_extra_arguments() {
    let plan = plan_for(800.0, 16, &[], false, 4000.0).unwrap();
    assert!(plan.args.is_empty());
}

#[test]
fn a_model_that_fits_here_stays_here() {
    let plan = plan_for(800.0, 16, &[worker("10.0.0.2:50052", 4096)], false, 4000.0).unwrap();
    assert!(plan.args.is_empty());
    assert!(plan.note.contains("fits on this PC"));
}

#[test]
fn a_model_too_big_for_everyone_is_refused_with_a_clear_message() {
    let err = plan_for(9000.0, 16, &[worker("10.0.0.2:50052", 2048)], false, 1000.0)
        .err()
        .expect("should be refused");
    assert!(err.contains("needs about"), "{err}");
}

#[test]
fn a_big_model_is_split_with_the_master_filling_first() {
    // need = 3300 MB; the master offers 1000 -> 5 of 16 layers stay; the worker lends 4096 * 0.75.
    let plan = plan_for(3000.0, 16, &[worker("10.0.0.2:50052", 4096)], false, 1000.0).unwrap();
    assert_eq!(arg_after(&plan.args, "--rpc"), "10.0.0.2:50052");
    assert_eq!(arg_after(&plan.args, "-ngl"), "12"); // 16 + 1 - 5
    assert_eq!(arg_after(&plan.args, "-ts"), "1.000");
    assert!(
        arg_after(&plan.args, "-ot").contains("output|token_embd"),
        "the output layer must stay on the master"
    );
    assert!(
        plan.note.contains("5 of 16 layers on this PC"),
        "{}",
        plan.note
    );
}

#[test]
fn force_share_spreads_a_small_model_over_every_pc() {
    let plan = plan_for(800.0, 16, &[worker("10.0.0.2:50052", 4096)], true, 1000.0).unwrap();
    assert_eq!(arg_after(&plan.args, "-ngl"), "13"); // 4 layers stay: 16 + 1 - 4
}

#[test]
fn several_workers_get_tensor_split_weights_in_order() {
    let ws = [
        worker("10.0.0.2:50052", 2048),
        worker("10.0.0.3:50052", 4096),
    ];
    let plan = plan_for(3000.0, 16, &ws, false, 800.0).unwrap();
    assert_eq!(
        arg_after(&plan.args, "--rpc"),
        "10.0.0.2:50052,10.0.0.3:50052"
    );
    assert_eq!(arg_after(&plan.args, "-ts"), "0.333,0.667");
}

#[test]
fn split_never_loses_or_invents_layers() {
    let ws = [
        worker("a:1111", 3000),
        worker("b:2222", 5000),
        worker("c:3333", 2000),
    ];
    for n_layer in [1u32, 2, 16, 32, 80] {
        for force in [false, true] {
            for master_cap in [200.0, 1500.0, 9000.0] {
                let s = split_sizes(6000.0, n_layer, &ws, force, master_cap);
                assert_eq!(
                    s.local_layers + s.remote_layers.iter().sum::<u32>(),
                    s.n_layer,
                    "layers={n_layer} force={force} master={master_cap}"
                );
            }
        }
    }
}

#[test]
fn the_output_layer_always_stays_on_the_master() {
    // llama.cpp counts the output layer as one extra "layer"; it must not be sent to a worker.
    let plan = plan_for(3000.0, 28, &[worker("10.0.0.2:50052", 8192)], false, 500.0).unwrap();
    let ngl: u32 = arg_after(&plan.args, "-ngl").parse().unwrap();
    assert!(ngl <= 28 + 1);
    assert!(arg_after(&plan.args, "-ot").ends_with("=CPU"));
}

#[test]
fn probe_sees_open_and_closed_ports() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    assert!(probe(&addr));
    drop(listener);
    assert!(!probe(&addr));
    assert!(!probe("not a real endpoint"));
}

use super::*;
use std::collections::HashSet;

#[test]
fn safe_name_accepts_plain_gguf_names() {
    for ok in [
        "model.gguf",
        "Llama-3.2-1B-Instruct-Q4_K_M.gguf",
        "A.GGUF",
        "m.mmproj.gguf",
    ] {
        assert_eq!(safe_name(ok).unwrap(), ok);
    }
}

#[test]
fn safe_name_blocks_anything_that_could_escape_the_models_folder() {
    for bad in [
        "",
        "model.bin",
        "..\\x.gguf",
        "../x.gguf",
        "a/b.gguf",
        "a\\b.gguf",
        "C:evil.gguf",
        "x..gguf",
        "nul\0.gguf",
        ".gguf.exe",
    ] {
        assert!(safe_name(bad).is_err(), "{bad:?} should be rejected");
    }
}

#[test]
fn proj_name_sits_next_to_the_model() {
    assert_eq!(proj_name("qwen.gguf"), "qwen.mmproj.gguf");
    assert_eq!(
        proj_name("Qwen2-VL-2B-Q4_K_M.GGUF"),
        "Qwen2-VL-2B-Q4_K_M.mmproj.gguf"
    );
    assert!(safe_name(&proj_name("qwen.gguf")).is_ok());
}

#[test]
fn catalog_entries_are_well_formed() {
    let cat = catalog();
    assert!(cat.len() >= 8);
    let mut ids = HashSet::new();
    let mut files = HashSet::new();
    for c in &cat {
        assert!(ids.insert(c.id), "duplicate id {}", c.id);
        assert!(files.insert(c.file), "duplicate file {}", c.file);
        assert!(safe_name(c.file).is_ok(), "unsafe file name {}", c.file);
        assert!(
            c.url.starts_with("https://huggingface.co/") && c.url.ends_with(c.file),
            "bad url {}",
            c.url
        );
        assert!(c.size_mb > 100, "{} has an implausible size", c.id);
        assert!(!c.name.is_empty() && !c.desc.is_empty());
    }
}

#[test]
fn vision_entries_carry_a_projector_and_text_models_do_not() {
    let cat = catalog();
    assert!(
        cat.iter().any(|c| c.vision),
        "the catalog should offer at least one vision model"
    );
    for c in &cat {
        if c.vision {
            let url = c
                .proj_url
                .as_ref()
                .expect("a vision model needs a projector url");
            assert!(
                url.starts_with("https://huggingface.co/") && url.contains("mmproj"),
                "{url}"
            );
            assert!(c.proj_file.as_ref().unwrap().ends_with(".mmproj.gguf"));
            assert!(c.proj_mb > 0);
        } else {
            assert!(c.proj_url.is_none() && c.proj_file.is_none() && c.proj_mb == 0);
        }
    }
}

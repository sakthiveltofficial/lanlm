//! Reading just enough of a GGUF file's header to plan with: the layer count and the context
//! length the model was trained for. Large arrays (the tokenizer) are skipped, not loaded.

use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GgufInfo {
    pub arch: String,
    /// Number of transformer blocks.
    pub n_layer: u32,
    /// Context length the model was trained for, if the header says.
    pub ctx_train: Option<u32>,
}

fn rd<const N: usize, R: Read>(r: &mut R) -> std::io::Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact(&mut b)?;
    Ok(b)
}

fn rd_u32<R: Read>(r: &mut R) -> std::io::Result<u32> {
    Ok(u32::from_le_bytes(rd(r)?))
}

fn rd_u64<R: Read>(r: &mut R) -> std::io::Result<u64> {
    Ok(u64::from_le_bytes(rd(r)?))
}

fn scalar_size(t: u32) -> Option<i64> {
    Some(match t {
        0 | 1 | 7 => 1,
        2 | 3 => 2,
        4..=6 => 4,
        10..=12 => 8,
        _ => return None,
    })
}

fn skip_value<R: Read + Seek>(r: &mut R, t: u32) -> std::io::Result<()> {
    if let Some(n) = scalar_size(t) {
        r.seek(SeekFrom::Current(n))?;
    } else if t == 8 {
        let len = rd_u64(r)?;
        r.seek(SeekFrom::Current(len as i64))?;
    } else if t == 9 {
        let et = rd_u32(r)?;
        let n = rd_u64(r)?;
        if let Some(sz) = scalar_size(et) {
            r.seek(SeekFrom::Current(sz * n as i64))?;
        } else {
            for _ in 0..n {
                skip_value(r, et)?;
            }
        }
    } else {
        return Err(std::io::Error::other("unknown GGUF value type"));
    }
    Ok(())
}

/// Reads the header of a GGUF file.
pub fn read(path: &Path) -> Result<GgufInfo, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut r = BufReader::with_capacity(1 << 16, f);
    let io = |e: std::io::Error| format!("Could not read the model header: {e}");
    if &rd::<4, _>(&mut r).map_err(io)? != b"GGUF" {
        return Err("Not a GGUF file".into());
    }
    let _version = rd_u32(&mut r).map_err(io)?;
    let _tensors = rd_u64(&mut r).map_err(io)?;
    let kv = rd_u64(&mut r).map_err(io)?;

    let mut info = GgufInfo::default();
    let mut blocks: Vec<(String, u32)> = Vec::new();
    let mut ctxs: Vec<(String, u32)> = Vec::new();
    for _ in 0..kv {
        let klen = rd_u64(&mut r).map_err(io)? as usize;
        if klen > 4096 {
            return Err("The model header is damaged".into());
        }
        let mut key = vec![0u8; klen];
        r.read_exact(&mut key).map_err(io)?;
        let key = String::from_utf8_lossy(&key).to_string();
        let t = rd_u32(&mut r).map_err(io)?;
        if key == "general.architecture" && t == 8 {
            let l = rd_u64(&mut r).map_err(io)? as usize;
            if l > 4096 {
                return Err("The model header is damaged".into());
            }
            let mut s = vec![0u8; l];
            r.read_exact(&mut s).map_err(io)?;
            info.arch = String::from_utf8_lossy(&s).to_string();
        } else if (t == 4 || t == 5) && key.ends_with(".block_count") {
            blocks.push((key, rd_u32(&mut r).map_err(io)?));
        } else if (t == 4 || t == 5) && key.ends_with(".context_length") {
            ctxs.push((key, rd_u32(&mut r).map_err(io)?));
        } else {
            skip_value(&mut r, t).map_err(io)?;
        }
        if !info.arch.is_empty() {
            let want_b = format!("{}.block_count", info.arch);
            let want_c = format!("{}.context_length", info.arch);
            if let Some((_, n)) = blocks.iter().find(|(k, _)| *k == want_b) {
                info.n_layer = *n;
                info.ctx_train = ctxs.iter().find(|(k, _)| *k == want_c).map(|(_, v)| *v);
                // The context length usually comes right after; stop once we have both or the
                // keys we need can no longer appear soon.
                if info.ctx_train.is_some() {
                    return Ok(info);
                }
            }
        }
    }
    if info.n_layer > 0 {
        return Ok(info);
    }
    Err("The model header has no block_count".into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn s(buf: &mut Vec<u8>, text: &str) {
        buf.extend_from_slice(&(text.len() as u64).to_le_bytes());
        buf.extend_from_slice(text.as_bytes());
    }

    /// A minimal GGUF header with a big tokenizer array in front, as real files have.
    pub fn fake_gguf(arch: &str, layers: u32, ctx: Option<u32>) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"GGUF");
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes());
        let kvs = 3 + u64::from(ctx.is_some());
        b.extend_from_slice(&kvs.to_le_bytes());
        s(&mut b, "general.architecture");
        b.extend_from_slice(&8u32.to_le_bytes());
        s(&mut b, arch);
        // A string array like the vocabulary.
        s(&mut b, "tokenizer.ggml.tokens");
        b.extend_from_slice(&9u32.to_le_bytes());
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&3u64.to_le_bytes());
        for t in ["a", "bb", "ccc"] {
            s(&mut b, t);
        }
        s(&mut b, &format!("{arch}.block_count"));
        b.extend_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&layers.to_le_bytes());
        if let Some(c) = ctx {
            s(&mut b, &format!("{arch}.context_length"));
            b.extend_from_slice(&4u32.to_le_bytes());
            b.extend_from_slice(&c.to_le_bytes());
        }
        b
    }

    fn write(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("lanlm-gguf-{}-{name}", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn reads_layers_and_context_past_a_tokenizer_array() {
        let p = write("a.gguf", &fake_gguf("llama", 32, Some(8192)));
        let i = read(&p).unwrap();
        assert_eq!(
            i,
            GgufInfo {
                arch: "llama".into(),
                n_layer: 32,
                ctx_train: Some(8192)
            }
        );
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_header_without_a_context_length_still_gives_the_layers() {
        let p = write("b.gguf", &fake_gguf("qwen2", 28, None));
        let i = read(&p).unwrap();
        assert_eq!((i.n_layer, i.ctx_train), (28, None));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn rejects_other_files_and_truncated_headers() {
        let p = write("c.gguf", b"not a model at all");
        assert!(read(&p).is_err());
        let mut cut = fake_gguf("llama", 32, Some(1));
        cut.truncate(60);
        let q = write("d.gguf", &cut);
        assert!(read(&q).is_err());
        assert!(read(Path::new("/definitely/not/here.gguf")).is_err());
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(q);
    }
}

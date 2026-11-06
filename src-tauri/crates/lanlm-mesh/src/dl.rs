use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

/// Download `url` to `dest`, resuming from `dest.part` when the server allows it.
/// `is_cancelled` is polled between chunks; a cancelled download keeps its
/// `.part` file so it can be resumed later. `progress(done, total)` is
/// throttled to about ten calls a second.
pub async fn download(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    is_cancelled: impl Fn() -> bool,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let part: PathBuf = {
        let mut s = dest.as_os_str().to_owned();
        s.push(".part");
        PathBuf::from(s)
    };
    let existing = tokio::fs::metadata(&part)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    let mut req = client.get(url);
    if existing > 0 {
        req = req.header("Range", format!("bytes={existing}-"));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("Request failed: {e}"))?;
    let status = resp.status();

    // The .part file already holds the whole file.
    if status.as_u16() == 416 {
        tokio::fs::rename(&part, dest)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    if !status.is_success() {
        return Err(format!("Server answered {status}"));
    }

    let resumed = status.as_u16() == 206;
    let start = if resumed { existing } else { 0 };
    let total = resp.content_length().map(|l| l + start).unwrap_or(0);

    let mut file = if resumed {
        tokio::fs::OpenOptions::new().append(true).open(&part).await
    } else {
        tokio::fs::File::create(&part).await
    }
    .map_err(|e| e.to_string())?;

    let mut done = start;
    let mut last = Instant::now();
    let mut stream = resp.bytes_stream();
    progress(done, total);
    while let Some(chunk) = stream.next().await {
        if is_cancelled() {
            let _ = file.flush().await;
            return Err("cancelled".into());
        }
        let chunk = chunk.map_err(|e| format!("Download interrupted: {e}"))?;
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        done += chunk.len() as u64;
        if last.elapsed() >= Duration::from_millis(100) {
            progress(done, total);
            last = Instant::now();
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    progress(done, total.max(done));
    tokio::fs::rename(&part, dest)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests;

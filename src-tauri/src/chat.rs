use std::sync::atomic::Ordering;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::ipc::Channel;
use tauri::State;

use crate::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReq {
    /// OpenAI-style messages: [{ role, content }]
    messages: Vec<Value>,
    temperature: f32,
    max_tokens: i32,
}

#[derive(Clone, Serialize)]
#[serde(
    tag = "event",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChatEvent {
    Delta {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Stats {
        prompt_tps: f64,
        gen_tps: f64,
        tokens: u64,
        context_tokens: u64,
    },
    Done,
}

/// Streams a reply from the running llama-server (OpenAI chat API) to the UI.
/// Going through Rust avoids any browser CORS issue with the local server.
#[tauri::command]
pub async fn chat_stream(
    state: State<'_, AppState>,
    req: ChatReq,
    on_event: Channel<ChatEvent>,
) -> Result<(), String> {
    let (port, key) = state
        .server
        .lock()
        .await
        .as_ref()
        .map(|s| (s.port, s.key.clone()))
        .ok_or("No model is loaded")?;
    state.chat_cancel.store(false, Ordering::SeqCst);

    let body = json!({
        "messages": req.messages,
        "temperature": req.temperature,
        "max_tokens": req.max_tokens,
        "stream": true,
    });
    let mut req_builder = state
        .http
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&body);
    if let Some(k) = key {
        req_builder = req_builder.bearer_auth(k);
    }
    let resp = req_builder
        .send()
        .await
        .map_err(|e| format!("Could not reach llama-server: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("llama-server answered {status}: {text}"));
    }

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    'outer: while let Some(chunk) = stream.next().await {
        if state.chat_cancel.load(Ordering::SeqCst) {
            break; // dropping the stream closes the connection and stops generation
        }
        let chunk = chunk.map_err(|e| format!("Stream interrupted: {e}"))?;
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let Some(data) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                break 'outer;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            let delta = &v["choices"][0]["delta"];
            if let Some(t) = delta["reasoning_content"]
                .as_str()
                .filter(|t| !t.is_empty())
            {
                let _ = on_event.send(ChatEvent::Reasoning {
                    text: t.to_string(),
                });
            }
            if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
                let _ = on_event.send(ChatEvent::Delta {
                    text: t.to_string(),
                });
            }
            if let Some(t) = v.get("timings") {
                // Tokens now held in the context: what was read (new + cached) plus what was written.
                let ctx = t["prompt_n"].as_u64().unwrap_or(0)
                    + t["cache_n"].as_u64().unwrap_or(0)
                    + t["predicted_n"].as_u64().unwrap_or(0);
                let _ = on_event.send(ChatEvent::Stats {
                    prompt_tps: t["prompt_per_second"].as_f64().unwrap_or(0.0),
                    gen_tps: t["predicted_per_second"].as_f64().unwrap_or(0.0),
                    tokens: t["predicted_n"].as_u64().unwrap_or(0),
                    context_tokens: ctx,
                });
            }
        }
    }
    let _ = on_event.send(ChatEvent::Done);
    Ok(())
}

#[tauri::command]
pub fn chat_cancel(state: State<'_, AppState>) {
    state.chat_cancel.store(true, Ordering::SeqCst);
}

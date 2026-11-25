//! A Telegram bot that answers from the cluster, so you can chat with your own PCs from a phone.
//!
//! It uses long polling (`getUpdates`), so it needs no public address, port forwarding or
//! webhook: it only makes outgoing HTTPS requests to Telegram. Each message is sent to an
//! OpenAI-compatible endpoint (the cluster's gateway, or the model loaded on this PC), which the
//! caller provides as a function.
//!
//! The bot is private: only Telegram users on the allow-list get answers. Anyone else is told
//! their user ID and shows up in the app as a pending request, so the owner can approve them.
//!
//! Messages pass through Telegram's servers, so the app must say so before it is turned on.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::watch;

/// Telegram refuses messages longer than 4096 characters.
pub const MAX_MESSAGE: usize = 4000;
/// How many earlier messages of a chat are sent with each question.
pub const HISTORY: usize = 20;
/// Pending requests kept for the owner to approve.
const MAX_PENDING: usize = 20;

/// What a person chose for the bot. Saved, so it comes back after a restart.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TelegramSettings {
    pub enabled: bool,
    /// The token BotFather gave, like `123456:ABC-DEF…`.
    pub token: String,
    /// Telegram user IDs allowed to use the bot.
    pub allowed: Vec<i64>,
    /// Instructions sent with every chat (empty: none).
    pub system: String,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PendingUser {
    pub id: i64,
    pub name: String,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct TelegramStatus {
    pub running: bool,
    /// The bot's @username once Telegram has confirmed the token.
    pub bot_name: Option<String>,
    pub error: Option<String>,
    pub pending: Vec<PendingUser>,
    /// Messages answered since the bot started.
    pub answered: u64,
}

/// Where to send a chat: `host:port` of an OpenAI-compatible server and its key, if any.
pub type Endpoint =
    Arc<dyn Fn() -> BoxFuture<'static, Result<(String, Option<String>), String>> + Send + Sync>;

/// A running bot. Dropping it does not stop it: call [`Bot::stop`].
pub struct Bot {
    shared: Arc<Shared>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

struct Shared {
    settings: Mutex<TelegramSettings>,
    status: Mutex<TelegramStatus>,
    /// Conversation per Telegram chat id.
    history: Mutex<HashMap<i64, Vec<Value>>>,
    /// Users already told they are not allowed, so they are told only once.
    told: Mutex<Vec<i64>>,
}

impl Bot {
    /// Starts polling Telegram. `api_base` is `https://api.telegram.org` (tests use a stand-in).
    pub fn start(
        http: reqwest::Client,
        settings: TelegramSettings,
        endpoint: Endpoint,
        api_base: String,
    ) -> Bot {
        let shared = Arc::new(Shared {
            settings: Mutex::new(settings),
            status: Mutex::new(TelegramStatus {
                running: true,
                ..Default::default()
            }),
            history: Mutex::new(HashMap::new()),
            told: Mutex::new(Vec::new()),
        });
        let (stop, stop_rx) = watch::channel(false);
        let task = tokio::spawn(poll_loop(http, shared.clone(), endpoint, api_base, stop_rx));
        Bot { shared, stop, task }
    }

    pub fn status(&self) -> TelegramStatus {
        self.shared.status.lock().unwrap().clone()
    }

    pub fn settings(&self) -> TelegramSettings {
        self.shared.settings.lock().unwrap().clone()
    }

    /// Lets a Telegram user in (or takes them out), without restarting the bot.
    pub fn set_allowed(&self, allowed: Vec<i64>) {
        let mut st = self.shared.status.lock().unwrap();
        st.pending.retain(|p| !allowed.contains(&p.id));
        self.shared
            .told
            .lock()
            .unwrap()
            .retain(|id| !allowed.contains(id));
        self.shared.settings.lock().unwrap().allowed = allowed;
    }

    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(3), self.task).await;
    }
}

// ───────────────────────────── pure helpers (tested) ─────────────────────────────

/// A message from Telegram that the bot should act on.
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    pub update_id: i64,
    pub chat_id: i64,
    pub user_id: i64,
    pub user_name: String,
    pub text: String,
    pub has_photo: bool,
}

/// Picks the messages out of a `getUpdates` answer. Edits, joins and other updates are skipped,
/// but their `update_id` still moves the offset on (see [`next_offset`]).
pub fn parse_updates(v: &Value) -> Vec<Incoming> {
    let Some(list) = v.get("result").and_then(Value::as_array) else {
        return vec![];
    };
    list.iter()
        .filter_map(|u| {
            let update_id = u.get("update_id")?.as_i64()?;
            let m = u.get("message")?;
            let chat_id = m.get("chat")?.get("id")?.as_i64()?;
            let from = m.get("from")?;
            let user_id = from.get("id")?.as_i64()?;
            let first = from.get("first_name").and_then(Value::as_str).unwrap_or("");
            let last = from.get("last_name").and_then(Value::as_str).unwrap_or("");
            let user = from.get("username").and_then(Value::as_str).unwrap_or("");
            let mut user_name = format!("{first} {last}").trim().to_string();
            if !user.is_empty() {
                user_name = if user_name.is_empty() {
                    format!("@{user}")
                } else {
                    format!("{user_name} (@{user})")
                };
            }
            let text = m
                .get("text")
                .or_else(|| m.get("caption"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let has_photo = m.get("photo").is_some();
            Some(Incoming {
                update_id,
                chat_id,
                user_id,
                user_name,
                text,
                has_photo,
            })
        })
        .collect()
}

/// The offset for the next `getUpdates`: one past the highest update seen.
pub fn next_offset(v: &Value, current: i64) -> i64 {
    v.get("result")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|u| u.get("update_id").and_then(Value::as_i64))
                .fold(current, |acc, id| acc.max(id + 1))
        })
        .unwrap_or(current)
}

#[derive(Debug, PartialEq)]
pub enum Cmd {
    Start,
    New,
    Status,
    Help,
    Ask(String),
}

/// Reads a message as a command or a question. `/new@MyBot` works too (group chats).
pub fn parse_command(text: &str) -> Cmd {
    let t = text.trim();
    if !t.starts_with('/') {
        return Cmd::Ask(t.to_string());
    }
    let word = t.split_whitespace().next().unwrap_or("");
    let name = word
        .trim_start_matches('/')
        .split('@')
        .next()
        .unwrap_or("")
        .to_lowercase();
    match name.as_str() {
        "start" => Cmd::Start,
        "new" | "reset" | "clear" => Cmd::New,
        "status" => Cmd::Status,
        "help" => Cmd::Help,
        _ => Cmd::Ask(t.to_string()),
    }
}

/// Splits a long reply into pieces Telegram accepts, preferring to break at paragraphs, then
/// lines, then spaces, and never inside a character.
pub fn split_message(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    while rest.chars().count() > max {
        // Byte index of the `max`-th character.
        let limit = rest
            .char_indices()
            .nth(max)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let head = &rest[..limit];
        let cut = head
            .rfind("\n\n")
            .or_else(|| head.rfind('\n'))
            .or_else(|| head.rfind(' '))
            .filter(|&i| i > limit / 2)
            .unwrap_or(limit);
        out.push(rest[..cut].trim_end().to_string());
        rest = rest[cut..].trim_start();
    }
    if !rest.is_empty() {
        out.push(rest.to_string());
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Keeps the last `max` messages, never starting with an assistant reply.
pub fn trim_history(h: &mut Vec<Value>, max: usize) {
    if h.len() > max {
        h.drain(..h.len() - max);
    }
    while h
        .first()
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        == Some("assistant")
    {
        h.remove(0);
    }
}

/// The reply text from an OpenAI chat completion. Some models put their thinking in
/// `<think>…</think>` before the answer; that part is left out.
pub fn extract_reply(v: &Value) -> Option<String> {
    let content = v
        .get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()?;
    let answer = match content.find("</think>") {
        Some(i) => &content[i + "</think>".len()..],
        None => content,
    };
    Some(answer.trim().to_string())
}

/// Adds someone who is not allowed to the pending list (once, newest last, capped).
pub fn add_pending(list: &mut Vec<PendingUser>, id: i64, name: &str) {
    if list.iter().any(|p| p.id == id) {
        return;
    }
    list.push(PendingUser {
        id,
        name: name.to_string(),
    });
    if list.len() > MAX_PENDING {
        list.remove(0);
    }
}

/// A token looks like `<digits>:<35 or so url-safe characters>`.
pub fn token_looks_valid(token: &str) -> bool {
    let Some((id, secret)) = token.trim().split_once(':') else {
        return false;
    };
    !id.is_empty()
        && id.chars().all(|c| c.is_ascii_digit())
        && secret.len() >= 20
        && secret
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

// ───────────────────────────── the loop ─────────────────────────────

async fn poll_loop(
    http: reqwest::Client,
    shared: Arc<Shared>,
    endpoint: Endpoint,
    api_base: String,
    mut stop: watch::Receiver<bool>,
) {
    let token = shared.settings.lock().unwrap().token.trim().to_string();
    let base = format!("{}/bot{}", api_base.trim_end_matches('/'), token);
    let set_error = |e: Option<String>| shared.status.lock().unwrap().error = e;

    // Check the token first, so a wrong one is reported clearly.
    let mut wait = Duration::from_secs(2);
    loop {
        if *stop.borrow() {
            shared.status.lock().unwrap().running = false;
            return;
        }
        match call(&http, &base, "getMe", json!({}), Duration::from_secs(20)).await {
            Ok(v) => {
                let name = v
                    .get("result")
                    .and_then(|r| r.get("username"))
                    .and_then(Value::as_str)
                    .map(|s| format!("@{s}"));
                let mut st = shared.status.lock().unwrap();
                st.bot_name = name;
                st.error = None;
                break;
            }
            Err(e) => {
                set_error(Some(e));
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = stop.changed() => {}
                }
                wait = (wait * 2).min(Duration::from_secs(60));
            }
        }
    }

    let mut offset = 0i64;
    let mut wait = Duration::from_secs(1);
    loop {
        if *stop.borrow() {
            break;
        }
        let req = call(
            &http,
            &base,
            "getUpdates",
            json!({ "offset": offset, "timeout": 25, "allowed_updates": ["message"] }),
            Duration::from_secs(40),
        );
        let res = tokio::select! {
            r = req => r,
            _ = stop.changed() => break,
        };
        match res {
            Ok(v) => {
                wait = Duration::from_secs(1);
                set_error(None);
                offset = next_offset(&v, offset);
                for m in parse_updates(&v) {
                    let (http, shared, endpoint, base) =
                        (http.clone(), shared.clone(), endpoint.clone(), base.clone());
                    tokio::spawn(async move { handle(&http, &shared, &endpoint, &base, m).await });
                }
            }
            Err(e) => {
                set_error(Some(e));
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = stop.changed() => break,
                }
                wait = (wait * 2).min(Duration::from_secs(30));
            }
        }
    }
    shared.status.lock().unwrap().running = false;
}

async fn call(
    http: &reqwest::Client,
    base: &str,
    method: &str,
    body: Value,
    timeout: Duration,
) -> Result<Value, String> {
    let r = http
        .post(format!("{base}/{method}"))
        .json(&body)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| format!("Could not reach Telegram: {}", e.without_url()))?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(v);
    }
    let desc = v
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("no details");
    Err(match status.as_u16() {
        401 | 404 => {
            "Telegram did not accept the bot token. Copy it again from BotFather.".to_string()
        }
        409 => "Another program is already using this bot token. Stop it, or make a new bot."
            .to_string(),
        _ => format!("Telegram answered {status}: {desc}"),
    })
}

async fn send(http: &reqwest::Client, base: &str, chat_id: i64, text: &str) {
    for part in split_message(text, MAX_MESSAGE) {
        let _ = call(
            http,
            base,
            "sendMessage",
            json!({ "chat_id": chat_id, "text": part }),
            Duration::from_secs(20),
        )
        .await;
    }
}

const HELP: &str = "Ask me anything: I answer with the AI running on your own PCs (LanLM).\n\n/new  start a new conversation\n/status  is a model running?\n/help  this message";

async fn handle(
    http: &reqwest::Client,
    shared: &Shared,
    endpoint: &Endpoint,
    base: &str,
    m: Incoming,
) {
    let (allowed, system) = {
        let s = shared.settings.lock().unwrap();
        (s.allowed.contains(&m.user_id), s.system.clone())
    };
    if !allowed {
        add_pending(
            &mut shared.status.lock().unwrap().pending,
            m.user_id,
            &m.user_name,
        );
        let first_time = {
            let mut told = shared.told.lock().unwrap();
            let first = !told.contains(&m.user_id);
            if first {
                told.push(m.user_id);
            }
            first
        };
        if first_time {
            let text = format!(
                "This bot is private. Your Telegram user ID is {}.\nAsk the owner to approve it in LanLM: Settings, Telegram.",
                m.user_id
            );
            send(http, base, m.chat_id, &text).await;
        }
        return;
    }
    match parse_command(&m.text) {
        Cmd::Start | Cmd::Help => send(http, base, m.chat_id, HELP).await,
        Cmd::New => {
            shared.history.lock().unwrap().remove(&m.chat_id);
            send(http, base, m.chat_id, "Started a new conversation.").await;
        }
        Cmd::Status => {
            let text = match (endpoint)().await {
                Ok(_) => "A model is running and ready.".to_string(),
                Err(e) => e,
            };
            send(http, base, m.chat_id, &text).await;
        }
        Cmd::Ask(q) => {
            if q.is_empty() {
                let text = if m.has_photo {
                    "I can only read text for now."
                } else {
                    "Send me a question as text."
                };
                send(http, base, m.chat_id, text).await;
                return;
            }
            let _ = call(
                http,
                base,
                "sendChatAction",
                json!({ "chat_id": m.chat_id, "action": "typing" }),
                Duration::from_secs(10),
            )
            .await;
            let mut messages = {
                let mut h = shared.history.lock().unwrap();
                let conv = h.entry(m.chat_id).or_default();
                conv.push(json!({ "role": "user", "content": q }));
                trim_history(conv, HISTORY);
                conv.clone()
            };
            if !system.trim().is_empty() {
                messages.insert(0, json!({ "role": "system", "content": system }));
            }
            match ask(http, endpoint, messages).await {
                Ok(reply) => {
                    {
                        let mut h = shared.history.lock().unwrap();
                        let conv = h.entry(m.chat_id).or_default();
                        conv.push(json!({ "role": "assistant", "content": reply }));
                        trim_history(conv, HISTORY);
                    }
                    shared.status.lock().unwrap().answered += 1;
                    send(
                        http,
                        base,
                        m.chat_id,
                        if reply.is_empty() {
                            "(no answer)"
                        } else {
                            &reply
                        },
                    )
                    .await;
                }
                Err(e) => {
                    // Forget the question, so the conversation does not hold an unanswered turn.
                    if let Some(conv) = shared.history.lock().unwrap().get_mut(&m.chat_id) {
                        conv.pop();
                    }
                    send(
                        http,
                        base,
                        m.chat_id,
                        &format!("Sorry, I could not answer: {e}"),
                    )
                    .await;
                }
            }
        }
    }
}

async fn ask(
    http: &reqwest::Client,
    endpoint: &Endpoint,
    messages: Vec<Value>,
) -> Result<String, String> {
    let (addr, key) = (endpoint)().await?;
    let mut req = http
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&json!({ "messages": messages, "stream": false }))
        .timeout(Duration::from_secs(600));
    if let Some(k) = key {
        req = req.bearer_auth(k);
    }
    let r = req
        .send()
        .await
        .map_err(|e| format!("the model did not answer ({})", e.without_url()))?;
    let status = r.status();
    let v: Value = r.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let msg = v
            .get("error")
            .and_then(|e| e.get("message").or(Some(e)))
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(format!("{status}: {msg}"));
    }
    extract_reply(&v).ok_or_else(|| "the model sent an empty answer".to_string())
}

#[cfg(test)]
mod tests;

use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::Uri;
use axum::routing::any;
use axum::Router;

#[test]
fn parses_messages_and_skips_other_updates() {
    let v = json!({ "ok": true, "result": [
        { "update_id": 10, "message": { "chat": { "id": 5 }, "from": { "id": 7, "first_name": "Ada", "username": "ada" }, "text": "hi" } },
        { "update_id": 11, "edited_message": { "chat": { "id": 5 } } },
        { "update_id": 12, "message": { "chat": { "id": 6 }, "from": { "id": 8 }, "photo": [{}], "caption": "what is this" } },
    ]});
    let m = parse_updates(&v);
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].text, "hi");
    assert_eq!(m[0].user_name, "Ada (@ada)");
    assert_eq!(m[1].text, "what is this");
    assert!(m[1].has_photo);
    assert_eq!(next_offset(&v, 0), 13);
    assert_eq!(next_offset(&json!({ "ok": true, "result": [] }), 13), 13);
}

#[test]
fn reads_commands() {
    assert_eq!(parse_command("/start"), Cmd::Start);
    assert_eq!(parse_command("/new@LanBot"), Cmd::New);
    assert_eq!(parse_command(" /STATUS "), Cmd::Status);
    assert_eq!(parse_command("/help me"), Cmd::Help);
    assert_eq!(parse_command("/weird"), Cmd::Ask("/weird".into()));
    assert_eq!(parse_command(" hello "), Cmd::Ask("hello".into()));
}

#[test]
fn splits_long_replies_at_natural_breaks() {
    assert_eq!(split_message("short", 10), vec!["short"]);
    let text = format!("{}\n\n{}", "a".repeat(8), "b".repeat(8));
    assert_eq!(split_message(&text, 12), vec!["a".repeat(8), "b".repeat(8)]);
    // No break at all: cut hard, and never inside a multi-byte character.
    let parts = split_message(&"é".repeat(25), 10);
    assert_eq!(parts.len(), 3);
    assert!(parts.iter().all(|p| p.chars().count() <= 10));
    assert_eq!(parts.concat(), "é".repeat(25));
}

#[test]
fn history_is_capped_and_starts_with_the_user() {
    let mut h: Vec<Value> = (0..7)
        .map(|i| json!({ "role": if i % 2 == 0 { "user" } else { "assistant" }, "content": i.to_string() }))
        .collect();
    trim_history(&mut h, 4);
    assert_eq!(h.len(), 3);
    assert_eq!(h[0]["role"], "user");
}

#[test]
fn reply_leaves_out_thinking() {
    let v = json!({ "choices": [{ "message": { "content": "<think>hmm</think>\n\nHello!" } }] });
    assert_eq!(extract_reply(&v).as_deref(), Some("Hello!"));
    assert_eq!(extract_reply(&json!({})), None);
}

#[test]
fn pending_is_unique_and_capped() {
    let mut l = vec![];
    add_pending(&mut l, 1, "a");
    add_pending(&mut l, 1, "a");
    assert_eq!(l.len(), 1);
    for i in 2..40 {
        add_pending(&mut l, i, "x");
    }
    assert_eq!(l.len(), MAX_PENDING);
    assert_eq!(l.last().unwrap().id, 39);
}

#[test]
fn checks_token_shape() {
    assert!(token_looks_valid(
        "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw"
    ));
    assert!(!token_looks_valid("not a token"));
    assert!(!token_looks_valid("12a:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw"));
    assert!(!token_looks_valid("123:short"));
}

// ───────── end to end, with stand-ins for Telegram and the model ─────────

#[derive(Default)]
struct Fake {
    polls: AtomicUsize,
    sent: Mutex<Vec<(i64, String)>>,
}

async fn telegram(State(f): State<Arc<Fake>>, uri: Uri, body: Bytes) -> axum::Json<Value> {
    let method = uri.path().rsplit('/').next().unwrap_or("").to_string();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let res = match method.as_str() {
        "getMe" => json!({ "ok": true, "result": { "username": "lan_bot" } }),
        "getUpdates" => {
            let n = f.polls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                json!({ "ok": true, "result": [
                    { "update_id": 1, "message": { "chat": { "id": 100 }, "from": { "id": 42, "first_name": "Owner" }, "text": "What is 2+2?" } },
                    { "update_id": 2, "message": { "chat": { "id": 200 }, "from": { "id": 99, "first_name": "Stranger" }, "text": "hi" } },
                ]})
            } else {
                tokio::time::sleep(Duration::from_millis(100)).await;
                json!({ "ok": true, "result": [] })
            }
        }
        "sendMessage" => {
            f.sent.lock().unwrap().push((
                body["chat_id"].as_i64().unwrap_or(0),
                body["text"].as_str().unwrap_or("").to_string(),
            ));
            json!({ "ok": true, "result": {} })
        }
        _ => json!({ "ok": true, "result": true }),
    };
    axum::Json(res)
}

async fn model(body: Bytes) -> axum::Json<Value> {
    let v: Value = serde_json::from_slice(&body).unwrap();
    let last = v["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string();
    axum::Json(
        json!({ "choices": [{ "message": { "role": "assistant", "content": format!("You asked: {last}") } }] }),
    )
}

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    addr.to_string()
}

#[tokio::test]
async fn answers_allowed_users_and_queues_strangers() {
    let fake = Arc::new(Fake::default());
    let tg = serve(
        Router::new()
            .fallback(any(telegram))
            .with_state(fake.clone()),
    )
    .await;
    let llm = serve(Router::new().fallback(any(model))).await;
    let endpoint: Endpoint = Arc::new(move || {
        let llm = llm.clone();
        Box::pin(async move { Ok((llm, None)) })
    });
    let settings = TelegramSettings {
        enabled: true,
        token: "1:abc".into(),
        allowed: vec![42],
        system: "Be brief.".into(),
    };
    let bot = Bot::start(
        reqwest::Client::new(),
        settings,
        endpoint,
        format!("http://{tg}"),
    );

    let mut ok = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if fake.sent.lock().unwrap().len() >= 2 {
            ok = true;
            break;
        }
    }
    assert!(
        ok,
        "the bot did not answer: {:?}",
        fake.sent.lock().unwrap()
    );
    let sent = fake.sent.lock().unwrap().clone();
    assert!(sent.contains(&(100, "You asked: What is 2+2?".to_string())));
    assert!(sent.iter().any(|(c, t)| *c == 200 && t.contains("99")));

    let st = bot.status();
    assert_eq!(st.bot_name.as_deref(), Some("@lan_bot"));
    assert_eq!(st.answered, 1);
    assert_eq!(
        st.pending,
        vec![PendingUser {
            id: 99,
            name: "Stranger".into()
        }]
    );

    bot.set_allowed(vec![42, 99]);
    assert!(bot.status().pending.is_empty());
    bot.stop().await;
}

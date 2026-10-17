//! Адаптер Telegram против заглушки Bot API: настоящий `bot::run` получает апдейты
//! (текст, фото с загрузкой файла, нажатие кнопки) и отправляет ответы. Токен и сеть не нужны.

use rentflow::app::{App, Config};
use rentflow::db;
use rentflow::ocr::NoExtractor;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use teloxide::prelude::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TOKEN: &str = "123:abc";

#[derive(Default)]
struct Log {
    sent: Vec<Value>,
    paths: Vec<String>,
}

fn user(id: i64) -> Value {
    json!({"id": id, "is_bot": false, "first_name": "Test"})
}

fn chat(id: i64) -> Value {
    json!({"id": id, "type": "private", "first_name": "Test"})
}

fn message(id: i64, extra: Value) -> Value {
    message_with_id(id, 10, extra)
}

fn message_with_id(id: i64, message_id: i64, extra: Value) -> Value {
    let mut m =
        json!({"message_id": message_id, "date": 1700000000, "chat": chat(id), "from": user(id)});
    m.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    m
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = sock.read(&mut chunk).await.unwrap_or(0);
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some(h) = text.find("\r\n\r\n") {
            let len = text[..h]
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if buf.len() >= h + 4 + len {
                let path = text
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_string();
                return (path, buf[h + 4..h + 4 + len].to_vec());
            }
        }
        if n == 0 {
            return (String::new(), vec![]);
        }
    }
}

async fn respond(sock: &mut tokio::net::TcpStream, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    sock.write_all(head.as_bytes()).await.unwrap();
    sock.write_all(body).await.unwrap();
}

async fn start_stub(updates: Vec<Value>) -> (String, Arc<Mutex<Log>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Log::default()));
    let log2 = log.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (updates, log) = (updates.clone(), log2.clone());
            tokio::spawn(async move {
                let (path, body) = read_request(&mut sock).await;
                log.lock().unwrap().paths.push(path.clone());
                let body_json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                if path.starts_with(&format!("/file/bot{TOKEN}/")) {
                    return respond(&mut sock, "application/octet-stream", &[1, 2, 3]).await;
                }
                let method = path.rsplit('/').next().unwrap_or("").to_lowercase();
                let result = match method.as_str() {
                    "getwebhookinfo" => {
                        json!({"url": "", "has_custom_certificate": false, "pending_update_count": 0})
                    }
                    "getme" => {
                        json!({"id": 999, "is_bot": true, "first_name": "RentFlow", "username": "rentflow_bot",
                        "can_join_groups": true, "can_read_all_group_messages": false, "supports_inline_queries": false,
                        "can_connect_to_business": false, "has_main_web_app": false})
                    }
                    "getupdates" => {
                        let offset = body_json["offset"].as_i64().unwrap_or(0);
                        let fresh: Vec<Value> = updates
                            .iter()
                            .filter(|u| u["update_id"].as_i64().unwrap() >= offset)
                            .cloned()
                            .collect();
                        if fresh.is_empty() {
                            tokio::time::sleep(Duration::from_millis(300)).await;
                        }
                        Value::Array(fresh)
                    }
                    "getfile" => {
                        json!({"file_id": "f", "file_unique_id": "u", "file_size": 3, "file_path": "photos/f.jpg"})
                    }
                    "answercallbackquery" => json!(true),
                    "editmessagereplymarkup" => message(777, json!({"text": "x"})),
                    "sendphoto" => message(
                        body_json["chat_id"].as_i64().unwrap_or(0),
                        json!({"text": "photo"}),
                    ),
                    "sendmessage" => {
                        let chat_id = body_json["chat_id"].as_i64().unwrap_or(0);
                        log.lock().unwrap().sent.push(body_json.clone());
                        message(chat_id, json!({"text": "sent"}))
                    }
                    _ => json!(true),
                };
                respond(
                    &mut sock,
                    "application/json",
                    json!({"ok": true, "result": result}).to_string().as_bytes(),
                )
                .await;
            });
        }
    });
    (url, log)
}

#[tokio::test]
async fn adapter_handles_text_photo_and_callback_end_to_end() {
    let updates = vec![
        // 1: текст
        json!({"update_id": 1, "message": message(777, json!({"text": "/whoami", "entities": [{"type": "bot_command", "offset": 0, "length": 7}]}))}),
        // 2: фото от подключённого арендатора (888) — должно скачаться и дойти до приложения
        json!({"update_id": 2, "message": message(888, json!({"photo": [{"file_id": "f", "file_unique_id": "u", "file_size": 3, "width": 10, "height": 10}]}))}),
        // 3: нажатие кнопки от постороннего
        json!({"update_id": 3, "callback_query": {"id": "cb1", "from": user(777), "chat_instance": "ci",
            "message": message(777, json!({"text": "карточка"})), "data": "ok:1"}}),
        // 4: /start с токеном от постороннего
        json!({"update_id": 4, "message": message_with_id(777, 11, json!({"text": "/start deadbeef", "entities": [{"type": "bot_command", "offset": 0, "length": 6}]}))}),
        // 5 и 6: одно и то же сообщение доставлено дважды (message_id совпадает) — ответ должен быть один
        json!({"update_id": 5, "message": message_with_id(777, 77, json!({"text": "/whoami", "entities": [{"type": "bot_command", "offset": 0, "length": 7}]}))}),
        json!({"update_id": 6, "message": message_with_id(777, 77, json!({"text": "/whoami", "entities": [{"type": "bot_command", "offset": 0, "length": 7}]}))}),
    ];
    let (url, log) = start_stub(updates).await;

    let pool = db::connect("sqlite::memory:").await.unwrap();
    sqlx::query("INSERT INTO app_user (telegram_id, chat_id, role, created_at) VALUES (888, 888, 'tenant', 'x')").execute(&pool).await.unwrap();
    let app = Arc::new(App::new(
        pool,
        Config {
            landlord_telegram_id: 1,
            bot_username: "rentflow_bot".into(),
            photo_dir: None,
            invite_ttl_days: 7,
            ocr_enabled: false,
        },
        Arc::new(NoExtractor),
    ));
    let bot = Bot::new(TOKEN).set_api_url(url.parse().unwrap());
    let runner = tokio::spawn(rentflow::bot::run(bot, app, 3));

    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if log.lock().unwrap().sent.len() >= 5 || std::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // даём время обработать дубль: он не должен породить шестой ответ
    tokio::time::sleep(Duration::from_secs(1)).await;
    runner.abort();

    let log = log.lock().unwrap();
    let texts: Vec<String> = log
        .sent
        .iter()
        .map(|m| m["text"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("Ваш Telegram-ID: 777")),
        "{texts:?}"
    );
    // фото арендатора скачано (getFile + файл) и обработано приложением
    assert!(
        log.paths.iter().any(|p| p.ends_with("/GetFile")),
        "{:?}",
        log.paths
    );
    assert!(
        log.paths.iter().any(|p| p.starts_with("/file/bot")),
        "{:?}",
        log.paths
    );
    assert!(
        texts.iter().any(|t| t.contains("не подключены")),
        "{texts:?}"
    );
    // кнопка: ответ на нажатие и снятие клавиатуры
    assert!(
        log.paths
            .iter()
            .any(|p| p.ends_with("/AnswerCallbackQuery")),
        "{:?}",
        log.paths
    );
    assert!(
        log.paths
            .iter()
            .any(|p| p.ends_with("/EditMessageReplyMarkup")),
        "{:?}",
        log.paths
    );
    // /start с несуществующим токеном — понятный отказ, а не молчание
    assert!(
        texts.iter().any(|t| t.contains("недействительна")),
        "{texts:?}"
    );
}

#[tokio::test]
async fn send_all_sends_photos_before_the_text_with_buttons() {
    let (url, log) = start_stub(vec![]).await;
    let bot = Bot::new(TOKEN).set_api_url(url.parse().unwrap());
    let mut reply =
        rentflow::app::Reply::with_buttons(100, "Расчёт", vec![vec![("Утвердить", "ap:1".into())]]);
    reply.photos = vec![
        ("file-A".into(), "Холодная вода: 755".into()),
        ("file-B".into(), "Горячая вода: 417".into()),
    ];
    rentflow::bot::send_all(&bot, vec![reply]).await;

    let log = log.lock().unwrap();
    let order: Vec<&str> = log
        .paths
        .iter()
        .filter_map(|p| p.rsplit('/').next())
        .filter(|m| m.starts_with("Send"))
        .collect();
    assert_eq!(
        order,
        ["SendPhoto", "SendPhoto", "SendMessage"],
        "фото уходят раньше текста с кнопками: {:?}",
        log.paths
    );
    assert_eq!(log.sent.len(), 1, "текст с кнопками отправлен один раз");
    assert!(
        log.sent[0]["reply_markup"].is_object(),
        "кнопки на месте: {}",
        log.sent[0]
    );
}

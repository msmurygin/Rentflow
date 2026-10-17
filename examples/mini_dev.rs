//! Локальный стенд Mini App без Telegram: база во временном файле, распознавание-заглушка, уведомления в лог.
//! `GET /dev/init?user=<id>` выдаёт подписанный initData — только здесь, в production-бинарнике этого нет.
//!
//!   cargo run --example mini_dev -- web/dist 8099

use async_trait::async_trait;
use axum::{Router, extract::Query, routing::get};
use rentflow::api::{ApiState, Notifier, auth::sign_init_data, router};
use rentflow::app::{App, Config, Incoming, Reply, Sender};
use rentflow::ocr::{Candidate, Extractor};
use std::{collections::HashMap, sync::Arc};

const TOKEN: &str = "123456:DEV-ONLY";

struct LogNotifier;
#[async_trait]
impl Notifier for LogNotifier {
    async fn send(&self, replies: Vec<Reply>) {
        for r in replies {
            println!(
                "[notify chat {}] {}",
                r.chat_id,
                r.text.replace('\n', " | ")
            );
        }
    }
}

/// «Распознаёт» фиксированные показания по очереди, чтобы стенд показывал подсказку ИИ.
struct DemoOcr;
#[async_trait]
impl Extractor for DemoOcr {
    async fn extract(&self, image: &[u8]) -> Result<Vec<Candidate>, String> {
        let code = ["cold", "hot", "t1", "t2"][image.get(4).copied().unwrap_or(0) as usize % 4];
        let value =
            ["755", "417", "3975.48", "1179"][image.get(4).copied().unwrap_or(0) as usize % 4];
        Ok(vec![Candidate {
            register: Some(code.into()),
            value: Some(value.into()),
            confidence: "high".into(),
        }])
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let web = args.next().unwrap_or_else(|| "web/dist".into());
    let port = args.next().unwrap_or_else(|| "8099".into());
    let dir = std::env::temp_dir().join(format!("rentflow-dev-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    let pool = rentflow::db::connect(&format!("sqlite://{}/dev.db", dir.display())).await?;
    let app = Arc::new(App::new(
        pool,
        Config {
            bot_username: "rentflow_dev_bot".into(),
            photo_dir: Some(dir.join("photos")),
            invite_ttl_days: 7,
            ocr_enabled: true,
            // только на учебном стенде: сценарий проверяет и аннулирование оплаченного счёта
            allow_void_paid: true,
            ..Default::default()
        },
        Arc::new(DemoOcr),
    ));
    let bot_app = app.clone();
    let state = Arc::new(ApiState::new(app, Arc::new(LogNotifier), TOKEN.into(), 3));
    // имитация принятия приглашения ботом: /dev/accept?user=2&token=...
    let dev = Router::new()
        .route(
            "/dev/accept",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let app = bot_app.clone();
                async move {
                    let id: i64 = q.get("user").and_then(|v| v.parse().ok()).unwrap_or(2);
                    let from = Sender {
                        telegram_id: id,
                        chat_id: id,
                        name: format!("Dev{id}"),
                    };
                    let now = chrono::Utc::now().naive_utc() + chrono::Duration::hours(3);
                    let r = app
                        .handle(&from, Incoming::Start(q.get("token").cloned()), now)
                        .await;
                    r.first().map(|r| r.text.clone()).unwrap_or_default()
                }
            }),
        )
        .route(
            "/dev/init",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                let id = q.get("user").cloned().unwrap_or_else(|| "1".into());
                let user = format!(r#"{{"id":{id},"first_name":"Dev{id}"}}"#);
                let now = chrono::Utc::now().timestamp().to_string();
                sign_init_data(&[("auth_date", &now), ("user", &user)], TOKEN)
            }),
        );
    let app = router(state, Some(web.into())).merge(dev);
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;
    println!(
        "mini app dev stand: http://127.0.0.1:{port}  (data in {})",
        dir.display()
    );
    axum::serve(listener, app).await?;
    Ok(())
}

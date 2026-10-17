use anyhow::{Context, Result};
use rentflow::app::{App, Config};
use rentflow::ocr::{ClaudeExtractor, Extractor, NoExtractor};
use std::{path::PathBuf, sync::Arc};
use teloxide::prelude::*;

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    let token =
        env("TELEGRAM_BOT_TOKEN").context("TELEGRAM_BOT_TOKEN не задан (токен от @BotFather)")?;
    let landlord_telegram_id: i64 = env("LANDLORD_TELEGRAM_ID")
        .context("LANDLORD_TELEGRAM_ID не задан (ваш числовой Telegram-ID; узнать: напишите боту /whoami)")?
        .parse()
        .context("LANDLORD_TELEGRAM_ID должен быть числом")?;
    let database_url = env("DATABASE_URL").unwrap_or_else(|| "sqlite://data/rentflow.db".into());
    if let Some(path) = database_url
        .strip_prefix("sqlite://")
        .filter(|p| !p.contains(":memory:"))
        && let Some(dir) = std::path::Path::new(path).parent()
    {
        std::fs::create_dir_all(dir).context("не удалось создать каталог базы")?;
    }
    let tz_offset_hours: i64 = env("TZ_OFFSET_HOURS")
        .map(|v| v.parse())
        .transpose()
        .context("TZ_OFFSET_HOURS")?
        .unwrap_or(3);

    let extractor: Arc<dyn Extractor> = match env("ANTHROPIC_API_KEY") {
        Some(key) => {
            let model = env("ANTHROPIC_MODEL").unwrap_or_else(|| "claude-opus-5-5".into());
            tracing::info!(%model, "photo recognition enabled");
            Arc::new(ClaudeExtractor::new(key, model))
        }
        None => {
            tracing::warn!("ANTHROPIC_API_KEY не задан: показания вводятся вручную");
            Arc::new(NoExtractor)
        }
    };

    let pool = rentflow::db::connect(&database_url)
        .await
        .context("не удалось открыть базу")?;
    let mut bot = Bot::new(token.clone());
    if let Some(url) = env("TELEGRAM_API_URL") {
        bot = bot.set_api_url(url.parse().context("TELEGRAM_API_URL должен быть URL")?);
    }
    let me = bot.get_me().await.context(
        "Не удалось подключиться к Telegram. Проверьте токен; если ошибка сетевая (HandshakeFailure, timeout) — \
         api.telegram.org недоступен из вашей сети: задайте HTTPS_PROXY (http:// или socks5://) или TELEGRAM_API_URL, см. README → «Проблемы с сетью»",
    )?;
    let bot_username = me.username().to_string();
    tracing::info!(%bot_username, "bot started");

    let app = Arc::new(App::new(
        pool,
        Config {
            landlord_telegram_id,
            bot_username,
            photo_dir: Some(PathBuf::from(
                env("PHOTO_DIR").unwrap_or_else(|| "data/photos".into()),
            )),
            ocr_enabled: env("ANTHROPIC_API_KEY").is_some(),
            invite_ttl_days: env("INVITE_TTL_DAYS")
                .map(|v| v.parse())
                .transpose()
                .context("INVITE_TTL_DAYS")?
                .unwrap_or(7),
        },
        extractor,
    ));

    // HTTP API для Mini App работает рядом с ботом в том же процессе и на той же базе.
    if let Some(bind) = env("HTTP_BIND").or_else(|| Some("0.0.0.0:8080".into())) {
        let state = Arc::new(rentflow::api::ApiState::new(
            app.clone(),
            Arc::new(rentflow::api::TelegramNotifier(bot.clone())),
            token.clone(),
            tz_offset_hours,
        ));
        let router = rentflow::api::router(state, env("WEB_DIR").map(PathBuf::from));
        let listener = tokio::net::TcpListener::bind(&bind)
            .await
            .with_context(|| format!("не удалось занять HTTP_BIND={bind}"))?;
        tracing::info!(%bind, "http api started");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                tracing::error!(error = %e, "http api stopped");
            }
        });
    }
    // Кнопка «Открыть» в чате с ботом ведёт в Mini App (то же, что настройка Menu Button в BotFather).
    if let Some(url) = env("MINIAPP_URL") {
        use teloxide::types::{MenuButton, WebAppInfo};
        let button = MenuButton::WebApp {
            text: "RentFlow".into(),
            web_app: WebAppInfo {
                url: url.parse().context("MINIAPP_URL должен быть https-URL")?,
            },
        };
        if let Err(e) = bot.set_chat_menu_button().menu_button(button).await {
            tracing::warn!(error = %e, "не удалось задать кнопку меню Mini App");
        }
    }
    // Напоминания арендатору и владельцу по расписанию передачи показаний (журнал в базе не даёт повторов).
    {
        let (bot, app) = (bot.clone(), app.clone());
        tokio::spawn(async move {
            loop {
                let now = chrono::Utc::now().naive_utc() + chrono::Duration::hours(tz_offset_hours);
                match app.due_reminders(now).await {
                    Ok(replies) => rentflow::bot::send_all(&bot, replies).await,
                    Err(e) => tracing::error!(error = %e, "reminders failed"),
                }
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            }
        });
    }
    rentflow::bot::run(bot, app, tz_offset_hours).await;
    Ok(())
}

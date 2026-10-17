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
    let bot = Bot::new(token);
    let me = bot.get_me().await.context("Telegram не принял токен")?;
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
    rentflow::bot::run(bot, app, tz_offset_hours).await;
    Ok(())
}

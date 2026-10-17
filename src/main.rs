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

/// Служебные команды: `rentflow backup|verify-backup|restore|backup-mark-offsite|healthcheck`.
async fn cli(cmd: &str, args: &[String]) -> Result<()> {
    use rentflow::backup;
    let now = chrono::Utc::now().naive_utc();
    let database_url = env("DATABASE_URL").unwrap_or_else(|| "sqlite://data/rentflow.db".into());
    let data_dir = database_url
        .strip_prefix("sqlite://")
        .and_then(|p| std::path::Path::new(p).parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data"));
    let backups_root = || {
        args.first()
            .map(PathBuf::from)
            .or_else(|| env("BACKUP_DIR").map(PathBuf::from))
            .unwrap_or_else(|| data_dir.join("backups"))
    };
    match cmd {
        "backup" => {
            let pool = rentflow::db::connect(&database_url).await?;
            let photos = env("PHOTO_DIR").map(PathBuf::from);
            let dir = backup::create_backup(&pool, photos.as_deref(), &backups_root(), now).await?;
            let report = backup::verify_backup(&dir).await?;
            if !report.ok() {
                anyhow::bail!(
                    "копия создана, но не прошла проверку: {}",
                    report.problems.join("; ")
                );
            }
            println!("{}", dir.display());
            Ok(())
        }
        "verify-backup" => {
            let dir = args.first().context("укажите каталог копии")?;
            let report = backup::verify_backup(std::path::Path::new(dir)).await?;
            println!("схема {}, строк: {:?}", report.schema_version, report.rows);
            if !report.ok() {
                anyhow::bail!("копия НЕ пригодна: {}", report.problems.join("; "));
            }
            println!("копия в порядке");
            Ok(())
        }
        "restore" => {
            let (Some(src), Some(dst)) = (args.first(), args.get(1)) else {
                anyhow::bail!(
                    "использование: rentflow restore <каталог копии> <пустой каталог данных>"
                );
            };
            backup::restore_backup(std::path::Path::new(src), std::path::Path::new(dst)).await?;
            println!("восстановлено в {dst}");
            Ok(())
        }
        "migrate-check" => {
            let report = backup::migrate_check(&database_url).await?;
            println!("схема {}, строк: {:?}", report.schema_version, report.rows);
            if !report.ok() {
                anyhow::bail!("база не прошла проверку: {}", report.problems.join("; "));
            }
            println!("миграции применены, база в порядке");
            Ok(())
        }
        "backup-mark-offsite" => backup::mark_offsite(&backups_root(), now),
        "healthcheck" => {
            let bind = env("HTTP_BIND").unwrap_or_else(|| "0.0.0.0:8080".into());
            let port = bind.rsplit(':').next().unwrap_or("8080");
            let r = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()?
                .get(format!("http://127.0.0.1:{port}/healthz"))
                .send()
                .await?;
            if r.status().is_success() {
                Ok(())
            } else {
                anyhow::bail!("healthz: {}", r.status())
            }
        }
        other => anyhow::bail!("неизвестная команда {other}"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(cmd) = args.first() {
        return cli(cmd, &args[1..]).await;
    }
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
            offsite_expected: env("OFFSITE_EXPECTED").is_some_and(|v| v == "1"),
            allow_void_paid: env("ALLOW_VOID_PAID").is_some_and(|v| v == "1"),
            admin_telegram_id: env("ADMIN_TELEGRAM_ID")
                .and_then(|v| v.parse().ok())
                .or(Some(landlord_telegram_id).filter(|id| *id != 0)),
            data_dir: database_url
                .strip_prefix("sqlite://")
                .and_then(|p| std::path::Path::new(p).parent())
                .map(PathBuf::from),
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
    // Сторож: раз в минуту проверяет Telegram, раз в 5 минут оценивает состояние и пишет администратору (не чаще раза в 6 часов).
    if let Some(admin) = app.admin_chat() {
        let (bot, app) = (bot.clone(), app.clone());
        tokio::spawn(async move {
            let mut last_sent: std::collections::HashMap<&'static str, i64> = Default::default();
            let mut tick = 0u32;
            loop {
                let unix = chrono::Utc::now().timestamp();
                if bot.get_me().await.is_ok() {
                    app.note_telegram_ok(unix);
                }
                if tick % 5 == 4 {
                    let now =
                        chrono::Utc::now().naive_utc() + chrono::Duration::hours(tz_offset_hours);
                    for alert in rentflow::ops::evaluate(&app.vitals(now, unix).await) {
                        if last_sent.get(alert.key).is_none_or(|t| unix - t > 6 * 3600) {
                            last_sent.insert(alert.key, unix);
                            let _ = bot
                                .send_message(ChatId(admin), format!("🚨 RentFlow: {}", alert.text))
                                .await;
                        }
                    }
                }
                tick = tick.wrapping_add(1);
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        });
    } else {
        // без администратора алертов нет, но «последний ответ Telegram» для /readyz всё равно поддерживаем
        let (bot, app) = (bot.clone(), app.clone());
        tokio::spawn(async move {
            loop {
                if bot.get_me().await.is_ok() {
                    app.note_telegram_ok(chrono::Utc::now().timestamp());
                }
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        });
    }
    rentflow::bot::run(bot, app, tz_offset_hours).await;
    Ok(())
}

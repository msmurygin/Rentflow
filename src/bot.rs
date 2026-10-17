//! Адаптер Telegram: переводит апдейты teloxide в `Incoming` и ответы `Reply` в сообщения.
//! Логики здесь нет — она в `app.rs` и покрыта тестами без сети.

use crate::app::{App, Incoming, Reply, Sender};
use chrono::{Duration, NaiveDateTime, Utc};
use std::sync::Arc;
use teloxide::{
    dispatching::UpdateFilterExt,
    net::Download,
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, InputFile},
};

const MAX_PHOTO_BYTES: u32 = 12 * 1024 * 1024;

#[derive(Clone)]
struct Ctx {
    app: Arc<App>,
    tz_offset_hours: i64,
}

impl Ctx {
    /// Местное время: период начисления определяется по нему, а не по UTC.
    fn now(&self) -> NaiveDateTime {
        Utc::now().naive_utc() + Duration::hours(self.tz_offset_hours)
    }
}

pub async fn run(bot: Bot, app: Arc<App>, tz_offset_hours: i64) {
    let ctx = Ctx {
        app,
        tz_offset_hours,
    };
    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(on_message))
        .branch(Update::filter_callback_query().endpoint(on_callback));
    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![ctx])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}

/// Отправляет ответы и возвращает неудачи: (чат, безопасный код причины). Остальные ответы уходят независимо от сбоев.
pub async fn send_all_report(bot: &Bot, replies: Vec<Reply>) -> Vec<(i64, &'static str)> {
    let mut failed = vec![];
    for r in replies {
        for (file_id, caption) in &r.photos {
            // `local:<путь>` — фото из Mini App: у Telegram file_id нет, отправляем файл с диска
            let file = match file_id.strip_prefix("local:") {
                Some(path) => match tokio::fs::read(path).await {
                    Ok(bytes) => InputFile::memory(bytes).file_name("meter.jpg"),
                    Err(e) => {
                        tracing::warn!(path, error = %e, "local photo unreadable");
                        continue;
                    }
                },
                None => InputFile::file_id(file_id.clone()),
            };
            let sent = bot
                .send_photo(ChatId(r.chat_id), file)
                .caption(caption.clone())
                .await;
            if let Err(e) = sent {
                tracing::warn!(chat = r.chat_id, error = %e, "send_photo failed");
                failed.push((r.chat_id, classify(&e)));
            }
        }
        let mut req = bot.send_message(ChatId(r.chat_id), r.text);
        if !r.buttons.is_empty() {
            let rows: Vec<Vec<InlineKeyboardButton>> = r
                .buttons
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|b| InlineKeyboardButton::callback(b.label, b.data))
                        .collect()
                })
                .collect();
            req = req.reply_markup(InlineKeyboardMarkup::new(rows));
        }
        if let Err(e) = req.await {
            // например, пользователь заблокировал бота: остальные ответы всё равно отправляем
            tracing::warn!(chat = r.chat_id, error = %e, "send_message failed");
            failed.push((r.chat_id, classify(&e)));
        }
    }
    failed
}

pub async fn send_all(bot: &Bot, replies: Vec<Reply>) {
    send_all_report(bot, replies).await;
}

/// Как `send_all`, но каждая неудача пишется в журнал доставки (без текста сообщения) — по нему видно,
/// кому не дошло и почему (бот заблокирован, чат не найден, сеть).
pub async fn send_logged(bot: &Bot, app: &App, kind: &str, replies: Vec<Reply>) {
    let now = Utc::now().naive_utc();
    for (chat, reason) in send_all_report(bot, replies).await {
        app.record_failed_delivery(now, chat, kind, reason).await;
    }
}

/// Безопасный код причины сбоя доставки: без текста ошибки и персональных данных.
pub fn classify(e: &teloxide::RequestError) -> &'static str {
    use teloxide::{ApiError, RequestError};
    match e {
        RequestError::Api(ApiError::BotBlocked) => "bot_blocked",
        RequestError::Api(ApiError::ChatNotFound) => "chat_not_found",
        RequestError::Api(ApiError::UserDeactivated) => "user_deactivated",
        RequestError::Api(_) => "telegram_api_error",
        RequestError::RetryAfter(_) => "rate_limited",
        RequestError::Network(_) => "network",
        _ => "other",
    }
}

async fn on_message(bot: Bot, msg: Message, ctx: Ctx) -> ResponseResult<()> {
    // Бот работает только в личных чатах: счета и показания не для групп.
    if !msg.chat.is_private() {
        return Ok(());
    }
    let Some(user) = msg.from.as_ref() else {
        return Ok(());
    };
    let from = Sender {
        telegram_id: user.id.0 as i64,
        chat_id: msg.chat.id.0,
        name: user.full_name(),
    };
    let now = ctx.now();

    let input = if let Some(photos) = msg.photo() {
        let Some(best) = photos.last() else {
            return Ok(());
        };
        if !ctx.app.accepts_photos_from(from.telegram_id).await {
            send_all(
                &bot,
                ctx.app
                    .handle(&from, Incoming::Text("/help".into()), now)
                    .await,
            )
            .await;
            return Ok(());
        }
        if best.file.size > MAX_PHOTO_BYTES {
            send_all(
                &bot,
                vec![Reply::text(
                    from.chat_id,
                    "Фото слишком большое. Отправьте его как обычное фото (сжатое).",
                )],
            )
            .await;
            return Ok(());
        }
        let mut bytes = Vec::new();
        let downloaded: Result<(), String> = async {
            let file = bot
                .get_file(best.file.id.clone())
                .await
                .map_err(|e| e.to_string())?;
            bot.download_file(&file.path, &mut bytes)
                .await
                .map_err(|e| e.to_string())
        }
        .await;
        if let Err(e) = downloaded {
            tracing::warn!(error = %e, "photo download failed");
            send_all(&bot, vec![Reply::text(from.chat_id, "Не удалось скачать фото. Отправьте его ещё раз или введите показание вручную: /enter")]).await;
            return Ok(());
        }
        Incoming::Photo {
            file_id: best.file.id.clone(),
            unique_id: best.file.unique_id.clone(),
            bytes,
        }
    } else if let Some(text) = msg.text() {
        let first = text.split_whitespace().next().unwrap_or("");
        if first == "/start" || first.starts_with("/start@") {
            let payload = text.split_whitespace().nth(1).map(str::to_string);
            Incoming::Start(payload)
        } else {
            Incoming::Text(text.to_string())
        }
    } else {
        return Ok(());
    };

    let replies = ctx.app.handle(&from, input, now).await;
    send_all(&bot, replies).await;
    Ok(())
}

async fn on_callback(bot: Bot, q: CallbackQuery, ctx: Ctx) -> ResponseResult<()> {
    bot.answer_callback_query(q.id.clone()).await.ok();
    let (Some(data), Some(msg)) = (q.data.clone(), q.message.clone()) else {
        return Ok(());
    };
    if !msg.chat().is_private() {
        return Ok(());
    }
    let from = Sender {
        telegram_id: q.from.id.0 as i64,
        chat_id: msg.chat().id.0,
        name: q.from.full_name(),
    };
    // Кнопки сообщения гасим сразу: повторное нажатие невозможно, а действие всё равно идемпотентно.
    bot.edit_message_reply_markup(msg.chat().id, msg.id())
        .await
        .ok();
    let replies = ctx
        .app
        .handle(&from, Incoming::Callback(data), ctx.now())
        .await;
    send_all(&bot, replies).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use teloxide::{ApiError, RequestError};

    #[test]
    fn delivery_failures_get_stable_safe_codes() {
        assert_eq!(
            classify(&RequestError::Api(ApiError::BotBlocked)),
            "bot_blocked"
        );
        assert_eq!(
            classify(&RequestError::Api(ApiError::ChatNotFound)),
            "chat_not_found"
        );
        assert_eq!(
            classify(&RequestError::Api(ApiError::UserDeactivated)),
            "user_deactivated"
        );
        assert_eq!(
            classify(&RequestError::Api(ApiError::MessageTextIsEmpty)),
            "telegram_api_error"
        );
    }
}

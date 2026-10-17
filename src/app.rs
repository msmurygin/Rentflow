//! Ядро бота: «входящее сообщение → список ответов». Не знает про Telegram — адаптер в `bot.rs`
//! только переводит апдейты в `Incoming` и ответы в сообщения. Благодаря этому весь сценарий
//! (настройка → показания → расчёт → утверждение → счёт) тестируется без сети.

use crate::ocr::Extractor;
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::{path::PathBuf, sync::Arc};

pub struct Config {
    /// Единственный владелец: только этот Telegram-ID может настраивать объект и утверждать счета.
    pub landlord_telegram_id: i64,
    pub bot_username: String,
    /// Куда сохранять оригиналы фото (`None` — только ссылки Telegram).
    pub photo_dir: Option<PathBuf>,
    pub invite_ttl_days: i64,
    /// Фото уходят во внешний ИИ-сервис распознавания — арендатору об этом сообщается.
    pub ocr_enabled: bool,
}

pub struct App {
    pub(crate) pool: SqlitePool,
    pub(crate) cfg: Config,
    pub(crate) extractor: Arc<dyn Extractor>,
}

#[derive(Debug, Clone)]
pub struct Sender {
    pub telegram_id: i64,
    pub chat_id: i64,
    pub name: String,
}

#[derive(Debug, Clone)]
pub enum Incoming {
    /// `/start` с токеном приглашения или без.
    Start(Option<String>),
    Text(String),
    Photo {
        file_id: String,
        unique_id: String,
        bytes: Vec<u8>,
    },
    Callback(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub label: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub chat_id: i64,
    pub text: String,
    pub buttons: Vec<Vec<Button>>,
    /// Фото (Telegram file_id, подпись), отправляются перед текстом: кнопки остаются под последним сообщением.
    pub photos: Vec<(String, String)>,
}

impl Reply {
    pub fn text(chat_id: i64, text: impl Into<String>) -> Self {
        Self {
            chat_id,
            text: text.into(),
            buttons: vec![],
            photos: vec![],
        }
    }

    pub fn with_buttons(
        chat_id: i64,
        text: impl Into<String>,
        buttons: Vec<Vec<(&str, String)>>,
    ) -> Self {
        let buttons = buttons
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|(l, d)| Button {
                        label: l.to_string(),
                        data: d,
                    })
                    .collect()
            })
            .collect();
        Self {
            chat_id,
            text: text.into(),
            buttons,
            photos: vec![],
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Сообщение для пользователя, не сбой.
    #[error("{0}")]
    User(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub type Res<T> = Result<T, AppError>;

pub fn user_err<T>(msg: impl Into<String>) -> Res<T> {
    Err(AppError::User(msg.into()))
}

#[derive(Debug, Clone)]
pub struct UserRow {
    pub id: i64,
    pub chat_id: i64,
}

pub enum Who {
    Landlord(UserRow),
    Tenant(UserRow),
    Stranger,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum State {
    /// Мастер настройки: номер шага и ответы.
    Setup { step: usize, answers: Vec<String> },
    /// Форма ввода показаний: ждём число для регистра `code`. `edit` — правка одного значения из итоговой сводки
    /// (после ввода возвращаемся к сводке), иначе идём к следующему незаполненному регистру.
    Entry { code: String, edit: bool },
    /// Число для `code` уже введено, ждём фото-подтверждение (если оно обязательно).
    Proof { code: String, edit: bool },
    /// Аннулирование утверждённого счёта: выбран счёт, потом причина, потом подтверждение.
    Void {
        bill_id: i64,
        reason: Option<String>,
    },
    /// Меню тарифов: выбрана ли дата, номер шага и ответы (`keep` | `off` | цена).
    Tariff {
        month: Option<String>,
        step: usize,
        answers: Vec<String>,
    },
}

pub fn now_iso(now: NaiveDateTime) -> String {
    now.format("%Y-%m-%dT%H:%M:%S").to_string()
}

impl App {
    pub fn new(pool: SqlitePool, cfg: Config, extractor: Arc<dyn Extractor>) -> Self {
        Self {
            pool,
            cfg,
            extractor,
        }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Единственная точка входа. Ошибки превращаются в понятный ответ, а не в падение бота.
    pub async fn handle(&self, from: &Sender, input: Incoming, now: NaiveDateTime) -> Vec<Reply> {
        match self.route(from, input, now).await {
            Ok(replies) => replies,
            Err(AppError::User(msg)) => vec![Reply::text(from.chat_id, msg)],
            Err(AppError::Db(e)) => {
                tracing::error!(error = %e, "database error");
                vec![Reply::text(
                    from.chat_id,
                    "Что-то пошло не так. Попробуйте ещё раз; если повторится — сообщите владельцу.",
                )]
            }
        }
    }

    /// `true` — это событие обрабатывается впервые; `false` — дубль (повторная доставка или второй экземпляр бота).
    /// Ключ вставляется атомарно, поэтому даже при гонке двух процессов обработает только один.
    pub async fn first_time(&self, key: &str, now: NaiveDateTime) -> bool {
        let inserted =
            sqlx::query("INSERT OR IGNORE INTO seen_update (key, created_at) VALUES (?, ?)")
                .bind(key)
                .bind(now_iso(now))
                .execute(&self.pool)
                .await;
        match inserted {
            Ok(r) => {
                if r.rows_affected() == 1 {
                    // записи старше недели не нужны
                    let _ = sqlx::query("DELETE FROM seen_update WHERE created_at < ?")
                        .bind(now_iso(now - chrono::Duration::days(7)))
                        .execute(&self.pool)
                        .await;
                }
                r.rows_affected() == 1
            }
            Err(e) => {
                tracing::error!(error = %e, "seen_update insert failed");
                true
            }
        }
    }

    /// Нужно ли скачивать фото от этого пользователя (чужим людям трафик не тратим).
    pub async fn accepts_photos_from(&self, telegram_id: i64) -> bool {
        telegram_id == self.cfg.landlord_telegram_id
            || sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM app_user WHERE telegram_id = ? AND role = 'tenant'",
            )
            .bind(telegram_id)
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0)
                > 0
    }

    pub(crate) async fn who(&self, from: &Sender, now: NaiveDateTime) -> Res<Who> {
        if from.telegram_id == self.cfg.landlord_telegram_id {
            sqlx::query(
                "INSERT INTO app_user (telegram_id, chat_id, role, display_name, created_at)
                 VALUES (?, ?, 'landlord', ?, ?)
                 ON CONFLICT(telegram_id) DO UPDATE SET chat_id = excluded.chat_id",
            )
            .bind(from.telegram_id)
            .bind(from.chat_id)
            .bind(&from.name)
            .bind(now_iso(now))
            .execute(&self.pool)
            .await?;
            let id: i64 = sqlx::query_scalar("SELECT id FROM app_user WHERE telegram_id = ?")
                .bind(from.telegram_id)
                .fetch_one(&self.pool)
                .await?;
            return Ok(Who::Landlord(UserRow {
                id,
                chat_id: from.chat_id,
            }));
        }
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT id FROM app_user WHERE telegram_id = ? AND role = 'tenant'")
                .bind(from.telegram_id)
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some((id,)) => {
                sqlx::query("UPDATE app_user SET chat_id = ? WHERE id = ?")
                    .bind(from.chat_id)
                    .bind(id)
                    .execute(&self.pool)
                    .await?;
                Ok(Who::Tenant(UserRow {
                    id,
                    chat_id: from.chat_id,
                }))
            }
            None => Ok(Who::Stranger),
        }
    }

    pub(crate) async fn state(&self, telegram_id: i64) -> Res<Option<State>> {
        let raw: Option<(String,)> =
            sqlx::query_as("SELECT state FROM chat_state WHERE telegram_id = ?")
                .bind(telegram_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(raw.and_then(|(s,)| serde_json::from_str(&s).ok()))
    }

    pub(crate) async fn set_state(&self, telegram_id: i64, state: &State) -> Res<()> {
        sqlx::query("INSERT INTO chat_state (telegram_id, state) VALUES (?, ?) ON CONFLICT(telegram_id) DO UPDATE SET state = excluded.state")
            .bind(telegram_id)
            .bind(serde_json::to_string(state).expect("state serializes"))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(crate) async fn clear_state(&self, telegram_id: i64) -> Res<()> {
        sqlx::query("DELETE FROM chat_state WHERE telegram_id = ?")
            .bind(telegram_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn route(&self, from: &Sender, input: Incoming, now: NaiveDateTime) -> Res<Vec<Reply>> {
        let who = self.who(from, now).await?;
        match input {
            Incoming::Start(Some(token)) => self.accept_invite(from, &who, &token, now).await,
            Incoming::Start(None) => Ok(vec![self.help(from, &who)]),
            Incoming::Callback(data) => self.callback(from, &who, &data, now).await,
            Incoming::Photo {
                file_id,
                unique_id,
                bytes,
            } => match &who {
                Who::Tenant(u) => {
                    self.tenant_photo(from, u, &file_id, &unique_id, &bytes, now)
                        .await
                }
                Who::Landlord(_) => user_err(
                    "Фото счётчиков присылает арендатор. Владельцу они не нужны: расчёт придёт вам на утверждение.",
                ),
                Who::Stranger => Ok(vec![self.stranger(from)]),
            },
            Incoming::Text(text) => self.text(from, &who, &text, now).await,
        }
    }

    fn stranger(&self, from: &Sender) -> Reply {
        Reply::text(
            from.chat_id,
            format!(
                "Вы не подключены. Откройте ссылку-приглашение, которую прислал владелец квартиры.\nВаш Telegram-ID: {}",
                from.telegram_id
            ),
        )
    }

    fn help(&self, from: &Sender, who: &Who) -> Reply {
        let text = match who {
            Who::Landlord(_) => {
                "Команды владельца:\n\
                /setup — настроить объект, арендатора, стартовые показания и тарифы\n\
                /invite — одноразовая ссылка для арендатора\n\
                /status — показания и счета текущего периода\n\
                /tariffs — история тарифов по каждой позиции\n\
                /proof — фото-подтверждение показаний: обязательно или нет\n\
                /tariff — изменить тарифы по шагам (или одной строкой: /tariff cold 66.87 2026-08-01; коды: cold hot sewer t1 t2)\n\
                /paid — отметить счёт оплаченным\n\
                /reset — сбросить неполные показания периода\n\
                /void — аннулировать утверждённый неоплаченный счёт (с причиной)\n\
                /calc — пересчитать период, когда все показания уже приняты\n\
                /whoami — ваш Telegram-ID"
            }
            Who::Tenant(_) => {
                "Отправьте фото счётчиков: холодная вода, горячая вода и электричество (T1 и T2). \
                Бот покажет распознанное значение — проверьте и подтвердите.\n\
                /enter — ввести показание вручную\n\
                /readings — что уже принято, чего не хватает"
            }
            Who::Stranger => return self.stranger(from),
        };
        Reply::text(from.chat_id, text)
    }

    async fn text(
        &self,
        from: &Sender,
        who: &Who,
        text: &str,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let cmd = text.split_whitespace().next().unwrap_or("");
        let cmd = cmd.split('@').next().unwrap_or(cmd);
        match cmd {
            "/whoami" => {
                return Ok(vec![Reply::text(
                    from.chat_id,
                    format!("Ваш Telegram-ID: {}", from.telegram_id),
                )]);
            }
            "/help" => return Ok(vec![self.help(from, who)]),
            "/cancel" => {
                self.clear_state(from.telegram_id).await?;
                return Ok(vec![Reply::text(from.chat_id, "Отменено.")]);
            }
            _ => {}
        }
        match who {
            Who::Stranger => Ok(vec![self.stranger(from)]),
            Who::Landlord(u) => match cmd {
                "/setup" => self.setup_start(from, u).await,
                "/invite" => self.invite(from, u, now).await,
                "/status" => self.landlord_status(from, now).await,
                "/tariff" => self.tariff_cmd(from, u, text, now).await,
                "/tariffs" => self.tariff_history(from, now).await,
                "/proof" => self.proof_cmd(from).await,
                "/paid" => self.paid_list(from).await,
                "/reissue" => self.reissue_list(from).await,
                "/void" => self.void_list(from).await,
                "/reset" => self.reset_period(from, now).await,
                "/calc" => self.calc_cmd(from, now).await,
                c if c.starts_with('/') => Ok(vec![self.help(from, who)]),
                _ => self.landlord_text(from, u, text, now).await,
            },
            Who::Tenant(u) => match cmd {
                "/enter" => self.entry_start(from, u, now).await,
                "/readings" => self.tenant_readings(from, u, now).await,
                c if c.starts_with('/') => Ok(vec![self.help(from, who)]),
                _ => self.tenant_text(from, u, text, now).await,
            },
        }
    }

    async fn callback(
        &self,
        from: &Sender,
        who: &Who,
        data: &str,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let mut parts = data.split(':');
        let kind = parts.next().unwrap_or("");
        let a = parts.next();
        let id = a.and_then(|s| s.parse::<i64>().ok());
        match (who, kind) {
            (Who::Stranger, _) => Ok(vec![self.stranger(from)]),
            (Who::Landlord(u), "setup_ok") => self.setup_confirm(from, u, now).await,
            (Who::Landlord(_), "setup_no") => {
                self.clear_state(from.telegram_id).await?;
                Ok(vec![Reply::text(from.chat_id, "Настройка отменена.")])
            }
            (Who::Landlord(_), "ap") => self.bill_approve(from, id.unwrap_or(0), now).await,
            (Who::Landlord(_), "rj") => self.bill_reject(from, id.unwrap_or(0), now).await,
            (Who::Landlord(_), "pd") => self.bill_paid(from, id.unwrap_or(0), now).await,
            (Who::Landlord(_), "cc") => self.calc_cmd(from, now).await,
            (Who::Landlord(_), "pr") => self.proof_set(from, a.unwrap_or("")).await,
            (Who::Landlord(_), "th") => self.tariff_history(from, now).await,
            (Who::Landlord(_), "tnew") => self.tariff_start(from, now).await,
            (Who::Landlord(_), "tm") => self.tariff_month(from, a.unwrap_or(""), now).await,
            (Who::Landlord(_), "ri") => self.reissue_ask(from, id.unwrap_or(0), now).await,
            (Who::Landlord(_), "rc") => self.reissue_do(from, id.unwrap_or(0), now).await,
            (Who::Landlord(_), "vo") => self.void_pick(from, id.unwrap_or(0)).await,
            (Who::Landlord(_), "vc") => self.void_confirm(from, id.unwrap_or(0), now).await,
            (Who::Landlord(_), "vx") => self.void_cancel(from).await,
            (Who::Landlord(_), "rx") => {
                Ok(vec![Reply::text(from.chat_id, "Оставлено без изменений.")])
            }
            (Who::Landlord(_), "tk") => self.tariff_answer(from, "keep", now).await,
            (Who::Landlord(_), "toff") => self.tariff_answer(from, "off", now).await,
            (Who::Landlord(_), "ts") => self.tariff_save(from, now).await,
            (Who::Landlord(_), "tx") => {
                self.clear_state(from.telegram_id).await?;
                Ok(vec![Reply::text(
                    from.chat_id,
                    "Ввод тарифов отменён, ничего не изменилось.",
                )])
            }
            (Who::Tenant(u), "enter") => self.entry_start(from, u, now).await,
            (Who::Tenant(u), "ed") => self.entry_edit(from, u, a.unwrap_or(""), now).await,
            (Who::Tenant(u), "send") => self.entry_submit(from, u, now).await,
            (Who::Tenant(u), "cx") => self.entry_cancel(from, u, now).await,
            // кнопки из старой версии бота
            (Who::Tenant(_), "ok" | "fix" | "chg" | "r" | "no") => {
                user_err("Эта кнопка устарела. Нажмите /enter — ввод теперь идёт по шагам.")
            }
            _ => user_err("Это действие вам недоступно."),
        }
    }
}

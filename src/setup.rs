//! Настройка владельцем: мастер /setup, тарифы, одноразовое приглашение арендатора.

use crate::app::*;
use crate::util::*;
use chrono::{Duration, NaiveDateTime};
use rust_decimal::Decimal;

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Date,
    DateOrUnknown,
    Decimal,
    DecimalOrNo,
}

const STEPS: &[(&str, &str, Kind)] = &[
    (
        "property",
        "Название объекта (например: «Квартира на Тверской»):",
        Kind::Text,
    ),
    (
        "tenant",
        "Имя арендатора (как вы его называете):",
        Kind::Text,
    ),
    (
        "base_date",
        "Дата, на которую сняты СТАРТОВЫЕ показания (ГГГГ-ММ-ДД или ДД.ММ.ГГГГ). Если дата неизвестна — напишите «неизвестна», я её не придумываю:",
        Kind::DateOrUnknown,
    ),
    (
        "base_cold",
        "Стартовое показание ХВС (холодная вода), м³:",
        Kind::Decimal,
    ),
    (
        "base_hot",
        "Стартовое показание ГВС (горячая вода), м³:",
        Kind::Decimal,
    ),
    (
        "base_t1",
        "Стартовое показание электричества T1, кВт·ч:",
        Kind::Decimal,
    ),
    (
        "base_t2",
        "Стартовое показание электричества T2, кВт·ч:",
        Kind::Decimal,
    ),
    (
        "tariff_from",
        "Дата, с которой действуют эти тарифы (ГГГГ-ММ-ДД). Позже новые тарифы добавляются командой /tariff:",
        Kind::Date,
    ),
    ("p_cold", "Тариф ХВС, ₽ за м³:", Kind::Decimal),
    ("p_hot", "Тариф ГВС, ₽ за м³:", Kind::Decimal),
    (
        "p_sewer",
        "Тариф водоотведения, ₽ за м³ (объём = ХВС + ГВС). Если арендатор водоотведение НЕ оплачивает — напишите «нет»:",
        Kind::DecimalOrNo,
    ),
    ("p_t1", "Тариф электричества T1, ₽ за кВт·ч:", Kind::Decimal),
    ("p_t2", "Тариф электричества T2, ₽ за кВт·ч:", Kind::Decimal),
];

fn validate(kind: Kind, input: &str) -> Option<String> {
    let t = input.trim();
    match kind {
        Kind::Text => (!t.is_empty() && t.chars().count() <= 100).then(|| t.to_string()),
        Kind::Date => parse_date(t).map(|d| d.to_string()),
        Kind::DateOrUnknown => {
            if matches!(
                t.to_lowercase().as_str(),
                "неизвестна" | "неизвестно" | "нет" | "unknown"
            ) {
                Some("unknown".into())
            } else {
                parse_date(t).map(|d| d.to_string())
            }
        }
        Kind::Decimal => parse_decimal(t).map(|d| d.to_string()),
        Kind::DecimalOrNo => {
            if matches!(t.to_lowercase().as_str(), "нет" | "no") {
                Some("no".into())
            } else {
                parse_decimal(t).map(|d| d.to_string())
            }
        }
    }
}

fn answer<'a>(answers: &'a [String], key: &str) -> &'a str {
    let i = STEPS.iter().position(|s| s.0 == key).expect("known key");
    &answers[i]
}

impl App {
    pub(crate) async fn property_id(&self) -> Res<Option<i64>> {
        Ok(
            sqlx::query_scalar("SELECT id FROM property ORDER BY id LIMIT 1")
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub(crate) async fn setup_start(&self, from: &Sender, _u: &UserRow) -> Res<Vec<Reply>> {
        if self.property_id().await?.is_some() {
            return user_err(
                "Объект уже настроен (в этой версии один объект). Тарифы добавляются командой /tariff, арендатор — /invite.",
            );
        }
        self.set_state(
            from.telegram_id,
            &State::Setup {
                step: 0,
                answers: vec![],
            },
        )
        .await?;
        Ok(vec![Reply::text(
            from.chat_id,
            format!("Настройка объекта. /cancel — отмена.\n\n{}", STEPS[0].1),
        )])
    }

    pub(crate) async fn landlord_text(
        &self,
        from: &Sender,
        _u: &UserRow,
        text: &str,
        _now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(State::Setup { step, mut answers }) = self.state(from.telegram_id).await? else {
            return Ok(vec![Reply::text(
                from.chat_id,
                "Не понял. Список команд: /help",
            )]);
        };
        let (_, _, kind) = STEPS[step];
        let Some(value) = validate(kind, text) else {
            return user_err(format!(
                "Не получилось разобрать ответ. Попробуйте ещё раз.\n\n{}",
                STEPS[step].1
            ));
        };
        answers.push(value);
        if step + 1 < STEPS.len() {
            let next = step + 1;
            let prompt = STEPS[next].1.to_string();
            self.set_state(
                from.telegram_id,
                &State::Setup {
                    step: next,
                    answers,
                },
            )
            .await?;
            return Ok(vec![Reply::text(from.chat_id, prompt)]);
        }
        let summary = setup_summary(&answers);
        self.set_state(
            from.telegram_id,
            &State::Setup {
                step: STEPS.len(),
                answers,
            },
        )
        .await?;
        Ok(vec![Reply::with_buttons(
            from.chat_id,
            format!("Проверьте данные:\n\n{summary}\n\nСохранить?"),
            vec![vec![
                ("✅ Сохранить", "setup_ok".into()),
                ("❌ Отмена", "setup_no".into()),
            ]],
        )])
    }

    pub(crate) async fn setup_confirm(
        &self,
        from: &Sender,
        u: &UserRow,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(State::Setup { step, answers }) = self.state(from.telegram_id).await? else {
            return user_err("Нет незавершённой настройки. Начните с /setup.");
        };
        if step != STEPS.len() || answers.len() != STEPS.len() {
            return user_err("Настройка не завершена. Ответьте на оставшиеся вопросы.");
        }
        let ts = now_iso(now);
        let mut tx = self.pool.begin().await?;
        let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM property")
            .fetch_one(&mut *tx)
            .await?;
        if exists > 0 {
            return user_err("Объект уже настроен.");
        }
        let sewer_on = answer(&answers, "p_sewer") != "no";
        let property_id = sqlx::query("INSERT INTO property (landlord_id, name, sewer_enabled, created_at) VALUES (?, ?, ?, ?)")
            .bind(u.id)
            .bind(answer(&answers, "property"))
            .bind(sewer_on)
            .bind(&ts)
            .execute(&mut *tx)
            .await?
            .last_insert_rowid();
        sqlx::query("INSERT INTO tenancy (property_id, tenant_name) VALUES (?, ?)")
            .bind(property_id)
            .bind(answer(&answers, "tenant"))
            .execute(&mut *tx)
            .await?;
        let base_date = match answer(&answers, "base_date") {
            "unknown" => None,
            d => Some(d.to_string()),
        };
        for (code, lbl, unit) in REGISTERS {
            let register_id = sqlx::query(
                "INSERT INTO register (property_id, code, label, unit) VALUES (?, ?, ?, ?)",
            )
            .bind(property_id)
            .bind(code)
            .bind(lbl)
            .bind(unit)
            .execute(&mut *tx)
            .await?
            .last_insert_rowid();
            // Стартовое показание — якорь для первого расчёта; начислений за прошлое оно не создаёт.
            sqlx::query(
                "INSERT INTO reading (register_id, period, kind, value, read_on, source, status, confirmed_by, created_at)
                 VALUES (?, '0000-00', 'baseline', ?, ?, 'baseline', 'confirmed', ?, ?)",
            )
            .bind(register_id)
            .bind(answer(&answers, &format!("base_{code}")))
            .bind(&base_date)
            .bind(u.id)
            .bind(&ts)
            .execute(&mut *tx)
            .await?;
            sqlx::query("INSERT INTO tariff (property_id, code, price, valid_from, created_at) VALUES (?, ?, ?, ?, ?)")
                .bind(property_id)
                .bind(code)
                .bind(answer(&answers, &format!("p_{code}")))
                .bind(answer(&answers, "tariff_from"))
                .bind(&ts)
                .execute(&mut *tx)
                .await?;
        }
        if sewer_on {
            sqlx::query("INSERT INTO tariff (property_id, code, price, valid_from, created_at) VALUES (?, 'sewer', ?, ?, ?)")
                .bind(property_id)
                .bind(answer(&answers, "p_sewer"))
                .bind(answer(&answers, "tariff_from"))
                .bind(&ts)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        self.clear_state(from.telegram_id).await?;
        Ok(vec![Reply::text(
            from.chat_id,
            "Объект сохранён. Стартовые показания записаны и начислений не создают.\nТеперь /invite — ссылка для арендатора.",
        )])
    }

    pub(crate) async fn invite(
        &self,
        from: &Sender,
        u: &UserRow,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(property_id) = self.property_id().await? else {
            return user_err("Сначала настройте объект: /setup");
        };
        let linked: Option<i64> =
            sqlx::query_scalar("SELECT tenant_user_id FROM tenancy WHERE property_id = ?")
                .bind(property_id)
                .fetch_one(&self.pool)
                .await?;
        if linked.is_some() {
            return user_err("Арендатор уже подключён.");
        }
        let _ = u;
        // Прежние неиспользованные ссылки гасим: действует только последняя.
        sqlx::query("UPDATE invite SET expires_at = ? WHERE property_id = ? AND used_at IS NULL")
            .bind(now_iso(now))
            .bind(property_id)
            .execute(&self.pool)
            .await?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let expires = now + Duration::days(self.cfg.invite_ttl_days);
        sqlx::query(
            "INSERT INTO invite (token, property_id, expires_at, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&token)
        .bind(property_id)
        .bind(now_iso(expires))
        .bind(now_iso(now))
        .execute(&self.pool)
        .await?;
        Ok(vec![Reply::text(
            from.chat_id,
            format!(
                "Ссылка для арендатора (одноразовая, действует {} дн.):\nhttps://t.me/{}?start={}",
                self.cfg.invite_ttl_days, self.cfg.bot_username, token
            ),
        )])
    }

    pub(crate) async fn accept_invite(
        &self,
        from: &Sender,
        who: &Who,
        token: &str,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        match who {
            Who::Landlord(_) => {
                return user_err("Вы владелец. Ссылку-приглашение открывает арендатор.");
            }
            Who::Tenant(_) => {
                return user_err("Вы уже подключены. Отправьте фото счётчиков или нажмите /enter.");
            }
            Who::Stranger => {}
        }
        let ts = now_iso(now);
        let mut tx = self.pool.begin().await?;
        let invite: Option<(i64,)> = sqlx::query_as(
            "SELECT property_id FROM invite WHERE token = ? AND used_at IS NULL AND expires_at > ?",
        )
        .bind(token)
        .bind(&ts)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((property_id,)) = invite else {
            return user_err(
                "Ссылка недействительна: она уже использована или истекла. Попросите владельца прислать новую.",
            );
        };
        let user_id = sqlx::query("INSERT INTO app_user (telegram_id, chat_id, role, display_name, created_at) VALUES (?, ?, 'tenant', ?, ?)")
            .bind(from.telegram_id)
            .bind(from.chat_id)
            .bind(&from.name)
            .bind(&ts)
            .execute(&mut *tx)
            .await?
            .last_insert_rowid();
        // Одноразовость: токен гасится атомарно, второй параллельный запрос получит 0 строк.
        let used = sqlx::query(
            "UPDATE invite SET used_by = ?, used_at = ? WHERE token = ? AND used_at IS NULL",
        )
        .bind(user_id)
        .bind(&ts)
        .bind(token)
        .execute(&mut *tx)
        .await?;
        let linked = sqlx::query("UPDATE tenancy SET tenant_user_id = ? WHERE property_id = ? AND tenant_user_id IS NULL")
            .bind(user_id)
            .bind(property_id)
            .execute(&mut *tx)
            .await?;
        if used.rows_affected() != 1 || linked.rows_affected() != 1 {
            return user_err("Ссылка недействительна или арендатор уже подключён.");
        }
        let (name, landlord_chat): (String, i64) = sqlx::query_as(
            "SELECT p.name, l.chat_id FROM property p JOIN app_user l ON l.id = p.landlord_id WHERE p.id = ?",
        )
        .bind(property_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(vec![
            Reply::text(
                from.chat_id,
                format!(
                    "Вы подключены к объекту «{name}».\n\nКаждый месяц отправляйте фото счётчиков: холодная вода, горячая вода и электричество (T1 и T2). \
                     Бот покажет распознанное значение — проверьте его и подтвердите. Если фото не читается, нажмите /enter и введите показание вручную.\n\
                     /readings — что уже принято."
                ),
            ),
            Reply::text(
                landlord_chat,
                format!("Арендатор {} подключился к «{name}».", from.name),
            ),
        ])
    }

    pub(crate) async fn tariff_cmd(
        &self,
        from: &Sender,
        _u: &UserRow,
        text: &str,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(property_id) = self.property_id().await? else {
            return user_err("Сначала настройте объект: /setup");
        };
        let args: Vec<&str> = text.split_whitespace().skip(1).collect();
        let usage = "Формат: /tariff <код> <цена> <дата начала>, например:\n/tariff cold 66.87 2026-08-01\nКоды: cold, hot, sewer, t1, t2. Водоотведение выключается так: /tariff sewer off";
        let code = args.first().copied().unwrap_or("");
        if !matches!(code, "cold" | "hot" | "sewer" | "t1" | "t2") {
            return user_err(usage);
        }
        if code == "sewer" && args.get(1) == Some(&"off") {
            sqlx::query("UPDATE property SET sewer_enabled = 0 WHERE id = ?")
                .bind(property_id)
                .execute(&self.pool)
                .await?;
            return Ok(vec![Reply::text(
                from.chat_id,
                "Водоотведение выключено: в новых счетах его не будет.",
            )]);
        }
        let (Some(price), Some(date)) = (
            args.get(1).and_then(|s| parse_decimal(s)),
            args.get(2).and_then(|s| parse_date(s)),
        ) else {
            return user_err(usage);
        };
        let ts = now_iso(now);
        let inserted = sqlx::query("INSERT INTO tariff (property_id, code, price, valid_from, created_at) VALUES (?, ?, ?, ?, ?)")
            .bind(property_id)
            .bind(code)
            .bind(price.to_string())
            .bind(date.to_string())
            .bind(&ts)
            .execute(&self.pool)
            .await;
        if let Err(sqlx::Error::Database(e)) = &inserted
            && e.is_unique_violation()
        {
            return user_err("Тариф с такой датой начала уже есть.");
        }
        inserted?;
        if code == "sewer" {
            sqlx::query("UPDATE property SET sewer_enabled = 1 WHERE id = ?")
                .bind(property_id)
                .execute(&self.pool)
                .await?;
        }
        Ok(vec![Reply::text(
            from.chat_id,
            format!(
                "Тариф «{}»: {} ₽ с {}. Уже выставленные счета не меняются.",
                label(code),
                money(price),
                date
            ),
        )])
    }
}

fn setup_summary(a: &[String]) -> String {
    let dec = |k: &str| num(answer(a, k).parse::<Decimal>().unwrap_or_default());
    let base_date = match answer(a, "base_date") {
        "unknown" => "дата неизвестна".to_string(),
        d => d.to_string(),
    };
    let sewer = match answer(a, "p_sewer") {
        "no" => "не оплачивается арендатором".to_string(),
        _ => format!("{} ₽/м³ (ХВС + ГВС)", dec("p_sewer")),
    };
    format!(
        "Объект: {}\nАрендатор: {}\n\nСтартовые показания ({base_date}):\n• ХВС: {}\n• ГВС: {}\n• T1: {}\n• T2: {}\n\nТарифы с {}:\n• ХВС: {} ₽/м³\n• ГВС: {} ₽/м³\n• Водоотведение: {sewer}\n• T1: {} ₽/кВт·ч\n• T2: {} ₽/кВт·ч",
        answer(a, "property"),
        answer(a, "tenant"),
        dec("base_cold"),
        dec("base_hot"),
        dec("base_t1"),
        dec("base_t2"),
        answer(a, "tariff_from"),
        dec("p_cold"),
        dec("p_hot"),
        dec("p_t1"),
        dec("p_t2"),
    )
}

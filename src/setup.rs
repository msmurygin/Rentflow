//! Настройка владельцем: мастер /setup, тарифы, одноразовое приглашение арендатора.

use crate::app::*;
use crate::util::*;
use chrono::{Duration, NaiveDateTime};
use rust_decimal::Decimal;

#[derive(Clone, Copy)]
enum Kind {
    Text,
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

/// Всё, что нужно для первой настройки объекта: общий вход для мастера `/setup` и для Mini App.
pub struct NewProperty {
    pub name: String,
    pub tenant_name: String,
    /// Дата стартовых показаний; `None` — неизвестна (бот её не придумывает).
    pub base_date: Option<String>,
    /// Стартовые показания по кодам регистров (строки Decimal).
    pub baselines: Vec<(String, String)>,
    /// Цены `cold hot t1 t2` (строки Decimal).
    pub prices: Vec<(String, String)>,
    /// `None` — водоотведение выключено.
    pub sewer_price: Option<String>,
}

impl App {
    /// Создаёт объект, арендатора, регистры, стартовые показания (начислений не создают) и тарифы текущего месяца.
    pub async fn create_property(
        &self,
        landlord_id: i64,
        np: &NewProperty,
        now: NaiveDateTime,
    ) -> Res<i64> {
        let ts = now_iso(now);
        // Тарифы заводятся на текущий расчётный месяц независимо от даты настройки.
        let month = period_of(now.date());
        let mut tx = self.pool.begin().await?;
        let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM property")
            .fetch_one(&mut *tx)
            .await?;
        if exists > 0 {
            return user_err("Объект уже настроен.");
        }
        let property_id =
            sqlx::query("INSERT INTO property (landlord_id, name, created_at) VALUES (?, ?, ?)")
                .bind(landlord_id)
                .bind(&np.name)
                .bind(&ts)
                .execute(&mut *tx)
                .await?
                .last_insert_rowid();
        sqlx::query("INSERT INTO tenancy (property_id, tenant_name) VALUES (?, ?)")
            .bind(property_id)
            .bind(&np.tenant_name)
            .execute(&mut *tx)
            .await?;
        let find = |list: &[(String, String)], code: &str| {
            list.iter().find(|x| x.0 == code).map(|x| x.1.clone())
        };
        for (code, lbl, unit) in REGISTERS {
            let (Some(base), Some(price)) = (find(&np.baselines, code), find(&np.prices, code))
            else {
                return user_err("Не хватает стартового показания или тарифа.");
            };
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
            .bind(base)
            .bind(&np.base_date)
            .bind(landlord_id)
            .bind(&ts)
            .execute(&mut *tx)
            .await?;
            insert_tariff(&mut tx, property_id, code, &price, &month, true, &ts).await?;
        }
        // Водоотведение: «нет» — выключено с этого месяца (цену не храним), иначе включено с этой ценой.
        match &np.sewer_price {
            None => insert_tariff(&mut tx, property_id, "sewer", "0", &month, false, &ts).await?,
            Some(price) => {
                insert_tariff(&mut tx, property_id, "sewer", price, &month, true, &ts).await?
            }
        }
        tx.commit().await?;
        Ok(property_id)
    }

    /// Новая одноразовая ссылка-приглашение (прежние неиспользованные гасятся); возвращает токен.
    pub async fn create_invite(&self, property_id: i64, now: NaiveDateTime) -> Res<String> {
        let linked: Option<i64> =
            sqlx::query_scalar("SELECT tenant_user_id FROM tenancy WHERE property_id = ?")
                .bind(property_id)
                .fetch_one(&self.pool)
                .await?;
        if linked.is_some() {
            return user_err("Арендатор уже подключён.");
        }
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
        Ok(token)
    }

    pub fn invite_url(&self, token: &str) -> String {
        format!("https://t.me/{}?start={}", self.cfg.bot_username, token)
    }
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
            format!(
                "Настройка объекта: {n} вопросов. /cancel — отмена.\n\n(1/{n}) {}",
                STEPS[0].1,
                n = STEPS.len()
            ),
        )])
    }

    pub(crate) async fn landlord_text(
        &self,
        from: &Sender,
        _u: &UserRow,
        text: &str,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        if let Some(State::Tariff { .. }) = self.state(from.telegram_id).await? {
            return self.tariff_text(from, text, now).await;
        }
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
            let prompt = format!("({}/{}) {}", next + 1, STEPS.len(), STEPS[next].1);
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
        let summary = setup_summary(&answers, &period_of(now.date()));
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
        let base_date = match answer(&answers, "base_date") {
            "unknown" => None,
            d => Some(d.to_string()),
        };
        let np = NewProperty {
            name: answer(&answers, "property").into(),
            tenant_name: answer(&answers, "tenant").into(),
            base_date,
            baselines: REGISTERS
                .iter()
                .map(|r| {
                    (
                        r.0.to_string(),
                        answer(&answers, &format!("base_{}", r.0)).to_string(),
                    )
                })
                .collect(),
            prices: ["cold", "hot", "t1", "t2"]
                .iter()
                .map(|c| {
                    (
                        c.to_string(),
                        answer(&answers, &format!("p_{c}")).to_string(),
                    )
                })
                .collect(),
            sewer_price: match answer(&answers, "p_sewer") {
                "no" => None,
                price => Some(price.to_string()),
            },
        };
        self.create_property(u.id, &np, now).await?;
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
        let _ = u;
        let token = self.create_invite(property_id, now).await?;
        Ok(vec![Reply::text(
            from.chat_id,
            format!(
                "Ссылка для арендатора (одноразовая, действует {} дн.):\n{}",
                self.cfg.invite_ttl_days,
                self.invite_url(&token)
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
        let required: bool = sqlx::query_scalar("SELECT proof_required FROM property WHERE id = ?")
            .bind(property_id)
            .fetch_one(&mut *tx)
            .await?;
        let proof_note = if required {
            "к каждому показанию приложите фото счётчика (владелец требует подтверждение), "
        } else {
            ""
        };
        tx.commit().await?;
        Ok(vec![
            Reply::with_buttons(
                from.chat_id,
                format!(
                    "Вы подключены к объекту «{name}».\n\nКак отправить показания (раз в месяц):\n\
                     1) нажмите «Ввести показания» (или /enter) и вводите числа по шагам: ХВС → ГВС → T1 → T2;\n\
                     2) {proof_note}в конце проверьте сводку и нажмите «Отправить».\n\
                     Вместо ввода можно прислать фото счётчика — я прочитаю цифры, а вы проверите их на сводке.\n\
                     /readings — что уже введено, /cancel — отмена.{}",
                    if self.cfg.ocr_enabled {
                        "\n\nПриватность: чтобы прочитать цифры, фото счётчиков передаются во внешний сервис распознавания. Присылайте только фото самих счётчиков."
                    } else {
                        ""
                    }
                ),
                vec![vec![("▶️ Ввести показания", "enter".into())]],
            ),
            Reply::text(
                landlord_chat,
                format!("Арендатор {} подключился к «{name}».", from.name),
            ),
        ])
    }
}

fn setup_summary(a: &[String], month: &str) -> String {
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
        "Объект: {}\nАрендатор: {}\n\nСтартовые показания ({base_date}):\n• ХВС: {}\n• ГВС: {}\n• T1: {}\n• T2: {}\n\nТарифы с расчётного месяца {} (дальше действуют, пока вы их не измените):\n• ХВС: {} ₽/м³\n• ГВС: {} ₽/м³\n• Водоотведение: {sewer}\n• T1: {} ₽/кВт·ч\n• T2: {} ₽/кВт·ч",
        answer(a, "property"),
        answer(a, "tenant"),
        dec("base_cold"),
        dec("base_hot"),
        dec("base_t1"),
        dec("base_t2"),
        month,
        dec("p_cold"),
        dec("p_hot"),
        dec("p_t1"),
        dec("p_t2"),
    )
}

/// Запись тарифа месяца `month` (`ГГГГ-ММ`); `valid_from` хранится как первое число месяца.
pub(crate) async fn insert_tariff(
    tx: &mut sqlx::SqliteConnection,
    property_id: i64,
    code: &str,
    price: &str,
    month: &str,
    enabled: bool,
    ts: &str,
) -> Res<()> {
    sqlx::query(
        "INSERT INTO tariff (property_id, code, price, valid_from, created_at, enabled) VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(property_id, code, valid_from)
         DO UPDATE SET price = excluded.price, enabled = excluded.enabled, created_at = excluded.created_at",
    )
    .bind(property_id)
    .bind(code)
    .bind(price)
    .bind(format!("{month}-01"))
    .bind(ts)
    .bind(enabled)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

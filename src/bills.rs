//! Расчёт черновика счёта и решения владельца (утвердить / отклонить / оплачено).
//! Деньги считает только `billing::calculate_with` на Decimal; ИИ здесь не участвует.

use crate::app::*;
use crate::billing::{self, BillingError, MeterInput, MeterKind, ReadingPrecision, SewerRule};
use crate::util::*;
use chrono::NaiveDateTime;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapLine {
    pub code: String,
    pub previous_raw: String,
    pub current_raw: String,
    pub previous: String,
    pub current: String,
    pub usage: String,
    pub price: String,
    pub amount: String,
    pub previous_read_on: Option<String>,
}

/// Неизменяемый снимок расчёта: показания, тарифы и суммы на момент создания.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub period: String,
    pub currency: String,
    pub total: String,
    pub policy: String,
    pub lines: Vec<SnapLine>,
}

pub struct Draft {
    pub bill_id: i64,
    pub snapshot: Snapshot,
}

fn explain(e: &BillingError) -> String {
    let l = |id: &String| label(id).to_string();
    match e {
        BillingError::NoMeters => "нет счётчиков".into(),
        BillingError::MissingPrevious(id) => format!("{}: нет предыдущего показания", l(id)),
        BillingError::MissingCurrent(id) => format!("{}: нет текущего показания", l(id)),
        BillingError::MissingTariff(id) => format!("{}: не задан тариф (команда /tariff)", l(id)),
        BillingError::NegativeValue(id) => format!("{}: отрицательное значение", l(id)),
        BillingError::NegativeUsage(id) => format!(
            "{}: расход отрицательный — нужна ручная корректировка (замена счётчика?)",
            l(id)
        ),
        BillingError::TariffChangedInPeriod(id) => {
            format!(
                "{}: тариф менялся внутри периода — нужна ручная разбивка, автоматически не считаю",
                l(id)
            )
        }
    }
}

fn kind_of(code: &str) -> MeterKind {
    match code {
        "cold" => MeterKind::ColdWater,
        "hot" => MeterKind::HotWater,
        _ => MeterKind::Electricity,
    }
}

impl App {
    /// Цена тарифа, действующая в расчётном периоде. Смена внутри периода — отказ, а не молчаливый выбор.
    async fn tariff_for(
        &self,
        property_id: i64,
        code: &str,
        period: &str,
    ) -> Res<Result<Decimal, BillingError>> {
        let (first, last) =
            period_bounds(period).ok_or_else(|| AppError::User("Некорректный период.".into()))?;
        let changed: i64 = sqlx::query_scalar("SELECT count(*) FROM tariff WHERE property_id = ? AND code = ? AND valid_from > ? AND valid_from <= ?")
            .bind(property_id)
            .bind(code)
            .bind(first.to_string())
            .bind(last.to_string())
            .fetch_one(&self.pool)
            .await?;
        if changed > 0 {
            return Ok(Err(BillingError::TariffChangedInPeriod(code.into())));
        }
        let price: Option<(String,)> =
            sqlx::query_as("SELECT price FROM tariff WHERE property_id = ? AND code = ? AND valid_from <= ? ORDER BY valid_from DESC LIMIT 1")
                .bind(property_id)
                .bind(code)
                .bind(first.to_string())
                .fetch_optional(&self.pool)
                .await?;
        Ok(match price {
            Some((p,)) => Ok(p.parse().unwrap_or_default()),
            None => Err(BillingError::MissingTariff(code.into())),
        })
    }

    /// Создаёт черновик счёта за период по подтверждённым показаниям. Дубль за период невозможен (уникальный индекс).
    pub(crate) async fn create_draft(
        &self,
        property_id: i64,
        period: &str,
        now: NaiveDateTime,
    ) -> Res<Draft> {
        let sewer_enabled: bool =
            sqlx::query_scalar("SELECT sewer_enabled FROM property WHERE id = ?")
                .bind(property_id)
                .fetch_one(&self.pool)
                .await?;
        let mut inputs = Vec::with_capacity(4);
        let mut prev_dates: Vec<Option<String>> = Vec::new();
        for (code, _, _) in REGISTERS {
            let register_id: i64 =
                sqlx::query_scalar("SELECT id FROM register WHERE property_id = ? AND code = ?")
                    .bind(property_id)
                    .bind(code)
                    .fetch_one(&self.pool)
                    .await?;
            let current: Option<(String,)> = sqlx::query_as(
                "SELECT value FROM reading WHERE register_id = ? AND period = ? AND kind = 'submitted' AND status = 'confirmed'",
            )
            .bind(register_id)
            .bind(period)
            .fetch_optional(&self.pool)
            .await?;
            // Предыдущее — последнее подтверждённое показание более раннего периода (или стартовое).
            let previous: Option<(String, Option<String>)> = sqlx::query_as(
                "SELECT value, read_on FROM reading WHERE register_id = ? AND status = 'confirmed' AND period < ? ORDER BY period DESC LIMIT 1",
            )
            .bind(register_id)
            .bind(period)
            .fetch_optional(&self.pool)
            .await?;
            let tariff = self.tariff_for(property_id, code, period).await?;
            prev_dates.push(previous.as_ref().and_then(|p| p.1.clone()));
            inputs.push(MeterInput {
                meter_id: code.into(),
                kind: kind_of(code),
                // Политика объекта: из каждого накопительного показания берётся целая часть до вычитания.
                precision: ReadingPrecision::FloorBeforeDelta,
                previous: previous.and_then(|p| p.0.parse().ok()),
                current: current.and_then(|c| c.0.parse().ok()),
                tariff: tariff.as_ref().ok().copied(),
                tariff_changed_in_period: matches!(
                    tariff,
                    Err(BillingError::TariffChangedInPeriod(_))
                ),
                replacement_adjustment: false,
            });
        }
        let sewer = if sewer_enabled {
            match self.tariff_for(property_id, "sewer", period).await? {
                Ok(tariff) => Some(SewerRule { tariff }),
                Err(e) => return user_err(explain(&e)),
            }
        } else {
            None
        };
        let bill =
            billing::calculate_with(&inputs, sewer).map_err(|e| AppError::User(explain(&e)))?;
        let snapshot = Snapshot {
            period: period.into(),
            currency: "RUB".into(),
            total: bill.total.to_string(),
            policy: "Из каждого накопительного показания берётся целая часть до вычитания; водоотведение = ХВС + ГВС".into(),
            lines: bill
                .lines
                .iter()
                .enumerate()
                .map(|(i, l)| SnapLine {
                    code: if l.kind == MeterKind::Sewer { "sewer".into() } else { l.meter_id.clone() },
                    previous_raw: l.previous_raw.to_string(),
                    current_raw: l.current_raw.to_string(),
                    previous: l.previous.to_string(),
                    current: l.current.to_string(),
                    usage: l.usage.to_string(),
                    price: l.tariff.to_string(),
                    amount: l.amount.to_string(),
                    previous_read_on: prev_dates.get(i).cloned().flatten(),
                })
                .collect(),
        };
        let json = serde_json::to_string(&snapshot).expect("snapshot serializes");
        let inserted = sqlx::query(
            "INSERT INTO bill (property_id, period, version, status, total, snapshot, created_at)
             VALUES (?, ?, (SELECT COALESCE(MAX(version), 0) + 1 FROM bill WHERE property_id = ? AND period = ?), 'draft', ?, ?, ?)",
        )
        .bind(property_id)
        .bind(period)
        .bind(property_id)
        .bind(period)
        .bind(bill.total.to_string())
        .bind(&json)
        .bind(now_iso(now))
        .execute(&self.pool)
        .await;
        match inserted {
            Ok(r) => Ok(Draft {
                bill_id: r.last_insert_rowid(),
                snapshot,
            }),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => user_err(format!(
                "За {period} счёт уже выставлен — повторно не считаю."
            )),
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) async fn draft_to_landlord(&self, property_id: i64, d: &Draft) -> Res<Reply> {
        let chat = self.landlord_chat(property_id).await?;
        let tenant: String =
            sqlx::query_scalar("SELECT tenant_name FROM tenancy WHERE property_id = ?")
                .bind(property_id)
                .fetch_one(&self.pool)
                .await?;
        // Фото-подтверждения показаний этого периода — владелец сверяет цифры с ними.
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT g.code, r.photo_file_id FROM reading r JOIN register g ON g.id = r.register_id
              WHERE g.property_id = ? AND r.period = ? AND r.kind = 'submitted' AND r.status = 'confirmed' AND r.photo_file_id IS NOT NULL",
        )
        .bind(property_id)
        .bind(&d.snapshot.period)
        .fetch_all(&self.pool)
        .await?;
        let mut photos = vec![];
        for (code, _, _) in REGISTERS {
            if let Some((_, file_id)) = rows.iter().find(|r| r.0 == code) {
                let value = d
                    .snapshot
                    .lines
                    .iter()
                    .find(|l| l.code == code)
                    .map_or(String::new(), |l| {
                        format!(": {}", num(l.current_raw.parse().unwrap_or_default()))
                    });
                photos.push((file_id.clone(), format!("{}{value}", label(code))));
            }
        }
        let proof_note = match photos.len() {
            0 => "Фото к показаниям не приложены.".to_string(),
            n if n == REGISTERS.len() => "Фото-подтверждения всех показаний — выше.".to_string(),
            n => format!(
                "Фото-подтверждения приложены к {n} из {} показаний (выше).",
                REGISTERS.len()
            ),
        };
        let mut reply = Reply::with_buttons(
            chat,
            format!(
                "📋 Расчёт за {} (черновик #{})\nАрендатор: {tenant}\n\n{}\n\n{proof_note}\nАрендатору пока ничего не отправлено.",
                d.snapshot.period,
                d.bill_id,
                format_bill(&d.snapshot)
            ),
            vec![vec![
                ("✅ Утвердить и отправить", format!("ap:{}", d.bill_id)),
                ("❌ Отклонить", format!("rj:{}", d.bill_id)),
            ]],
        );
        reply.photos = photos;
        Ok(reply)
    }

    async fn load_bill(&self, from: &Sender, id: i64) -> Res<(i64, String, String, Snapshot)> {
        // Счёт должен принадлежать объекту этого владельца.
        let row: Option<(i64, String, String, String)> = sqlx::query_as(
            "SELECT b.property_id, b.period, b.status, b.snapshot FROM bill b
               JOIN property p ON p.id = b.property_id JOIN app_user l ON l.id = p.landlord_id
              WHERE b.id = ? AND l.telegram_id = ?",
        )
        .bind(id)
        .bind(from.telegram_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((property_id, period, status, snap)) = row else {
            return user_err("Счёт не найден.");
        };
        let snap = serde_json::from_str(&snap)
            .map_err(|_| AppError::User("Повреждённый снимок счёта.".into()))?;
        Ok((property_id, period, status, snap))
    }

    async fn tenant_chat(&self, property_id: i64) -> Res<Option<i64>> {
        Ok(sqlx::query_scalar("SELECT u.chat_id FROM tenancy t JOIN app_user u ON u.id = t.tenant_user_id WHERE t.property_id = ?")
            .bind(property_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub(crate) async fn bill_approve(
        &self,
        from: &Sender,
        id: i64,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let (property_id, period, _, snap) = self.load_bill(from, id).await?;
        // Решение принимается ровно один раз: повторное нажатие ничего не рассылает.
        let done = sqlx::query(
            "UPDATE bill SET status = 'approved', decided_at = ? WHERE id = ? AND status = 'draft'",
        )
        .bind(now_iso(now))
        .bind(id)
        .execute(&self.pool)
        .await?;
        if done.rows_affected() != 1 {
            return Ok(vec![Reply::text(from.chat_id, "Этот счёт уже обработан.")]);
        }
        let mut replies = vec![];
        if let Some(chat) = self.tenant_chat(property_id).await? {
            replies.push(Reply::text(
                chat,
                format!(
                    "🧾 Счёт за {period}\n\n{}\n\nОплатите владельцу напрямую. Он отметит оплату в боте, и вы получите подтверждение.",
                    format_bill(&snap)
                ),
            ));
        }
        replies.push(Reply::with_buttons(
            from.chat_id,
            format!(
                "✅ Счёт за {period} утверждён и отправлен арендатору: {} ₽.",
                money(snap.total.parse().unwrap_or_default())
            ),
            vec![vec![("💰 Отметить оплаченным", format!("pd:{id}"))]],
        ));
        Ok(replies)
    }

    pub(crate) async fn bill_reject(
        &self,
        from: &Sender,
        id: i64,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let (property_id, period, _, _) = self.load_bill(from, id).await?;
        let done = sqlx::query(
            "UPDATE bill SET status = 'rejected', decided_at = ? WHERE id = ? AND status = 'draft'",
        )
        .bind(now_iso(now))
        .bind(id)
        .execute(&self.pool)
        .await?;
        if done.rows_affected() != 1 {
            return Ok(vec![Reply::text(from.chat_id, "Этот счёт уже обработан.")]);
        }
        // Показания периода открываются заново; история в таблице reading сохраняется (status = superseded).
        self.supersede_period(property_id, &period).await?;
        let mut replies = vec![];
        if let Some(chat) = self.tenant_chat(property_id).await? {
            replies.push(Reply::text(
                chat,
                format!("Владелец не подтвердил расчёт за {period}. Проверьте показания и отправьте их заново: фото или /enter."),
            ));
        }
        replies.push(Reply::text(
            from.chat_id,
            format!("❌ Расчёт за {period} отклонён. Показания сброшены, арендатор уведомлён."),
        ));
        Ok(replies)
    }

    pub(crate) async fn bill_paid(
        &self,
        from: &Sender,
        id: i64,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let (property_id, period, status, snap) = self.load_bill(from, id).await?;
        let done = sqlx::query(
            "UPDATE bill SET status = 'paid', paid_at = ? WHERE id = ? AND status = 'approved'",
        )
        .bind(now_iso(now))
        .bind(id)
        .execute(&self.pool)
        .await?;
        if done.rows_affected() != 1 {
            return Ok(vec![Reply::text(
                from.chat_id,
                if status == "paid" {
                    "Оплата уже отмечена."
                } else {
                    "Отметить оплату можно только у утверждённого счёта."
                },
            )]);
        }
        let mut replies = vec![];
        if let Some(chat) = self.tenant_chat(property_id).await? {
            replies.push(Reply::text(
                chat,
                format!(
                    "💰 Оплата за {period} ({} ₽) отмечена владельцем. Спасибо!",
                    money(snap.total.parse().unwrap_or_default())
                ),
            ));
        }
        replies.push(Reply::text(
            from.chat_id,
            format!("💰 Счёт за {period} отмечен оплаченным."),
        ));
        Ok(replies)
    }

    pub(crate) async fn paid_list(&self, from: &Sender) -> Res<Vec<Reply>> {
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT b.id, b.period, b.total FROM bill b JOIN property p ON p.id = b.property_id JOIN app_user l ON l.id = p.landlord_id
              WHERE b.status = 'approved' AND l.telegram_id = ? ORDER BY b.period",
        )
        .bind(from.telegram_id)
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(vec![Reply::text(
                from.chat_id,
                "Неоплаченных утверждённых счетов нет.",
            )]);
        }
        let buttons = rows
            .iter()
            .map(|(id, period, total)| {
                vec![Button {
                    label: format!(
                        "{period}: {} ₽ — оплачен",
                        money(total.parse().unwrap_or_default())
                    ),
                    data: format!("pd:{id}"),
                }]
            })
            .collect();
        Ok(vec![Reply {
            chat_id: from.chat_id,
            text: "Какой счёт оплачен?".into(),
            buttons,
            photos: vec![],
        }])
    }
}

/// Построчная расшифровка: показания, расход, тариф, сумма.
pub fn format_bill(s: &Snapshot) -> String {
    let d = |x: &str| x.parse::<Decimal>().unwrap_or_default();
    let usage_of = |code: &str| {
        s.lines
            .iter()
            .find(|l| l.code == code)
            .map(|l| d(&l.usage))
            .unwrap_or_default()
    };
    let mut out = vec![];
    for l in &s.lines {
        if l.code == "sewer" {
            out.push(format!(
                "• Водоотведение: {} + {} = {} м³ × {} = {} ₽",
                num(usage_of("cold")),
                num(usage_of("hot")),
                num(d(&l.usage)),
                money(d(&l.price)),
                money(d(&l.amount))
            ));
            continue;
        }
        let mut line = format!(
            "• {}: {} − {} = {} {} × {} = {} ₽",
            label(&l.code),
            num(d(&l.current)),
            num(d(&l.previous)),
            num(d(&l.usage)),
            unit(&l.code),
            money(d(&l.price)),
            money(d(&l.amount))
        );
        if l.current_raw != l.current || l.previous_raw != l.previous {
            line.push_str(&format!(
                " (счётчик: {} → {}; считается целая часть)",
                num(d(&l.previous_raw)),
                num(d(&l.current_raw))
            ));
        }
        out.push(line);
    }
    out.push(format!("\nИтого: {} ₽", money(d(&s.total))));
    out.join("\n")
}

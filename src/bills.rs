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
    }
}

fn kind_of(code: &str) -> MeterKind {
    match code {
        "cold" => MeterKind::ColdWater,
        "hot" => MeterKind::HotWater,
        _ => MeterKind::Electricity,
    }
}

/// Версия тарифа: действует с расчётного месяца `month` (`ГГГГ-ММ`) и дальше, пока её не заменит более поздняя.
/// `enabled` нужен водоотведению: выключение, как и цена, действует по месяцам.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TariffRow {
    pub month: String,
    pub price: Decimal,
    pub enabled: bool,
}

/// Тариф, действующий в расчётном месяце `period`: последняя версия с месяцем не позже него.
/// Внутри месяца тариф не меняется — он всегда относится к месяцу целиком (без пропорций и разбивки по дням).
pub fn effective_tariff<'a>(versions: &'a [TariffRow], period: &str) -> Option<&'a TariffRow> {
    versions
        .iter()
        .filter(|v| v.month.as_str() <= period)
        .max_by(|a, b| a.month.cmp(&b.month))
}

impl App {
    /// Версии тарифа позиции по возрастанию месяца.
    pub(crate) async fn tariff_versions(
        &self,
        property_id: i64,
        code: &str,
    ) -> Res<Vec<TariffRow>> {
        let rows: Vec<(String, String, bool)> = sqlx::query_as(
            "SELECT substr(valid_from, 1, 7), price, enabled FROM tariff WHERE property_id = ? AND code = ? ORDER BY valid_from ASC",
        )
        .bind(property_id)
        .bind(code)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(month, price, enabled)| {
                Some(TariffRow {
                    month,
                    price: price.parse().ok()?,
                    enabled,
                })
            })
            .collect())
    }

    /// Цена позиции за расчётный месяц целиком (см. `effective_tariff`).
    async fn tariff_for(
        &self,
        property_id: i64,
        code: &str,
        period: &str,
    ) -> Res<Result<Decimal, BillingError>> {
        let versions = self.tariff_versions(property_id, code).await?;
        Ok(match effective_tariff(&versions, period) {
            Some(t) => Ok(t.price),
            None => Err(BillingError::MissingTariff(code.into())),
        })
    }

    /// Водоотведение за месяц: нет настройки или выключено — строки в счёте нет. Включение действует по месяцам, как цена.
    pub(crate) async fn sewer_for(&self, property_id: i64, period: &str) -> Res<Option<SewerRule>> {
        let versions = self.tariff_versions(property_id, "sewer").await?;
        Ok(effective_tariff(&versions, period)
            .filter(|t| t.enabled)
            .map(|t| SewerRule { tariff: t.price }))
    }

    /// Считает снимок счёта за период по подтверждённым показаниям и тарифам, действующим в этом месяце.
    pub(crate) async fn compute_draft(&self, property_id: i64, period: &str) -> Res<Snapshot> {
        let mut inputs = Vec::with_capacity(4);
        let mut prev_dates: Vec<Option<String>> = Vec::new();
        for (code, _, _) in self.registers_of(property_id).await? {
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
                tariff: tariff.ok(),
                replacement_adjustment: false,
            });
        }
        let sewer = self.sewer_for(property_id, period).await?;
        let bill =
            billing::calculate_with(&inputs, sewer).map_err(|e| AppError::User(explain(&e)))?;
        Ok(Snapshot {
            period: period.into(),
            currency: "RUB".into(),
            total: bill.total.to_string(),
            policy: "Из каждого накопительного показания берётся целая часть до вычитания; водоотведение = ХВС + ГВС; тарифы — по расчётному месяцу целиком".into(),
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
        })
    }

    /// Создаёт черновик счёта за период. Дубль за период невозможен (уникальный индекс).
    pub(crate) async fn create_draft(
        &self,
        property_id: i64,
        period: &str,
        now: NaiveDateTime,
    ) -> Res<Draft> {
        let snapshot = self.compute_draft(property_id, period).await?;
        self.insert_draft(property_id, period, snapshot, now).await
    }

    pub(crate) async fn insert_draft(
        &self,
        property_id: i64,
        period: &str,
        snapshot: Snapshot,
        now: NaiveDateTime,
    ) -> Res<Draft> {
        let json = serde_json::to_string(&snapshot).expect("snapshot serializes");
        let inserted = sqlx::query(
            "INSERT INTO bill (property_id, period, version, status, total, snapshot, created_at)
             VALUES (?, ?, (SELECT COALESCE(MAX(version), 0) + 1 FROM bill WHERE property_id = ? AND period = ?), 'draft', ?, ?, ?)",
        )
        .bind(property_id)
        .bind(period)
        .bind(property_id)
        .bind(period)
        .bind(&snapshot.total)
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

    /// Тарифы с месяца `from_month` изменились: черновики этого и последующих месяцев пересчитываются по новым тарифам.
    /// Утверждённые и оплаченные счета не затрагиваются никогда.
    pub(crate) async fn refresh_drafts(
        &self,
        property_id: i64,
        from_month: &str,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let drafts: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, period, snapshot FROM bill WHERE property_id = ? AND status = 'draft' AND period >= ? ORDER BY period",
        )
        .bind(property_id)
        .bind(from_month)
        .fetch_all(&self.pool)
        .await?;
        let chat = self.landlord_chat(property_id).await?;
        let mut replies = vec![];
        for (id, period, old) in drafts {
            let old_json = serde_json::from_str::<Snapshot>(&old)
                .ok()
                .and_then(|s| serde_json::to_string(&s).ok());
            let fresh = self.compute_draft(property_id, &period).await;
            if let (Ok(new), Some(old_json)) = (&fresh, &old_json)
                && serde_json::to_string(new).ok().as_deref() == Some(old_json.as_str())
            {
                continue; // тарифы изменились, но этот месяц они не затронули
            }
            let voided = sqlx::query("UPDATE bill SET status = 'void', note = 'тарифы изменились', decided_at = ? WHERE id = ? AND status = 'draft'")
                .bind(now_iso(now))
                .bind(id)
                .execute(&self.pool)
                .await?;
            if voided.rows_affected() != 1 {
                continue;
            }
            match fresh {
                Ok(snapshot) => {
                    let draft = self.insert_draft(property_id, &period, snapshot, now).await?;
                    replies.push(Reply::text(chat, format!("🔄 Тарифы изменились — черновик за {period} пересчитан по новым тарифам. Прежний черновик отменён.")));
                    replies.push(self.draft_to_landlord(property_id, &draft).await?);
                }
                Err(AppError::User(reason)) => replies.push(Reply::text(
                    chat,
                    format!("🔄 Тарифы изменились — прежний черновик за {period} отменён, но пересчитать не удалось:\n{reason}\nИсправьте причину и нажмите /calc."),
                )),
                Err(e) => return Err(e),
            }
        }
        Ok(replies)
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
            "SELECT g.code, CASE WHEN r.photo_file_id LIKE 'local:%' THEN 'local:' || r.photo_path ELSE r.photo_file_id END
               FROM reading r JOIN register g ON g.id = r.register_id
              WHERE g.property_id = ? AND r.period = ? AND r.kind = 'submitted' AND r.status = 'confirmed' AND r.photo_file_id IS NOT NULL",
        )
        .bind(property_id)
        .bind(&d.snapshot.period)
        .fetch_all(&self.pool)
        .await?;
        let regs = self.registers_of(property_id).await?;
        let mut photos = vec![];
        for (code, _, _) in regs.iter().copied() {
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
            n if n == regs.len() => "Фото-подтверждения всех показаний — выше.".to_string(),
            n => format!(
                "Фото-подтверждения приложены к {n} из {} показаний (выше).",
                regs.len()
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

    pub(crate) async fn load_bill(
        &self,
        from: &Sender,
        id: i64,
    ) -> Res<(i64, String, String, Snapshot)> {
        // Счёт должен принадлежать объекту этого владельца.
        let row: Option<(i64, String, String, String)> = sqlx::query_as(
            "SELECT b.property_id, b.period, b.status, b.snapshot FROM bill b
               JOIN membership m ON m.property_id = b.property_id AND m.role = 'owner' JOIN app_user l ON l.id = m.user_id
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

    pub(crate) async fn tenant_chat(&self, property_id: i64) -> Res<Option<i64>> {
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
        let (property_id, period, status, snap) = self.load_bill(from, id).await?;
        // Решение принимается ровно один раз: повторное нажатие ничего не рассылает.
        // срок оплаты фиксируется в момент утверждения: число дней задаёт владелец (по умолчанию 10)
        let payment_days: i64 =
            sqlx::query_scalar("SELECT payment_days FROM property WHERE id = ?")
                .bind(property_id)
                .fetch_one(&self.pool)
                .await?;
        let due = now.date() + chrono::Duration::days(payment_days);
        let done = sqlx::query(
            "UPDATE bill SET status = 'approved', decided_at = ?, due_date = ? WHERE id = ? AND status = 'draft'",
        )
        .bind(now_iso(now))
        .bind(due.to_string())
        .bind(id)
        .execute(&self.pool)
        .await?;
        if done.rows_affected() != 1 {
            return Ok(vec![Reply::text(from.chat_id, already_decided(&status))]);
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
        let (property_id, period, status, _) = self.load_bill(from, id).await?;
        let done = sqlx::query(
            "UPDATE bill SET status = 'rejected', decided_at = ? WHERE id = ? AND status = 'draft'",
        )
        .bind(now_iso(now))
        .bind(id)
        .execute(&self.pool)
        .await?;
        if done.rows_affected() != 1 {
            return Ok(vec![Reply::text(from.chat_id, already_decided(&status))]);
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
            "SELECT b.id, b.period, b.total FROM bill b JOIN membership m ON m.property_id = b.property_id AND m.role = 'owner' JOIN app_user l ON l.id = m.user_id
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

/// Почему кнопка под счётом больше не работает.
fn already_decided(status: &str) -> &'static str {
    if status == "void" {
        "Этот черновик устарел: тарифы изменились. Откройте обновлённый расчёт."
    } else {
        "Этот счёт уже обработан."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn v(month: &str, price: Decimal) -> TariffRow {
        TariffRow {
            month: month.into(),
            price,
            enabled: true,
        }
    }

    #[test]
    fn the_latest_version_not_after_the_month_applies_to_the_whole_month() {
        let vs = [v("2026-08", dec!(8)), v("2026-10", dec!(9))];
        assert_eq!(effective_tariff(&vs, "2026-07"), None);
        assert_eq!(effective_tariff(&vs, "2026-09").unwrap().price, dec!(8));
        assert_eq!(effective_tariff(&vs, "2026-10").unwrap().price, dec!(9));
        // тариф переносится на следующие месяцы, пока не заменён
        assert_eq!(effective_tariff(&vs, "2027-03").unwrap().price, dec!(9));
    }
}

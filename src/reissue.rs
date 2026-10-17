//! Перевыпуск: единственный явный способ исправить уже утверждённый счёт. Старый счёт не редактируется — он получает
//! статус «заменён» (сумма и состав остаются в базе как были), по тем же показаниям считается новый черновик, который
//! владелец утверждает как обычно. Оплаченные счета не перевыпускаются (корректировка платежей — вне MVP).

use crate::app::*;
use crate::bills::format_bill;
use crate::util::*;
use chrono::NaiveDateTime;
use rust_decimal::Decimal;

impl App {
    /// `/reissue`: утверждённые, но неоплаченные счета.
    pub(crate) async fn reissue_list(&self, from: &Sender) -> Res<Vec<Reply>> {
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
                "Нет утверждённых неоплаченных счетов, которые можно выпустить заново. Оплаченные счета не перевыпускаются.",
            )]);
        }
        let buttons = rows
            .iter()
            .map(|(id, period, total)| {
                vec![Button {
                    label: format!("{period}: {} ₽", money(total.parse().unwrap_or_default())),
                    data: format!("ri:{id}"),
                }]
            })
            .collect();
        Ok(vec![Reply {
            chat_id: from.chat_id,
            text: "Какой счёт выпустить заново по текущим тарифам? Старый счёт останется в истории со статусом «заменён».".into(),
            buttons,
            photos: vec![],
        }])
    }

    /// Показывает, что изменится, и просит подтверждения.
    pub(crate) async fn reissue_ask(
        &self,
        from: &Sender,
        id: i64,
        _now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let (property_id, period, status, old) = self.load_bill(from, id).await?;
        match status.as_str() {
            "approved" => {}
            "paid" => {
                return user_err(
                    "Счёт уже оплачен — перевыпуск невозможен. Корректировка оплаченных счетов в этой версии не поддерживается.",
                );
            }
            _ => return user_err("Перевыпустить можно только утверждённый неоплаченный счёт."),
        }
        let fresh = self.compute_draft(property_id, &period).await?;
        let (old_total, new_total): (Decimal, Decimal) = (
            old.total.parse().unwrap_or_default(),
            fresh.total.parse().unwrap_or_default(),
        );
        if old_total == new_total {
            return Ok(vec![Reply::text(
                from.chat_id,
                format!(
                    "По текущим тарифам счёт за {period} получился бы тем же ({} ₽) — перевыпуск не нужен.",
                    money(old_total)
                ),
            )]);
        }
        Ok(vec![Reply::with_buttons(
            from.chat_id,
            format!(
                "Выпустить счёт за {period} заново?\n\nСейчас выставлен: {} ₽.\nПо текущим тарифам: {} ₽.\n\n{}\n\nСтарый счёт станет «заменённым», арендатор получит уведомление. Новый счёт вы утвердите отдельно.",
                money(old_total),
                money(new_total),
                format_bill(&fresh)
            ),
            vec![vec![
                ("✅ Выпустить заново", format!("rc:{id}")),
                ("Оставить как есть", "rx".into()),
            ]],
        )])
    }

    pub(crate) async fn reissue_do(
        &self,
        from: &Sender,
        id: i64,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let (property_id, period, status, _) = self.load_bill(from, id).await?;
        if status != "approved" {
            return Ok(vec![Reply::text(from.chat_id, "Этот счёт уже обработан.")]);
        }
        // сначала считаем: если расчёт невозможен, старый счёт остаётся нетронутым
        let fresh = self.compute_draft(property_id, &period).await?;
        let voided = sqlx::query("UPDATE bill SET status = 'void', note = 'перевыпущен владельцем', decided_at = ? WHERE id = ? AND status = 'approved'")
            .bind(now_iso(now))
            .bind(id)
            .execute(&self.pool)
            .await?;
        if voided.rows_affected() != 1 {
            return Ok(vec![Reply::text(from.chat_id, "Этот счёт уже обработан.")]);
        }
        let draft = self.insert_draft(property_id, &period, fresh, now).await?;
        let mut replies = vec![];
        if let Some(chat) = self.tenant_chat(property_id).await? {
            replies.push(Reply::text(
                chat,
                format!("Владелец отозвал счёт за {period} и выставит новый. Предыдущую сумму не оплачивайте — дождитесь нового счёта."),
            ));
        }
        replies.push(Reply::text(from.chat_id, format!("♻️ Счёт за {period} отозван (статус «заменён»). Новый черновик ниже — проверьте и утвердите.")));
        replies.push(self.draft_to_landlord(property_id, &draft).await?);
        Ok(replies)
    }
}

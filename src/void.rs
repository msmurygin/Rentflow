//! `/void`: аннулирование утверждённого, но неоплаченного счёта (например, тестового).
//! Три шага — счёт, причина текстом, явное подтверждение. Оплаченный счёт не аннулируется никогда.
//! Исходный счёт остаётся в базе со статусом `void` и причиной; показания периода открываются заново.

use crate::app::*;
use crate::util::*;
use chrono::NaiveDateTime;

impl App {
    pub(crate) async fn void_list(&self, from: &Sender) -> Res<Vec<Reply>> {
        let rows: Vec<(i64, String, String, String)> = sqlx::query_as(
            "SELECT b.id, b.period, b.total, b.status FROM bill b JOIN property p ON p.id = b.property_id JOIN app_user l ON l.id = p.landlord_id
              WHERE b.status IN ('approved', 'paid') AND l.telegram_id = ? ORDER BY b.period",
        )
        .bind(from.telegram_id)
        .fetch_all(&self.pool)
        .await?;
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|r| r.3 != "paid" || self.cfg.allow_void_paid)
            .collect();
        if rows.is_empty() {
            return user_err(
                "Нет утверждённых неоплаченных счетов. Оплаченный счёт аннулировать нельзя; черновик отклоняется кнопкой «Отклонить» под расчётом.",
            );
        }
        let buttons = rows
            .iter()
            .map(|(id, period, total, status)| {
                vec![Button {
                    label: format!(
                        "{period}: {} ₽{}",
                        money(total.parse().unwrap_or_default()),
                        if status == "paid" {
                            " (оплачен)"
                        } else {
                            ""
                        }
                    ),
                    data: format!("vo:{id}"),
                }]
            })
            .collect();
        Ok(vec![Reply {
            chat_id: from.chat_id,
            text: "Какой счёт аннулировать? Арендатор получит уведомление, показания периода откроются заново. Оплаченный счёт аннулируется только с отдельным подтверждением.".into(),
            buttons,
            photos: vec![],
        }])
    }

    /// Выбран счёт: просим причину.
    pub(crate) async fn void_pick(&self, from: &Sender, id: i64) -> Res<Vec<Reply>> {
        let (_, period, status, _) = self.load_bill(from, id).await?;
        if status != "approved" && !(status == "paid" && self.cfg.allow_void_paid) {
            return user_err(if status == "paid" {
                "Счёт уже оплачен — аннулировать его нельзя."
            } else {
                "Аннулировать можно только утверждённый неоплаченный счёт."
            });
        }
        self.set_state(
            from.telegram_id,
            &State::Void {
                bill_id: id,
                reason: None,
                paid: status == "paid",
            },
        )
        .await?;
        Ok(vec![Reply::with_buttons(
            from.chat_id,
            format!(
                "Счёт за {period}. Напишите причину аннулирования одним сообщением (/cancel — отмена)."
            ),
            vec![vec![("🗑 Отмена", "vx".into())]],
        )])
    }

    /// Причина получена: показываем итог и просим подтвердить.
    pub(crate) async fn void_reason(&self, from: &Sender, text: &str) -> Res<Vec<Reply>> {
        let Some(State::Void { bill_id, paid, .. }) = self.state(from.telegram_id).await? else {
            return user_err("Нет начатого аннулирования. Начните с /void.");
        };
        let reason = text.trim();
        if reason.is_empty() || reason.chars().count() > 200 {
            return user_err("Причина — от 1 до 200 символов. Напишите ещё раз.");
        }
        let (_, period, _, snap) = self.load_bill(from, bill_id).await?;
        self.set_state(
            from.telegram_id,
            &State::Void {
                bill_id,
                reason: Some(reason.to_string()),
                paid,
            },
        )
        .await?;
        Ok(vec![Reply::with_buttons(
            from.chat_id,
            format!(
                "Аннулировать счёт за {period} на {} ₽?\nПричина: {reason}\n\n{}Арендатор получит уведомление. Счёт останется в истории со статусом «аннулирован». Показания периода откроются заново.",
                money(snap.total.parse().unwrap_or_default()),
                if paid {
                    "⚠️ Счёт уже ОПЛАЧЕН. Аннулирование не возвращает деньги — расчёт с арендатором придётся уладить самим.\n\n"
                } else {
                    ""
                }
            ),
            vec![vec![
                (
                    if paid {
                        "✅ Да, аннулировать оплаченный"
                    } else {
                        "✅ Аннулировать"
                    },
                    format!("vc:{bill_id}"),
                ),
                ("🗑 Отмена", "vx".into()),
            ]],
        )])
    }

    pub(crate) async fn void_cancel(&self, from: &Sender) -> Res<Vec<Reply>> {
        self.clear_state(from.telegram_id).await?;
        Ok(vec![Reply::text(
            from.chat_id,
            "Аннулирование отменено, счёт не тронут.",
        )])
    }

    /// Подтверждение. Состояние снимается до действия, а UPDATE срабатывает только для того статуса, который владелец видел
    /// при подтверждении: повторное нажатие ничего не аннулирует и никому ничего не рассылает.
    pub(crate) async fn void_confirm(
        &self,
        from: &Sender,
        id: i64,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let state = self.state(from.telegram_id).await?;
        self.clear_state(from.telegram_id).await?;
        let Some(State::Void {
            bill_id,
            reason: Some(reason),
            paid,
        }) = state
        else {
            return user_err("Это подтверждение устарело или уже использовано. Начните с /void.");
        };
        if bill_id != id {
            return user_err("Это подтверждение относится к другому счёту. Начните с /void.");
        }
        self.void_bill(from, id, &reason, paid, now).await
    }

    /// Общая операция для бота и Mini App. `expect_paid` — оплаченным ли владелец видел счёт, подтверждая:
    /// если статус изменился в промежутке, ничего не делаем.
    pub(crate) async fn void_bill(
        &self,
        from: &Sender,
        id: i64,
        reason: &str,
        expect_paid: bool,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let (property_id, period, status, snap) = self.load_bill(from, id).await?;
        if status == "paid" && !self.cfg.allow_void_paid {
            return user_err("Оплаченный счёт аннулировать нельзя.");
        }
        if status != "approved" && status != "paid" {
            return user_err("Счёт уже изменён (аннулирован или заменён) — ничего не сделано.");
        }
        if (status == "paid") != expect_paid {
            return user_err(
                "Статус счёта изменился, пока вы подтверждали (оплата отмечена). Начните с /void.",
            );
        }
        let done = sqlx::query(
            "UPDATE bill SET status = 'void', note = ?, decided_at = ? WHERE id = ? AND status = ?",
        )
        .bind(format!(
            "аннулирован владельцем{}: {reason}",
            if status == "paid" {
                " (был оплачен)"
            } else {
                ""
            }
        ))
        .bind(now_iso(now))
        .bind(id)
        .bind(&status)
        .execute(&self.pool)
        .await?;
        if done.rows_affected() != 1 {
            return user_err("Счёт уже изменён — ничего не сделано.");
        }
        self.supersede_period(property_id, &period).await?;
        sqlx::query("INSERT INTO audit_log (at, actor, action, entity, entity_id, details) VALUES (?, ?, 'bill_void', 'bill', ?, ?)")
            .bind(now_iso(now))
            .bind(from.telegram_id)
            .bind(id.to_string())
            .bind(serde_json::json!({ "period": period, "total": snap.total, "reason": reason, "was_paid": status == "paid" }).to_string())
            .execute(&self.pool)
            .await?;
        let mut replies = vec![];
        if let Some(chat) = self.tenant_chat(property_id).await? {
            replies.push(Reply::text(
                chat,
                format!(
                    "⚠️ Счёт за {period} ({} ₽) аннулирован владельцем: {reason}\n{}Введите показания заново: /enter",
                    money(snap.total.parse().unwrap_or_default()),
                    if status == "paid" { "Свяжитесь с владельцем по поводу уже сделанной оплаты.\n" } else { "Оплачивать его не нужно.\n" }
                ),
            ));
        }
        replies.push(Reply::text(
            from.chat_id,
            format!("🗑 Счёт за {period} аннулирован. Показания периода открыты заново, тарифы и настройки объекта не тронуты."),
        ));
        Ok(replies)
    }
}

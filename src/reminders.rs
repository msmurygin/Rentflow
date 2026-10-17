//! Расписание передачи показаний и напоминания. День месяца (1–28) задаёт владелец; бот напоминает арендатору накануне,
//! в срок и три дня после просрочки, а владельцу сообщает один раз о просрочке. Каждое напоминание — раз за период.

use crate::app::*;
use crate::util::*;
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Timelike};

/// Час (местного времени), раньше которого напоминания не отправляются.
pub const REMIND_HOUR: u32 = 10;

/// Правило срока передачи показаний. Число 29–31 — «последний день месяца»: так срок есть в любом месяце (в феврале — 28/29-е).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingRule {
    Day(i64),
    LastDay,
}

impl ReadingRule {
    /// Ввод владельца: 1–28 — это число месяца, 29–31 — последний день месяца.
    pub fn from_input(day: i64) -> Option<Self> {
        match day {
            1..=28 => Some(Self::Day(day)),
            29..=31 => Some(Self::LastDay),
            _ => None,
        }
    }

    /// Число для API/интерфейса: 1–28 как есть, 31 — «последний день».
    pub fn api_day(self) -> i64 {
        match self {
            Self::Day(d) => d,
            Self::LastDay => 31,
        }
    }

    /// Срок за расчётный месяц `period`.
    pub fn due(self, period: &str) -> Option<NaiveDate> {
        let (first, last) = period_bounds(period)?;
        Some(match self {
            Self::Day(d) => first + Duration::days(d - 1),
            Self::LastDay => last,
        })
    }

    pub fn describe(self) -> String {
        match self {
            Self::Day(d) => format!("до {d}-го числа каждого месяца"),
            Self::LastDay => "в последний день каждого месяца".to_string(),
        }
    }
}

/// Какие напоминания нужны при `days_late` днях после срока (отрицательное — до срока): в день срока — первое,
/// через 2 дня — повторное (и один раз сообщение владельцу). Остальные дни — тишина, чтобы не спамить.
/// Окно повторного шире одного дня: после простоя бота оно всё равно уйдёт (журнал не даёт повторов).
pub fn due_kinds(days_late: i64) -> Vec<&'static str> {
    match days_late {
        0 => vec!["due"],
        2..=4 => vec!["repeat", "landlord_late"],
        _ => vec![],
    }
}

impl App {
    /// Задаёт правило срока передачи показаний (`None` — снять расписание).
    pub async fn set_reading_rule(&self, property_id: i64, rule: Option<ReadingRule>) -> Res<()> {
        let (day, last) = match rule {
            None => (None, false),
            Some(ReadingRule::Day(d)) => (Some(d), false),
            Some(ReadingRule::LastDay) => (None, true),
        };
        sqlx::query("UPDATE property SET reading_day = ?, reading_last = ? WHERE id = ?")
            .bind(day)
            .bind(last)
            .bind(property_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Ввод числа (1–31; 0 — снять). Всё остальное — понятная ошибка.
    pub async fn set_reading_day(&self, property_id: i64, input: Option<i64>) -> Res<()> {
        let rule = match input {
            None | Some(0) => None,
            Some(d) => Some(ReadingRule::from_input(d).ok_or_else(|| {
                AppError::User("День передачи показаний — число от 1 до 31 (29–31 означает последний день месяца).".into())
            })?),
        };
        self.set_reading_rule(property_id, rule).await
    }

    /// Срок оплаты счёта в днях после утверждения (1–90).
    pub async fn set_payment_days(&self, property_id: i64, days: i64) -> Res<()> {
        if !(1..=90).contains(&days) {
            return user_err("Срок оплаты — от 1 до 90 дней.");
        }
        sqlx::query("UPDATE property SET payment_days = ? WHERE id = ?")
            .bind(days)
            .bind(property_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(crate) async fn reading_rule(&self, property_id: i64) -> Res<Option<ReadingRule>> {
        let (day, last): (Option<i64>, bool) =
            sqlx::query_as("SELECT reading_day, reading_last FROM property WHERE id = ?")
                .bind(property_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(if last {
            Some(ReadingRule::LastDay)
        } else {
            day.map(ReadingRule::Day)
        })
    }

    /// `/schedule` — показать, `/schedule 25` — задать (29–31 = последний день месяца), `/schedule off` — снять.
    pub(crate) async fn schedule_cmd(&self, from: &Sender, text: &str) -> Res<Vec<Reply>> {
        let Some(property_id) = self.owner_property(from.telegram_id).await? else {
            return user_err("Сначала настройте объект: /setup");
        };
        let arg = text.split_whitespace().nth(1);
        match arg {
            None => {
                let msg = match self.reading_rule(property_id).await? {
                    Some(rule) => format!("Арендатор передаёт показания {}. Изменить: /schedule <день 1–31>, снять: /schedule off.", rule.describe()),
                    None => "Расписание не задано. Задать: /schedule <день>, например /schedule 25 (29–31 — последний день месяца). Бот напомнит арендатору в день срока и через 2 дня.".to_string(),
                };
                Ok(vec![Reply::text(from.chat_id, msg)])
            }
            Some("off" | "нет") => {
                self.set_reading_rule(property_id, None).await?;
                Ok(vec![Reply::text(
                    from.chat_id,
                    "Расписание снято, напоминаний не будет.",
                )])
            }
            Some(v) => {
                let Some(day) = v.parse::<i64>().ok().filter(|d| *d != 0) else {
                    return user_err("Нужно число от 1 до 31, например /schedule 25.");
                };
                self.set_reading_day(property_id, Some(day)).await?;
                let rule = ReadingRule::from_input(day).expect("проверено выше");
                Ok(vec![Reply::text(
                    from.chat_id,
                    format!(
                        "Срок передачи показаний: {}. Бот напомнит арендатору в день срока и через 2 дня; вам сообщит, если срок сорван.",
                        rule.describe()
                    ),
                )])
            }
        }
    }

    /// Напоминания, которые пора отправить. `utc` — текущее время UTC: у каждой квартиры свой часовой пояс, по нему
    /// определяются «сегодня» и «после 10:00». Повторный вызов ничего не даёт (журнал `reminder_log`), перезапуск безопасен.
    pub async fn due_reminders(&self, utc: NaiveDateTime) -> Res<Vec<Reply>> {
        let props: Vec<(i64, i64)> = sqlx::query_as("SELECT id, tz_offset_hours FROM property")
            .fetch_all(&self.pool)
            .await?;
        let mut out = vec![];
        for (property_id, tz) in props {
            let now = utc + Duration::hours(tz);
            if now.hour() < REMIND_HOUR {
                continue;
            }
            let Some(rule) = self.reading_rule(property_id).await? else {
                continue;
            };
            let Some(tenant) = self.tenant_chat(property_id).await? else {
                continue;
            };
            let period = self.open_period(property_id, now).await?;
            // показания уже отправлены (идёт расчёт) — напоминать нечего
            if self.period_billed(property_id, &period).await?
                || self.received(property_id, &period).await?.len()
                    >= self.registers_of(property_id).await?.len()
            {
                continue;
            }
            let Some(due) = rule.due(&period) else {
                continue;
            };
            let days_late = (now.date() - due).num_days();
            for kind in due_kinds(days_late) {
                if !self.first_reminder(property_id, &period, kind, now).await? {
                    continue;
                }
                let date = format!("{:02}.{:02}", due.day(), due.month());
                if kind == "landlord_late" {
                    out.push(Reply::text(
                        self.landlord_chat(property_id).await?,
                        format!("⏰ Арендатор не передал показания за {period} (срок был {date}). Ему отправлено повторное напоминание."),
                    ));
                    continue;
                }
                let text = match kind {
                    "due" => format!("⏰ Сегодня ({date}) срок передачи показаний за {period}."),
                    _ => format!(
                        "⏰ Показания за {period} ещё не получены (срок был {date}). Передайте их, пожалуйста."
                    ),
                };
                out.push(Reply::with_buttons(
                    tenant,
                    text,
                    vec![vec![("▶️ Ввести показания", "enter".into())]],
                ));
            }
        }
        out.extend(self.payment_reminders(utc).await?);
        // демо-пользователи (отрицательные id) — не люди в Telegram: им ничего не отправляем
        out.retain(|r| r.chat_id > 0);
        Ok(out)
    }

    /// Срок оплаты утверждённого счёта: в день срока — напоминание арендатору, через 2 дня после срока — повторное и один раз
    /// владельцу. «Оплачено» ставит только владелец: пока он не отметил, счёт считается неоплаченным.
    async fn payment_reminders(&self, utc: NaiveDateTime) -> Res<Vec<Reply>> {
        let rows: Vec<(i64, String, String, String, i64)> = sqlx::query_as(
            "SELECT b.property_id, b.period, b.total, b.due_date, p.tz_offset_hours FROM bill b JOIN property p ON p.id = b.property_id
              WHERE b.status = 'approved' AND b.due_date IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = vec![];
        for (property_id, period, total, due, tz) in rows {
            let now = utc + Duration::hours(tz);
            if now.hour() < REMIND_HOUR {
                continue;
            }
            let Some(due) = NaiveDate::parse_from_str(&due, "%Y-%m-%d").ok() else {
                continue;
            };
            let sum = money(total.parse().unwrap_or_default());
            let Some(tenant) = self.tenant_chat(property_id).await? else {
                continue;
            };
            let kinds: Vec<&str> = match (now.date() - due).num_days() {
                0 => vec!["pay_due"],
                2..=4 => vec!["pay_overdue", "pay_overdue_landlord"],
                _ => vec![],
            };
            for kind in kinds {
                if !self.first_reminder(property_id, &period, kind, now).await? {
                    continue;
                }
                let date = format!("{:02}.{:02}", due.day(), due.month());
                out.push(match kind {
                    "pay_due" => Reply::text(tenant, format!("💳 Сегодня ({date}) срок оплаты счёта за {period}: {sum} ₽.")),
                    "pay_overdue" => Reply::text(tenant, format!("💳 Срок оплаты счёта за {period} ({sum} ₽) прошёл {date}. Если вы уже заплатили — сообщите владельцу, он отметит оплату.")),
                    _ => Reply::text(self.landlord_chat(property_id).await?, format!("💳 Счёт за {period} ({sum} ₽) не оплачен: срок был {date}. Когда получите деньги — отметьте оплату (/paid).")),
                });
            }
        }
        Ok(out)
    }

    /// Записывает отправку в журнал; `true` — это первая отправка такого напоминания за период (можно слать).
    pub(crate) async fn first_reminder(
        &self,
        property_id: i64,
        period: &str,
        kind: &str,
        now: NaiveDateTime,
    ) -> Res<bool> {
        Ok(sqlx::query("INSERT OR IGNORE INTO reminder_log (property_id, period, kind, sent_at) VALUES (?, ?, ?, ?)")
            .bind(property_id)
            .bind(period)
            .bind(kind)
            .bind(now_iso(now))
            .execute(&self.pool)
            .await?
            .rows_affected()
            == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminders_come_on_the_day_and_two_days_later_only() {
        assert!(due_kinds(-1).is_empty());
        assert_eq!(due_kinds(0), ["due"]);
        assert!(due_kinds(1).is_empty());
        assert_eq!(due_kinds(2), ["repeat", "landlord_late"]);
        assert_eq!(due_kinds(4), ["repeat", "landlord_late"]);
        assert!(due_kinds(5).is_empty());
    }

    #[test]
    fn day_29_to_31_means_the_last_day_of_each_month() {
        let d = |y, m, d| NaiveDate::from_ymd_opt(y, m, d);
        assert_eq!(ReadingRule::from_input(25), Some(ReadingRule::Day(25)));
        for n in [29, 30, 31] {
            assert_eq!(ReadingRule::from_input(n), Some(ReadingRule::LastDay));
        }
        assert_eq!(ReadingRule::from_input(0), None);
        assert_eq!(ReadingRule::from_input(32), None);
        assert_eq!(ReadingRule::LastDay.due("2026-02"), d(2026, 2, 28));
        assert_eq!(
            ReadingRule::LastDay.due("2028-02"),
            d(2028, 2, 29),
            "високосный год"
        );
        assert_eq!(ReadingRule::LastDay.due("2026-04"), d(2026, 4, 30));
        assert_eq!(ReadingRule::LastDay.due("2026-12"), d(2026, 12, 31));
        assert_eq!(ReadingRule::Day(25).due("2026-10"), d(2026, 10, 25));
        assert_eq!(ReadingRule::LastDay.api_day(), 31);
        assert!(ReadingRule::LastDay.describe().contains("последний день"));
    }
}

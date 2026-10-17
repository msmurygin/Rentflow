//! Расписание передачи показаний и напоминания. День месяца (1–28) задаёт владелец; бот напоминает арендатору накануне,
//! в срок и три дня после просрочки, а владельцу сообщает один раз о просрочке. Каждое напоминание — раз за период.

use crate::app::*;
use crate::util::*;
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Timelike};

/// Час (местного времени), раньше которого напоминания не отправляются.
pub const REMIND_HOUR: u32 = 10;

/// Какие напоминания нужны при `days_late` днях после срока (отрицательное — до срока).
/// Возвращается только самое свежее для арендатора: после простоя бот не засыпает его старыми.
pub fn due_kinds(days_late: i64) -> Vec<&'static str> {
    match days_late {
        -1 => vec!["before"],
        0 => vec!["due"],
        1 => vec!["late1"],
        2 => vec!["late2"],
        3..=5 => vec!["late3", "landlord_late"],
        _ => vec![],
    }
}

/// Срок передачи за расчётный месяц `period` при дне `day`.
pub fn due_date(period: &str, day: i64) -> Option<NaiveDate> {
    let (first, _) = period_bounds(period)?;
    Some(first + Duration::days(day - 1))
}

impl App {
    /// Задаёт день передачи показаний (`None` — снять расписание).
    pub async fn set_reading_day(&self, property_id: i64, day: Option<i64>) -> Res<()> {
        if let Some(d) = day
            && !(1..=28).contains(&d)
        {
            return user_err(
                "День передачи показаний — число от 1 до 28 (так срок есть в каждом месяце).",
            );
        }
        sqlx::query("UPDATE property SET reading_day = ? WHERE id = ?")
            .bind(day)
            .bind(property_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub(crate) async fn reading_day(&self, property_id: i64) -> Res<Option<i64>> {
        Ok(
            sqlx::query_scalar("SELECT reading_day FROM property WHERE id = ?")
                .bind(property_id)
                .fetch_one(&self.pool)
                .await?,
        )
    }

    /// `/schedule` — показать, `/schedule 25` — задать, `/schedule off` — снять.
    pub(crate) async fn schedule_cmd(&self, from: &Sender, text: &str) -> Res<Vec<Reply>> {
        let Some(property_id) = self.property_id().await? else {
            return user_err("Сначала настройте объект: /setup");
        };
        let arg = text.split_whitespace().nth(1);
        match arg {
            None => {
                let msg = match self.reading_day(property_id).await? {
                    Some(d) => format!("Арендатор передаёт показания до {d}-го числа каждого месяца. Изменить: /schedule <день 1–28>, снять: /schedule off."),
                    None => "Расписание не задано. Задать: /schedule <день 1–28>, например /schedule 25. Бот будет напоминать арендатору накануне, в срок и после просрочки.".to_string(),
                };
                Ok(vec![Reply::text(from.chat_id, msg)])
            }
            Some("off" | "нет") => {
                self.set_reading_day(property_id, None).await?;
                Ok(vec![Reply::text(
                    from.chat_id,
                    "Расписание снято, напоминаний не будет.",
                )])
            }
            Some(v) => {
                let Some(day) = v.parse::<i64>().ok() else {
                    return user_err("Нужно число от 1 до 28, например /schedule 25.");
                };
                self.set_reading_day(property_id, Some(day)).await?;
                Ok(vec![Reply::text(
                    from.chat_id,
                    format!(
                        "Срок передачи показаний — {day}-е число. Бот напомнит арендатору накануне, в срок и (до трёх раз) после просрочки; вам сообщит, если срок сорван."
                    ),
                )])
            }
        }
    }

    /// Напоминания, которые пора отправить. Вызывается по таймеру; повторный вызов в тот же день ничего не даёт
    /// (журнал `reminder_log`), так что перезапуск бота безопасен.
    pub async fn due_reminders(&self, now: NaiveDateTime) -> Res<Vec<Reply>> {
        if now.hour() < REMIND_HOUR {
            return Ok(vec![]);
        }
        let props: Vec<(i64, i64)> =
            sqlx::query_as("SELECT id, reading_day FROM property WHERE reading_day IS NOT NULL")
                .fetch_all(&self.pool)
                .await?;
        let mut out = vec![];
        for (property_id, day) in props {
            let Some(tenant) = self.tenant_chat(property_id).await? else {
                continue;
            };
            let period = self.open_period(property_id, now).await?;
            // показания уже отправлены (идёт расчёт) — напоминать нечего
            if self.period_billed(property_id, &period).await?
                || self.received(property_id, &period).await?.len() >= REGISTERS.len()
            {
                continue;
            }
            let Some(due) = due_date(&period, day) else {
                continue;
            };
            let days_late = (now.date() - due).num_days();
            for kind in due_kinds(days_late) {
                let fresh = sqlx::query("INSERT OR IGNORE INTO reminder_log (property_id, period, kind, sent_at) VALUES (?, ?, ?, ?)")
                    .bind(property_id)
                    .bind(&period)
                    .bind(kind)
                    .bind(now_iso(now))
                    .execute(&self.pool)
                    .await?
                    .rows_affected()
                    == 1;
                if !fresh {
                    continue;
                }
                let date = format!("{:02}.{:02}", due.day(), due.month());
                if kind == "landlord_late" {
                    out.push(Reply::text(
                        self.landlord_chat(property_id).await?,
                        format!("⏰ Арендатор не передал показания за {period} (срок был {date}). Напоминания ему отправлены."),
                    ));
                    continue;
                }
                let text = match kind {
                    "before" => format!(
                        "⏰ Завтра ({date}) срок передачи показаний за {period}. Подготовьте фото счётчиков."
                    ),
                    "due" => format!("⏰ Сегодня ({date}) срок передачи показаний за {period}."),
                    n => format!(
                        "⏰ Показания за {period} просрочены (срок был {date}, {} дн.). Передайте их, пожалуйста.",
                        n.trim_start_matches("late")
                    ),
                };
                out.push(Reply::with_buttons(
                    tenant,
                    text,
                    vec![vec![("▶️ Ввести показания", "enter".into())]],
                ));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminders_follow_the_deadline() {
        assert!(due_kinds(-2).is_empty());
        assert_eq!(due_kinds(-1), ["before"]);
        assert_eq!(due_kinds(0), ["due"]);
        assert_eq!(due_kinds(2), ["late2"]);
        assert_eq!(due_kinds(3), ["late3", "landlord_late"]);
        assert!(due_kinds(6).is_empty());
    }

    #[test]
    fn due_date_is_the_chosen_day_of_the_billing_month() {
        assert_eq!(
            due_date("2026-10", 25),
            NaiveDate::from_ymd_opt(2026, 10, 25)
        );
        assert_eq!(
            due_date("2026-02", 28),
            NaiveDate::from_ymd_opt(2026, 2, 28)
        );
    }
}

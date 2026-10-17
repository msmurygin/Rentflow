//! Минимальный мониторинг: что проверять и когда звать администратора. Решение «есть ли проблема» — чистая функция,
//! поэтому оно проверяется тестами без Telegram и диска.

use serde::Serialize;

/// Что измерено на этот момент. `None` — показатель не настроен (например, offsite не используется).
#[derive(Debug, Clone, Default)]
pub struct Vitals {
    pub db_ok: bool,
    /// Сколько секунд назад Telegram последний раз ответил боту (`None` — ещё не опрашивали).
    pub telegram_age_secs: Option<i64>,
    pub disk_free_mb: Option<u64>,
    pub backup_age_hours: Option<i64>,
    pub offsite_age_hours: Option<i64>,
    pub offsite_expected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Alert {
    /// Стабильный ключ: по нему алерт не повторяется чаще заданного интервала.
    pub key: &'static str,
    pub text: String,
}

pub const TELEGRAM_STALE_SECS: i64 = 300;
pub const DISK_MIN_MB: u64 = 500;
pub const BACKUP_MAX_AGE_H: i64 = 36;

pub fn evaluate(v: &Vitals) -> Vec<Alert> {
    let mut out = vec![];
    let mut push = |key, text: String| out.push(Alert { key, text });
    if !v.db_ok {
        push("db", "База данных не отвечает.".into());
    }
    if v.telegram_age_secs.is_some_and(|a| a > TELEGRAM_STALE_SECS) {
        push(
            "telegram",
            "Telegram API не отвечает дольше 5 минут: бот может не получать сообщения.".into(),
        );
    }
    if v.disk_free_mb.is_some_and(|m| m < DISK_MIN_MB) {
        push(
            "disk",
            format!(
                "Мало места на диске: свободно {} МБ.",
                v.disk_free_mb.unwrap_or(0)
            ),
        );
    }
    match v.backup_age_hours {
        None => push("backup", "Резервных копий ещё нет.".into()),
        Some(h) if h > BACKUP_MAX_AGE_H => {
            push("backup", format!("Последняя резервная копия {h} ч назад."))
        }
        _ => {}
    }
    if v.offsite_expected && v.offsite_age_hours.is_none_or(|h| h > BACKUP_MAX_AGE_H) {
        push(
            "offsite",
            "Копия не выгружена за пределы сервера более 36 часов.".into(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> Vitals {
        Vitals {
            db_ok: true,
            telegram_age_secs: Some(30),
            disk_free_mb: Some(5000),
            backup_age_hours: Some(5),
            offsite_age_hours: Some(5),
            offsite_expected: true,
        }
    }

    #[test]
    fn healthy_system_raises_nothing() {
        assert!(evaluate(&healthy()).is_empty());
    }

    #[test]
    fn each_failure_has_its_own_alert() {
        let keys = |v: Vitals| evaluate(&v).into_iter().map(|a| a.key).collect::<Vec<_>>();
        assert_eq!(
            keys(Vitals {
                db_ok: false,
                ..healthy()
            }),
            ["db"]
        );
        assert_eq!(
            keys(Vitals {
                telegram_age_secs: Some(900),
                ..healthy()
            }),
            ["telegram"]
        );
        assert_eq!(
            keys(Vitals {
                disk_free_mb: Some(100),
                ..healthy()
            }),
            ["disk"]
        );
        assert_eq!(
            keys(Vitals {
                backup_age_hours: Some(40),
                ..healthy()
            }),
            ["backup"]
        );
        assert_eq!(
            keys(Vitals {
                backup_age_hours: None,
                ..healthy()
            }),
            ["backup"]
        );
        assert_eq!(
            keys(Vitals {
                offsite_age_hours: None,
                ..healthy()
            }),
            ["offsite"]
        );
    }

    #[test]
    fn offsite_is_ignored_when_not_configured() {
        let v = Vitals {
            offsite_expected: false,
            offsite_age_hours: None,
            ..healthy()
        };
        assert!(evaluate(&v).is_empty());
    }
}

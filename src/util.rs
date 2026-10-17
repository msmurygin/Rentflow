//! Разбор ввода, форматирование чисел и работа с расчётными периодами.

use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;
use std::str::FromStr;

/// (код, название, единица) — четыре регистра объекта. T1/T2 — два регистра одного электросчётчика.
pub const REGISTERS: [(&str, &str, &str); 4] = [
    ("cold", "Холодная вода (ХВС)", "м³"),
    ("hot", "Горячая вода (ГВС)", "м³"),
    ("t1", "Электричество T1", "кВт·ч"),
    ("t2", "Электричество T2", "кВт·ч"),
];

pub fn label(code: &str) -> &'static str {
    match code {
        "sewer" => "Водоотведение",
        _ => REGISTERS.iter().find(|r| r.0 == code).map_or("?", |r| r.1),
    }
}

pub fn unit(code: &str) -> &'static str {
    REGISTERS.iter().find(|r| r.0 == code).map_or("м³", |r| r.2)
}

pub fn is_register(code: &str) -> bool {
    REGISTERS.iter().any(|r| r.0 == code)
}

/// Неотрицательное десятичное число; принимает запятую и пробелы-разделители. Деньги и показания — только Decimal.
pub fn parse_decimal(s: &str) -> Option<Decimal> {
    let t: String = s
        .trim()
        .replace(',', ".")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    // только цифры и одна точка: «1e5», «0x10», «+5» — не показания
    if t.is_empty()
        || !t.chars().all(|c| c.is_ascii_digit() || c == '.')
        || t.matches('.').count() > 1
    {
        return None;
    }
    let d = Decimal::from_str(&t).ok()?;
    (!d.is_sign_negative() && d < Decimal::from(100_000_000) && d.scale() <= 4).then_some(d)
}

/// `ГГГГ-ММ-ДД` или `ДД.ММ.ГГГГ`.
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    let t = s.trim();
    NaiveDate::parse_from_str(t, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(t, "%d.%m.%Y"))
        .ok()
}

pub fn period_of(date: NaiveDate) -> String {
    format!("{:04}-{:02}", date.year(), date.month())
}

/// Первый и последний день расчётного периода `ГГГГ-ММ`.
pub fn period_bounds(period: &str) -> Option<(NaiveDate, NaiveDate)> {
    let (y, m) = period.split_once('-')?;
    let first = NaiveDate::from_ymd_opt(y.parse().ok()?, m.parse().ok()?, 1)?;
    let next = if first.month() == 12 {
        NaiveDate::from_ymd_opt(first.year() + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1)?
    };
    Some((first, next.pred_opt()?))
}

/// 2063.6 → «2063,60».
pub fn money(d: Decimal) -> String {
    format!(
        "{:.2}",
        d.round_dp_with_strategy(2, rust_decimal::RoundingStrategy::MidpointAwayFromZero)
    )
    .replace('.', ",")
}

/// 3975.480 → «3975,48»; 5.0 → «5».
pub fn num(d: Decimal) -> String {
    d.normalize().to_string().replace('.', ",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_accept_comma_and_reject_garbage() {
        assert_eq!(
            parse_decimal(" 3975,48 "),
            Some(Decimal::from_str("3975.48").unwrap())
        );
        assert_eq!(parse_decimal("12 340"), Some(Decimal::from(12340)));
        assert_eq!(parse_decimal("-1"), None);
        assert_eq!(parse_decimal("abc"), None);
        assert_eq!(parse_decimal(""), None);
        assert_eq!(parse_decimal("1e5"), None);
        assert_eq!(parse_decimal("999999999"), None);
    }

    #[test]
    fn dates_in_both_formats() {
        assert_eq!(
            parse_date("2026-07-25"),
            NaiveDate::from_ymd_opt(2026, 7, 25)
        );
        assert_eq!(
            parse_date("25.07.2026"),
            NaiveDate::from_ymd_opt(2026, 7, 25)
        );
        assert_eq!(parse_date("2026-13-01"), None);
    }

    #[test]
    fn period_bounds_handle_year_end_and_leap_years() {
        assert_eq!(
            period_bounds("2026-12"),
            Some((
                NaiveDate::from_ymd_opt(2026, 12, 1).unwrap(),
                NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()
            ))
        );
        assert_eq!(
            period_bounds("2028-02").unwrap().1,
            NaiveDate::from_ymd_opt(2028, 2, 29).unwrap()
        );
        assert_eq!(period_bounds("nonsense"), None);
    }

    #[test]
    fn formatting_uses_comma_and_two_digits() {
        assert_eq!(money(Decimal::from_str("2063.6").unwrap()), "2063,60");
        assert_eq!(num(Decimal::from_str("3975.480").unwrap()), "3975,48");
        assert_eq!(num(Decimal::from_str("5.0").unwrap()), "5");
    }
}

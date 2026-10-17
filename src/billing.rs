//! Детерминированный расчёт коммунальных начислений (спека §6).
//!
//! Деньги — только `Decimal`, итог округляется до копеек. LLM/OCR сюда не попадают:
//! ядро получает уже подтверждённые человеком показания.

use rust_decimal::{Decimal, RoundingStrategy};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeterKind {
    ColdWater,
    HotWater,
    Electricity,
}

/// Вход расчёта для одного обязательного счётчика.
#[derive(Debug, Clone)]
pub struct MeterInput {
    pub meter_id: String,
    pub kind: MeterKind,
    pub previous: Option<Decimal>,
    pub current: Option<Decimal>,
    /// Тариф за единицу (RUB). `None` — тариф не задан.
    pub tariff: Option<Decimal>,
    /// Тариф менялся внутри периода — расчёт только вручную (спека §6).
    pub tariff_changed_in_period: bool,
    /// Явная корректировка замены счётчика разрешает «отрицательный» расход.
    pub replacement_adjustment: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillLine {
    pub meter_id: String,
    pub previous: Decimal,
    pub current: Decimal,
    pub usage: Decimal,
    pub tariff: Decimal,
    pub amount: Decimal,
}

/// Неизменяемый снимок расчёта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillSnapshot {
    pub lines: Vec<BillLine>,
    pub total: Decimal,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BillingError {
    #[error("нет обязательных счётчиков для расчёта")]
    NoMeters,
    #[error("счётчик {0}: нет предыдущего показания")]
    MissingPrevious(String),
    #[error("счётчик {0}: нет текущего подтверждённого показания")]
    MissingCurrent(String),
    #[error("счётчик {0}: не задан тариф")]
    MissingTariff(String),
    #[error("счётчик {0}: показание или тариф отрицательные")]
    NegativeValue(String),
    #[error("счётчик {0}: расход отрицателен — нужна ручная корректировка (замена/обнуление)")]
    NegativeUsage(String),
    #[error("счётчик {0}: тариф менялся внутри периода — нужна ручная разбивка")]
    TariffChangedInPeriod(String),
}

fn round_money(v: Decimal) -> Decimal {
    v.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

/// Считает счёт. Любая проблема хотя бы по одному счётчику — ошибка, частичный счёт не выдаётся.
pub fn calculate(inputs: &[MeterInput]) -> Result<BillSnapshot, BillingError> {
    if inputs.is_empty() {
        return Err(BillingError::NoMeters);
    }
    let mut lines = Vec::with_capacity(inputs.len());
    for m in inputs {
        let previous = m
            .previous
            .ok_or_else(|| BillingError::MissingPrevious(m.meter_id.clone()))?;
        let current = m
            .current
            .ok_or_else(|| BillingError::MissingCurrent(m.meter_id.clone()))?;
        let tariff = m
            .tariff
            .ok_or_else(|| BillingError::MissingTariff(m.meter_id.clone()))?;
        if m.tariff_changed_in_period {
            return Err(BillingError::TariffChangedInPeriod(m.meter_id.clone()));
        }
        if previous.is_sign_negative() || current.is_sign_negative() || tariff.is_sign_negative() {
            return Err(BillingError::NegativeValue(m.meter_id.clone()));
        }
        let usage = current - previous;
        if usage.is_sign_negative() && !m.replacement_adjustment {
            return Err(BillingError::NegativeUsage(m.meter_id.clone()));
        }
        lines.push(BillLine {
            meter_id: m.meter_id.clone(),
            previous,
            current,
            usage,
            tariff,
            amount: round_money(usage * tariff),
        });
    }
    let total = lines.iter().map(|l| l.amount).sum();
    Ok(BillSnapshot { lines, total })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn input(prev: Option<Decimal>, curr: Option<Decimal>, tariff: Option<Decimal>) -> MeterInput {
        MeterInput {
            meter_id: "cold".into(),
            kind: MeterKind::ColdWater,
            previous: prev,
            current: curr,
            tariff,
            tariff_changed_in_period: false,
            replacement_adjustment: false,
        }
    }

    #[test]
    fn usage_5_m3_at_65_rub_is_325() {
        let b = calculate(&[input(Some(dec!(120)), Some(dec!(125)), Some(dec!(65)))]).unwrap();
        assert_eq!(b.lines[0].usage, dec!(5));
        assert_eq!(b.total, dec!(325));
    }

    #[test]
    fn rounds_to_kopecks_half_away_from_zero() {
        // 0.005 * 1 = 0.005 -> 0.01
        let b = calculate(&[input(Some(dec!(0)), Some(dec!(0.005)), Some(dec!(1)))]).unwrap();
        assert_eq!(b.total, dec!(0.01));
        // 1.341 * 65 = 87.165 -> 87.17
        let b = calculate(&[input(Some(dec!(124)), Some(dec!(125.341)), Some(dec!(65)))]).unwrap();
        assert_eq!(b.total, dec!(87.17));
    }

    #[test]
    fn zero_usage_is_allowed() {
        let b = calculate(&[input(Some(dec!(10)), Some(dec!(10)), Some(dec!(65)))]).unwrap();
        assert_eq!(b.total, dec!(0));
    }

    #[test]
    fn negative_usage_is_an_exception() {
        let e = calculate(&[input(Some(dec!(10)), Some(dec!(9)), Some(dec!(65)))]).unwrap_err();
        assert_eq!(e, BillingError::NegativeUsage("cold".into()));
    }

    #[test]
    fn negative_usage_allowed_only_with_explicit_replacement() {
        let mut m = input(Some(dec!(10)), Some(dec!(9)), Some(dec!(65)));
        m.replacement_adjustment = true;
        assert!(calculate(&[m]).is_ok());
    }

    #[test]
    fn missing_tariff_previous_current_are_errors() {
        assert_eq!(
            calculate(&[input(Some(dec!(1)), Some(dec!(2)), None)]).unwrap_err(),
            BillingError::MissingTariff("cold".into())
        );
        assert_eq!(
            calculate(&[input(None, Some(dec!(2)), Some(dec!(1)))]).unwrap_err(),
            BillingError::MissingPrevious("cold".into())
        );
        assert_eq!(
            calculate(&[input(Some(dec!(1)), None, Some(dec!(1)))]).unwrap_err(),
            BillingError::MissingCurrent("cold".into())
        );
    }

    #[test]
    fn incomplete_set_yields_no_partial_bill() {
        let ok = input(Some(dec!(1)), Some(dec!(2)), Some(dec!(1)));
        let mut bad = input(Some(dec!(1)), None, Some(dec!(1)));
        bad.meter_id = "hot".into();
        assert!(calculate(&[ok, bad]).is_err());
    }

    #[test]
    fn tariff_change_in_period_requires_manual_split() {
        let mut m = input(Some(dec!(1)), Some(dec!(2)), Some(dec!(1)));
        m.tariff_changed_in_period = true;
        assert_eq!(
            calculate(&[m]).unwrap_err(),
            BillingError::TariffChangedInPeriod("cold".into())
        );
    }

    #[test]
    fn total_is_sum_of_rounded_lines() {
        let a = input(Some(dec!(0)), Some(dec!(0.005)), Some(dec!(1))); // 0.01
        let mut b = a.clone();
        b.meter_id = "hot".into(); // 0.01
        assert_eq!(calculate(&[a, b]).unwrap().total, dec!(0.02));
    }

    #[test]
    fn empty_input_is_an_error() {
        assert_eq!(calculate(&[]).unwrap_err(), BillingError::NoMeters);
    }
}

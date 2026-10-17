//! Детерминированный расчёт коммунальных начислений (спека §6, §6.1, §16).
//!
//! Деньги — только `Decimal`, итог округляется до копеек. LLM/OCR сюда не попадают:
//! ядро получает уже подтверждённые человеком показания.

use rust_decimal::{Decimal, RoundingStrategy};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterKind {
    ColdWater,
    HotWater,
    Electricity,
    /// Производная строка водоотведения (ХВС + ГВС), не физический регистр.
    Sewer,
}

/// Как накопительное показание превращается в расчётное значение.
/// Это настройка регистра/договора, а не глобальное правило (golden dataset №1, решение 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadingPrecision {
    /// Показание берётся как есть.
    #[default]
    Exact,
    /// Дробная часть каждого накопительного показания отбрасывается **до** вычитания.
    FloorBeforeDelta,
}

impl ReadingPrecision {
    fn apply(self, v: Decimal) -> Decimal {
        match self {
            Self::Exact => v,
            Self::FloorBeforeDelta => v.floor(),
        }
    }
}

/// Вход расчёта для одного обязательного регистра.
#[derive(Debug, Clone)]
pub struct MeterInput {
    pub meter_id: String,
    pub kind: MeterKind,
    pub precision: ReadingPrecision,
    pub previous: Option<Decimal>,
    pub current: Option<Decimal>,
    /// Тариф за единицу (RUB). `None` — тариф не задан.
    pub tariff: Option<Decimal>,
    /// Тариф менялся внутри периода — расчёт только вручную (спека §6).
    pub tariff_changed_in_period: bool,
    /// Явная корректировка замены счётчика разрешает «отрицательный» расход.
    pub replacement_adjustment: bool,
}

/// Правило водоотведения: объём = расход ХВС + расход ГВС, свой тариф, отдельная строка.
/// Передаётся в расчёт только если услуга явно включена для арендатора.
#[derive(Debug, Clone, Copy)]
pub struct SewerRule {
    pub tariff: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillLine {
    pub meter_id: String,
    pub kind: MeterKind,
    /// Расчётные значения (после политики точности).
    pub previous: Decimal,
    pub current: Decimal,
    /// Исходные значения как подтверждены — хранятся рядом с расчётными.
    pub previous_raw: Decimal,
    pub current_raw: Decimal,
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

pub fn round_money(v: Decimal) -> Decimal {
    v.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

/// Считает счёт без производных услуг.
pub fn calculate(inputs: &[MeterInput]) -> Result<BillSnapshot, BillingError> {
    calculate_with(inputs, None)
}

/// Считает счёт; `sewer` добавляет строку водоотведения. Любая проблема хотя бы по одному
/// регистру — ошибка, частичный счёт не выдаётся.
pub fn calculate_with(
    inputs: &[MeterInput],
    sewer: Option<SewerRule>,
) -> Result<BillSnapshot, BillingError> {
    if inputs.is_empty() {
        return Err(BillingError::NoMeters);
    }
    let mut lines = Vec::with_capacity(inputs.len() + 1);
    for m in inputs {
        // Если нет обоих показаний, действенно сообщить про текущее: его ждём от арендатора.
        let current_raw = m
            .current
            .ok_or_else(|| BillingError::MissingCurrent(m.meter_id.clone()))?;
        let previous_raw = m
            .previous
            .ok_or_else(|| BillingError::MissingPrevious(m.meter_id.clone()))?;
        let tariff = m
            .tariff
            .ok_or_else(|| BillingError::MissingTariff(m.meter_id.clone()))?;
        if m.tariff_changed_in_period {
            return Err(BillingError::TariffChangedInPeriod(m.meter_id.clone()));
        }
        if previous_raw.is_sign_negative()
            || current_raw.is_sign_negative()
            || tariff.is_sign_negative()
        {
            return Err(BillingError::NegativeValue(m.meter_id.clone()));
        }
        let previous = m.precision.apply(previous_raw);
        let current = m.precision.apply(current_raw);
        let usage = current - previous;
        if usage.is_sign_negative() && !m.replacement_adjustment {
            return Err(BillingError::NegativeUsage(m.meter_id.clone()));
        }
        lines.push(BillLine {
            meter_id: m.meter_id.clone(),
            kind: m.kind,
            previous,
            current,
            previous_raw,
            current_raw,
            usage,
            tariff,
            amount: round_money(usage * tariff),
        });
    }

    if let Some(rule) = sewer {
        let volume: Decimal = lines
            .iter()
            .filter(|l| matches!(l.kind, MeterKind::ColdWater | MeterKind::HotWater))
            .map(|l| l.usage)
            .sum();
        if volume.is_sign_negative() {
            return Err(BillingError::NegativeUsage("sewer".into()));
        }
        lines.push(BillLine {
            meter_id: "sewer".into(),
            kind: MeterKind::Sewer,
            previous: Decimal::ZERO,
            current: volume,
            previous_raw: Decimal::ZERO,
            current_raw: volume,
            usage: volume,
            tariff: rule.tariff,
            amount: round_money(volume * rule.tariff),
        });
    }

    let total = lines.iter().map(|l| l.amount).sum();
    Ok(BillSnapshot { lines, total })
}

/// Расхождение между суммой, записанной вручную, и пересчитанной суммой строк.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TotalDiscrepancy {
    pub written: Decimal,
    pub calculated: Decimal,
    /// `written - calculated`.
    pub difference: Decimal,
}

/// Сверяет внешнюю (рукописную/квитанционную) сумму с расчётом; `None`, если совпали до копейки.
/// Расхождение никогда не принимается молча.
pub fn find_total_discrepancy(bill: &BillSnapshot, written: Decimal) -> Option<TotalDiscrepancy> {
    (written != bill.total).then(|| TotalDiscrepancy {
        written,
        calculated: bill.total,
        difference: written - bill.total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn input(prev: Option<Decimal>, curr: Option<Decimal>, tariff: Option<Decimal>) -> MeterInput {
        MeterInput {
            meter_id: "cold".into(),
            kind: MeterKind::ColdWater,
            precision: ReadingPrecision::Exact,
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

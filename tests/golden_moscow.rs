//! RF-GOLD-001 — Golden Dataset №1 (Moscow apartment, июль–сентябрь 2026).
//! Строго детерминированные проверки биллингового ядра по `billing_golden.json`; OCR здесь не участвует.

use rentflow::billing::{
    BillSnapshot, BillingError, MeterInput, MeterKind, ReadingPrecision, SewerRule, calculate,
    calculate_with, find_total_discrepancy,
};
use rust_decimal::Decimal;
use serde_json::Value;
use std::str::FromStr;

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn golden() -> Value {
    let raw = include_str!("fixtures/golden_moscow_001/billing_golden.json");
    serde_json::from_str(raw).unwrap()
}

const REGISTERS: [(&str, MeterKind); 4] = [
    ("cold", MeterKind::ColdWater),
    ("hot", MeterKind::HotWater),
    ("t1", MeterKind::Electricity),
    ("t2", MeterKind::Electricity),
];

fn period<'a>(g: &'a Value, label: &str) -> &'a Value {
    g["billing_periods"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["period"] == label)
        .unwrap_or_else(|| panic!("period {label}"))
}

fn tariff(p: &Value, reg: &str) -> Decimal {
    dec(p["tariffs_rub_per_unit"][reg].as_str().unwrap())
}

fn reading(p: &Value, reg: &str, which: &str) -> Decimal {
    Decimal::from(p["readings_integer"][reg][which].as_i64().unwrap())
}

fn inputs(p: &Value) -> Vec<MeterInput> {
    REGISTERS
        .iter()
        .map(|(reg, kind)| MeterInput {
            meter_id: (*reg).into(),
            kind: *kind,
            precision: ReadingPrecision::FloorBeforeDelta,
            previous: Some(reading(p, reg, "previous")),
            current: Some(reading(p, reg, "current")),
            tariff: Some(tariff(p, reg)),
            tariff_changed_in_period: false,
            replacement_adjustment: false,
        })
        .collect()
}

fn bill(p: &Value) -> BillSnapshot {
    calculate_with(
        &inputs(p),
        Some(SewerRule {
            tariff: tariff(p, "sewer"),
        }),
    )
    .unwrap()
}

fn line<'a>(b: &'a BillSnapshot, id: &str) -> &'a rentflow::billing::BillLine {
    b.lines.iter().find(|l| l.meter_id == id).unwrap()
}

#[test]
fn three_months_match_gold_to_the_kopeck() {
    let g = golden();
    for label in ["2026-07", "2026-08", "2026-09"] {
        let p = period(&g, label);
        let b = bill(p);
        for id in ["cold", "hot", "sewer", "t1", "t2"] {
            let expected = dec(p["lines_rub"][id].as_str().unwrap());
            assert_eq!(line(&b, id).amount, expected, "{label} {id}");
        }
        let water: Decimal = ["cold", "hot", "sewer"]
            .iter()
            .map(|i| line(&b, i).amount)
            .sum();
        let electricity: Decimal = ["t1", "t2"].iter().map(|i| line(&b, i).amount).sum();
        assert_eq!(
            water,
            dec(p["water_rub"].as_str().unwrap()),
            "{label} water"
        );
        assert_eq!(
            electricity,
            dec(p["electricity_rub"].as_str().unwrap()),
            "{label} electricity"
        );
        assert_eq!(
            b.total,
            dec(p["expected_total_rub"].as_str().unwrap()),
            "{label} total"
        );
    }
}

#[test]
fn headline_totals_and_grand_total() {
    let g = golden();
    let totals: Vec<Decimal> = ["2026-07", "2026-08", "2026-09"]
        .iter()
        .map(|l| bill(period(&g, l)).total)
        .collect();
    assert_eq!(totals, vec![dec("2063.61"), dec("2469.51"), dec("2104.29")]);
    assert_eq!(totals.iter().copied().sum::<Decimal>(), dec("6637.41"));
}

#[test]
fn usage_matches_fixture_for_every_register() {
    let g = golden();
    for label in ["2026-07", "2026-08", "2026-09"] {
        let p = period(&g, label);
        let b = bill(p);
        for (reg, _) in REGISTERS {
            let expected = Decimal::from(p["readings_integer"][reg]["usage"].as_i64().unwrap());
            assert_eq!(line(&b, reg).usage, expected, "{label} {reg}");
        }
    }
}

#[test]
fn floor_before_delta_not_floor_of_delta() {
    // 753.015 → 755.589: расход 2 м³ (floor каждого показания), а не 2.574
    let make = |precision| MeterInput {
        meter_id: "cold".into(),
        kind: MeterKind::ColdWater,
        precision,
        previous: Some(dec("753.015")),
        current: Some(dec("755.589")),
        tariff: Some(dec("65.77")),
        tariff_changed_in_period: false,
        replacement_adjustment: false,
    };
    let floored = calculate(&[make(ReadingPrecision::FloorBeforeDelta)]).unwrap();
    assert_eq!(floored.lines[0].usage, dec("2"));
    assert_eq!(floored.total, dec("131.54"));
    // raw-значения хранятся рядом с расчётными
    assert_eq!(floored.lines[0].previous_raw, dec("753.015"));
    assert_eq!(floored.lines[0].current_raw, dec("755.589"));
    assert_eq!(floored.lines[0].previous, dec("753"));

    let exact = calculate(&[make(ReadingPrecision::Exact)]).unwrap();
    assert_eq!(exact.lines[0].usage, dec("2.574"));
}

#[test]
fn tariff_version_by_period() {
    let g = golden();
    assert_eq!(tariff(period(&g, "2026-07"), "cold"), dec("65.77"));
    assert_eq!(tariff(period(&g, "2026-08"), "cold"), dec("66.87"));
    assert_eq!(tariff(period(&g, "2026-07"), "hot"), dec("312.50"));
    assert_eq!(tariff(period(&g, "2026-08"), "hot"), dec("317.71"));
    assert_eq!(tariff(period(&g, "2026-07"), "sewer"), dec("51.62"));
    assert_eq!(tariff(period(&g, "2026-08"), "sewer"), dec("52.48"));
    // и именно эти тарифы попадают в строки счёта
    assert_eq!(
        line(&bill(period(&g, "2026-07")), "cold").tariff,
        dec("65.77")
    );
    assert_eq!(
        line(&bill(period(&g, "2026-08")), "cold").tariff,
        dec("66.87")
    );
}

#[test]
fn sewer_is_sum_of_water_volume() {
    let g = golden();
    for (label, volume) in [("2026-07", "3"), ("2026-08", "6"), ("2026-09", "3")] {
        let b = bill(period(&g, label));
        assert_eq!(line(&b, "sewer").usage, dec(volume), "{label}");
        assert_eq!(line(&b, "sewer").kind, MeterKind::Sewer);
    }
}

#[test]
fn register_isolation_t1_t2_never_merged() {
    let g = golden();
    let p = period(&g, "2026-07");
    let b = bill(p);
    assert_eq!(line(&b, "t1").usage, dec("144"));
    assert_eq!(line(&b, "t2").usage, dec("61"));
    // если слить (T1+T2) под одним тарифом, получится другая сумма
    let merged = dec("205") * dec("8.60");
    assert_ne!(merged, line(&b, "t1").amount + line(&b, "t2").amount);
}

#[test]
fn sewer_off_removes_the_line_and_total_follows() {
    let g = golden();
    let p = period(&g, "2026-07");
    let b = calculate(&inputs(p)).unwrap();
    assert!(b.lines.iter().all(|l| l.kind != MeterKind::Sewer));
    assert_eq!(b.total, dec("2063.61") - dec("154.86"));
}

#[test]
fn handwritten_september_total_is_flagged_not_accepted() {
    let g = golden();
    let b = bill(period(&g, "2026-09"));
    let known = &g["known_discrepancies"][0];
    let written = dec(known["written_rub"].as_str().unwrap());
    let d = find_total_discrepancy(&b, written).expect("discrepancy must be detected");
    assert_eq!(d.calculated, dec("2104.29"));
    assert_eq!(d.difference, dec(known["difference_rub"].as_str().unwrap()));
    // совпадающая сумма расхождением не считается
    assert!(find_total_discrepancy(&bill(period(&g, "2026-07")), dec("2063.61")).is_none());
}

#[test]
fn incomplete_register_blocks_the_whole_bill() {
    let g = golden();
    let mut inputs = inputs(period(&g, "2026-07"));
    inputs
        .iter_mut()
        .find(|m| m.meter_id == "t2")
        .unwrap()
        .current = None;
    let err = calculate_with(
        &inputs,
        Some(SewerRule {
            tariff: dec("51.62"),
        }),
    )
    .unwrap_err();
    assert_eq!(err, BillingError::MissingCurrent("t2".into()));
}

#[test]
fn bootstrap_baseline_alone_bills_nothing() {
    // июньский baseline есть, текущих показаний нет — счёт не получается
    let g = golden();
    let b = &g["baseline"]["readings_integer"];
    let inputs: Vec<MeterInput> = REGISTERS
        .iter()
        .map(|(reg, kind)| MeterInput {
            meter_id: (*reg).into(),
            kind: *kind,
            precision: ReadingPrecision::FloorBeforeDelta,
            previous: Some(Decimal::from(b[*reg].as_i64().unwrap())),
            current: None,
            tariff: Some(dec("1")),
            tariff_changed_in_period: false,
            replacement_adjustment: false,
        })
        .collect();
    assert!(matches!(
        calculate(&inputs),
        Err(BillingError::MissingCurrent(_))
    ));
}

#[test]
fn baseline_date_is_not_invented() {
    let g = golden();
    let date = g["baseline"]["reading_date"].as_str().unwrap();
    assert!(
        date.starts_with("UNKNOWN"),
        "fixture says the baseline date is unconfirmed: {date}"
    );
}

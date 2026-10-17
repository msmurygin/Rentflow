//! Сбор входов расчёта из БД и создание черновика счёта (спека US-03, §6, §6.1, §16).
//!
//! Правило дат (§6): расход считается между двумя **последовательными подтверждёнными**
//! показаниями регистра. Текущее — последнее подтверждённое, снятое до конца периода
//! (включительно); предыдущее — подтверждённое перед ним (это может быть baseline, у которого
//! дата не подтверждена: она остаётся пустой и не подменяется). Тариф выбирается по расчётному
//! периоду (месячный снимок); смена тарифа внутри периода — ручная разбивка.
//! Водоотведение — отдельная строка, только если включено для аренды.

use crate::billing::{self, BillingError, MeterInput, MeterKind, ReadingPrecision, SewerRule};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::PgPool;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftOutcome {
    /// Создана новая версия черновика.
    Created { bill_id: Uuid, version: i32 },
    /// Входы не изменились — вернули существующий черновик.
    Unchanged { bill_id: Uuid, version: i32 },
}

#[derive(Debug, Error)]
pub enum CalculationError {
    #[error("период не найден")]
    PeriodNotFound,
    #[error("нет прав рассчитывать этот период")]
    Forbidden,
    #[error("у объекта нет активных счётчиков")]
    NoMeters,
    #[error("за период уже есть утверждённый счёт — сначала аннулируйте его с причиной")]
    PeriodAlreadyBilled,
    #[error("расчёт невозможен: {0}")]
    Validation(#[from] BillingError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

type RegisterRow = (Uuid, String, String, String);
/// (reading id, value, read_on — `None`, если дата baseline не подтверждена)
type ReadingRow = (Uuid, Decimal, Option<String>);

fn kind_of(s: &str) -> MeterKind {
    match s {
        "cold_water" => MeterKind::ColdWater,
        "hot_water" => MeterKind::HotWater,
        _ => MeterKind::Electricity,
    }
}

fn precision_of(s: &str) -> ReadingPrecision {
    match s {
        "floor_before_delta" => ReadingPrecision::FloorBeforeDelta,
        _ => ReadingPrecision::Exact,
    }
}

/// Тариф(ы), действующие в расчётном периоде: (цена, больше ли одного за период).
fn effective_tariff(rows: &[(Decimal, String)]) -> (Option<Decimal>, bool) {
    (rows.first().map(|r| r.0), rows.len() > 1)
}

/// Считает черновик счёта за период. Повтор с теми же входами возвращает тот же черновик.
pub async fn calculate_draft(
    pool: &PgPool,
    period_id: Uuid,
    actor_id: Uuid,
) -> Result<DraftOutcome, CalculationError> {
    let mut tx = pool.begin().await?;

    // Порядок блокировок везде: период → счёт.
    let period: Option<(Uuid, String, String, String, bool)> = sqlx::query_as(
        "SELECT p.id, bp.starts_on::text, bp.ends_on::text, p.currency::text, t.sewer_billed
           FROM billing_period bp
           JOIN tenancy t ON t.id = bp.tenancy_id
           JOIN property p ON p.id = t.property_id
          WHERE bp.id = $1
          FOR UPDATE OF bp",
    )
    .bind(period_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((property_id, starts_on, ends_on, currency, sewer_billed)) = period else {
        return Err(CalculationError::PeriodNotFound);
    };

    let allowed: Option<(bool,)> = sqlx::query_as(
        "SELECT true FROM app_user u JOIN property p ON p.account_id = u.account_id
          WHERE u.id = $1 AND p.id = $2 AND u.role IN ('owner', 'manager')",
    )
    .bind(actor_id)
    .bind(property_id)
    .fetch_optional(&mut *tx)
    .await?;
    if allowed.is_none() {
        return Err(CalculationError::Forbidden);
    }

    let final_exists: Option<(bool,)> = sqlx::query_as(
        "SELECT true FROM bill WHERE billing_period_id = $1 AND status IN ('approved', 'paid')",
    )
    .bind(period_id)
    .fetch_optional(&mut *tx)
    .await?;
    if final_exists.is_some() {
        return Err(CalculationError::PeriodAlreadyBilled);
    }

    let registers: Vec<RegisterRow> = sqlx::query_as(
        "SELECT r.id, m.kind, r.display_label, r.precision
           FROM meter_register r JOIN meter m ON m.id = r.meter_id
          WHERE m.property_id = $1 AND m.active AND r.active
          ORDER BY m.label, r.register_code",
    )
    .bind(property_id)
    .fetch_all(&mut *tx)
    .await?;
    if registers.is_empty() {
        return Err(CalculationError::NoMeters);
    }

    let mut inputs = Vec::with_capacity(registers.len());
    let mut sources: Vec<(Uuid, Option<ReadingRow>, Option<ReadingRow>)> = Vec::new();
    for (register_id, kind, label, precision) in &registers {
        // curr: последнее подтверждённое на конец периода (включительно); prev: перед ним.
        let curr: Option<ReadingRow> = sqlx::query_as(
            "SELECT id, value, read_at::date::text FROM meter_reading
              WHERE meter_register_id = $1 AND status = 'confirmed'
                AND read_at < ($2::date + 1)
              ORDER BY read_at DESC, revision DESC LIMIT 1",
        )
        .bind(register_id)
        .bind(&ends_on)
        .fetch_optional(&mut *tx)
        .await?;
        let prev: Option<ReadingRow> = match &curr {
            Some((curr_id, _, _)) => {
                sqlx::query_as(
                    "SELECT id, value, read_at::date::text FROM meter_reading
                      WHERE meter_register_id = $1 AND status = 'confirmed' AND id <> $2
                        AND (read_at IS NULL
                             OR read_at < (SELECT read_at FROM meter_reading WHERE id = $2))
                      ORDER BY read_at DESC NULLS LAST, revision DESC LIMIT 1",
                )
                .bind(register_id)
                .bind(curr_id)
                .fetch_optional(&mut *tx)
                .await?
            }
            None => None,
        };

        // Тариф — по расчётному периоду; больше одного в периоде — ручная разбивка.
        let rows: Vec<(Decimal, String)> = sqlx::query_as(
            "SELECT unit_price, valid_from::text FROM tariff
              WHERE meter_register_id = $1 AND valid_from <= $3::date
                AND (valid_to IS NULL OR valid_to >= $2::date)
              ORDER BY valid_from DESC",
        )
        .bind(register_id)
        .bind(&starts_on)
        .bind(&ends_on)
        .fetch_all(&mut *tx)
        .await?;
        let (tariff, changed) = effective_tariff(&rows);

        inputs.push(MeterInput {
            meter_id: label.clone(),
            kind: kind_of(kind),
            precision: precision_of(precision),
            previous: prev.as_ref().map(|p| p.1),
            current: curr.as_ref().map(|c| c.1),
            tariff,
            tariff_changed_in_period: changed,
            replacement_adjustment: false,
        });
        sources.push((*register_id, prev, curr));
    }

    let sewer = if sewer_billed {
        let rows: Vec<(Decimal, String)> = sqlx::query_as(
            "SELECT unit_price, valid_from::text FROM tariff
              WHERE service = 'sewer' AND property_id = $1 AND valid_from <= $3::date
                AND (valid_to IS NULL OR valid_to >= $2::date)
              ORDER BY valid_from DESC",
        )
        .bind(property_id)
        .bind(&starts_on)
        .bind(&ends_on)
        .fetch_all(&mut *tx)
        .await?;
        match effective_tariff(&rows) {
            (_, true) => return Err(BillingError::TariffChangedInPeriod("sewer".into()).into()),
            (Some(tariff), false) => Some(SewerRule { tariff }),
            (None, false) => return Err(BillingError::MissingTariff("sewer".into()).into()),
        }
    } else {
        None
    };

    let bill = billing::calculate_with(&inputs, sewer)?;

    let snapshot = json!({
        "rule": "consecutive_confirmed_readings",
        "currency": currency,
        "total": bill.total.to_string(),
        "lines": bill.lines.iter().enumerate().map(|(i, l)| {
            let src = sources.get(i);
            json!({
                "kind": if l.kind == MeterKind::Sewer { "sewer" } else { "register" },
                "meter_register_id": src.map(|s| s.0),
                "meter": l.meter_id,
                "precision": inputs.get(i).map(|m| format!("{:?}", m.precision)),
                "previous": l.previous.to_string(),
                "previous_raw": l.previous_raw.to_string(),
                "previous_reading_id": src.and_then(|s| s.1.as_ref()).map(|p| p.0),
                "previous_read_on": src.and_then(|s| s.1.as_ref()).and_then(|p| p.2.clone()),
                "current": l.current.to_string(),
                "current_raw": l.current_raw.to_string(),
                "current_reading_id": src.and_then(|s| s.2.as_ref()).map(|c| c.0),
                "current_read_on": src.and_then(|s| s.2.as_ref()).and_then(|c| c.2.clone()),
                "usage": l.usage.to_string(),
                "unit_price": l.tariff.to_string(),
                "amount": l.amount.to_string(),
            })
        }).collect::<Vec<_>>(),
    });

    // Идемпотентность: тот же снимок, что у последнего черновика — вернуть его.
    let last: Option<(Uuid, i32, String, serde_json::Value)> = sqlx::query_as(
        "SELECT id, version, status, snapshot FROM bill
          WHERE billing_period_id = $1 ORDER BY version DESC LIMIT 1",
    )
    .bind(period_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((id, version, status, old)) = &last
        && status == "draft"
        && old == &snapshot
    {
        return Ok(DraftOutcome::Unchanged {
            bill_id: *id,
            version: *version,
        });
    }
    // Прежний черновик заменяется новой версией (история остаётся).
    sqlx::query(
        "UPDATE bill SET status = 'voided', void_reason = 'superseded by recalculation'
          WHERE billing_period_id = $1 AND status = 'draft'",
    )
    .bind(period_id)
    .execute(&mut *tx)
    .await?;

    let version = last.map_or(1, |l| l.1 + 1);
    let bill_id: Uuid = sqlx::query_scalar(
        "INSERT INTO bill (billing_period_id, version, currency, total, snapshot)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(period_id)
    .bind(version)
    .bind(&currency)
    .bind(bill.total)
    .bind(&snapshot)
    .fetch_one(&mut *tx)
    .await?;

    for (i, l) in bill.lines.iter().enumerate() {
        let register_id = sources.get(i).map(|s| s.0);
        sqlx::query(
            "INSERT INTO bill_line
               (bill_id, meter_register_id, kind, previous, current, previous_raw, current_raw,
                usage, unit_price, amount)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(bill_id)
        .bind(register_id)
        .bind(if register_id.is_some() {
            "register"
        } else {
            "sewer"
        })
        .bind(l.previous)
        .bind(l.current)
        .bind(l.previous_raw)
        .bind(l.current_raw)
        .bind(l.usage)
        .bind(l.tariff)
        .bind(l.amount)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "INSERT INTO audit_event (actor_id, action, object_type, object_id, details)
         VALUES ($1, 'bill.calculate', 'bill', $2, jsonb_build_object('version', $3))",
    )
    .bind(actor_id)
    .bind(bill_id)
    .bind(version)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(DraftOutcome::Created { bill_id, version })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::approve_bill;
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    struct World {
        owner: Uuid,
        stranger: Uuid,
        prop: Uuid,
        ten: Uuid,
        period: Uuid,
        cold: Uuid,
        t1: Uuid,
        t2: Uuid,
    }

    async fn id(pool: &PgPool, sql: &'static str, binds: &[Uuid]) -> Uuid {
        let mut q = sqlx::query_scalar::<_, Uuid>(sql);
        for b in binds {
            q = q.bind(*b);
        }
        q.fetch_one(pool).await.unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    async fn register(
        pool: &PgPool,
        prop: Uuid,
        kind: &str,
        meter_label: &str,
        code: &str,
        label: &str,
        unit: &str,
        precision: &str,
    ) -> Uuid {
        let meter: Uuid = sqlx::query_scalar(
            "INSERT INTO meter (property_id, kind, label, unit) VALUES ($1, $2, $3, $4)
             ON CONFLICT (property_id, label) DO UPDATE SET label = EXCLUDED.label RETURNING id",
        )
        .bind(prop)
        .bind(kind)
        .bind(meter_label)
        .bind(unit)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query_scalar(
            "INSERT INTO meter_register (meter_id, register_code, display_label, unit, precision)
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(meter)
        .bind(code)
        .bind(label)
        .bind(unit)
        .bind(precision)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn reading(
        pool: &PgPool,
        register: Uuid,
        by: Uuid,
        value: &str,
        at: Option<&str>,
        status: &str,
    ) {
        sqlx::query(
            "INSERT INTO meter_reading (meter_register_id, value, read_at, source, status, confirmed_by)
             VALUES ($1, $2::numeric, $3::timestamptz,
                     CASE WHEN $3::text IS NULL THEN 'initial' ELSE 'manual' END, $4,
                     CASE WHEN $4 = 'confirmed' THEN $5 END)",
        )
        .bind(register).bind(value).bind(at).bind(status).bind(by)
        .execute(pool).await.unwrap();
    }

    async fn tariff(pool: &PgPool, register: Uuid, price: &str, from: &str, to: Option<&str>) {
        sqlx::query("INSERT INTO tariff (meter_register_id, unit_price, valid_from, valid_to) VALUES ($1, $2::numeric, $3::date, $4::date)")
            .bind(register).bind(price).bind(from).bind(to)
            .execute(pool).await.unwrap();
    }

    async fn sewer_tariff(pool: &PgPool, prop: Uuid, price: &str, from: &str, to: Option<&str>) {
        sqlx::query("INSERT INTO tariff (property_id, service, unit_price, valid_from, valid_to) VALUES ($1, 'sewer', $2::numeric, $3::date, $4::date)")
            .bind(prop).bind(price).bind(from).bind(to)
            .execute(pool).await.unwrap();
    }

    async fn base(pool: &PgPool) -> (Uuid, Uuid, Uuid, Uuid) {
        let acc = id(
            pool,
            "INSERT INTO account (name) VALUES ('A') RETURNING id",
            &[],
        )
        .await;
        let other = id(
            pool,
            "INSERT INTO account (name) VALUES ('B') RETURNING id",
            &[],
        )
        .await;
        let owner = id(pool, "INSERT INTO app_user (account_id, telegram_id, role) VALUES ($1, 1, 'owner') RETURNING id", &[acc]).await;
        let stranger = id(pool, "INSERT INTO app_user (account_id, telegram_id, role) VALUES ($1, 2, 'owner') RETURNING id", &[other]).await;
        let tenant = id(pool, "INSERT INTO app_user (account_id, telegram_id, role) VALUES ($1, 3, 'tenant') RETURNING id", &[acc]).await;
        let prop = id(
            pool,
            "INSERT INTO property (account_id, name) VALUES ($1, 'Flat') RETURNING id",
            &[acc],
        )
        .await;
        let ten = id(pool, "INSERT INTO tenancy (property_id, tenant_id, starts_on) VALUES ($1, $2, '2026-01-01') RETURNING id", &[prop, tenant]).await;
        (owner, stranger, prop, ten)
    }

    /// Четыре регистра (ХВС, ГВС, T1, T2), показания 31 августа и 30 сентября, тарифы 65 / 350 / 6 / 3.
    async fn world(pool: &PgPool) -> World {
        let (owner, stranger, prop, ten) = base(pool).await;
        let period = id(pool, "INSERT INTO billing_period (tenancy_id, starts_on, ends_on) VALUES ($1, '2026-09-01', '2026-09-30') RETURNING id", &[ten]).await;
        let cold = register(
            pool,
            prop,
            "cold_water",
            "cold",
            "main",
            "cold",
            "m3",
            "exact",
        )
        .await;
        let hot = register(pool, prop, "hot_water", "hot", "main", "hot", "m3", "exact").await;
        let t1 = register(
            pool,
            prop,
            "electricity",
            "elec",
            "T1",
            "t1",
            "kWh",
            "exact",
        )
        .await;
        let t2 = register(
            pool,
            prop,
            "electricity",
            "elec",
            "T2",
            "t2",
            "kWh",
            "exact",
        )
        .await;
        for (r, prev, curr, price) in [
            (cold, "120", "125", "65"),
            (hot, "50", "52.5", "350"),
            (t1, "1000", "1100", "6"),
            (t2, "500", "520", "3"),
        ] {
            reading(
                pool,
                r,
                owner,
                prev,
                Some("2026-08-31T10:00:00Z"),
                "confirmed",
            )
            .await;
            reading(
                pool,
                r,
                owner,
                curr,
                Some("2026-09-30T10:00:00Z"),
                "confirmed",
            )
            .await;
            tariff(pool, r, price, "2026-01-01", None).await;
        }
        World {
            owner,
            stranger,
            prop,
            ten,
            period,
            cold,
            t1,
            t2,
        }
    }

    async fn bills(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM bill")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn total_of(pool: &PgPool, bill: Uuid) -> Decimal {
        sqlx::query_scalar("SELECT total FROM bill WHERE id = $1")
            .bind(bill)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn draft_has_a_line_per_register_and_correct_total(pool: PgPool) {
        let w = world(&pool).await;
        let out = calculate_draft(&pool, w.period, w.owner).await.unwrap();
        let DraftOutcome::Created { bill_id, version } = out else {
            panic!("{out:?}")
        };
        assert_eq!(version, 1);
        // 5*65 + 2.5*350 + 100*6 + 20*3 = 325 + 875 + 600 + 60
        assert_eq!(total_of(&pool, bill_id).await, dec("1860"));
        let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM bill_line WHERE bill_id = $1")
            .bind(bill_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(lines, 4, "water x2 + T1 + T2, no sewer by default");
        let src: String = sqlx::query_scalar(
            "SELECT snapshot->'lines'->0->>'current_reading_id' FROM bill WHERE id = $1",
        )
        .bind(bill_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!src.is_empty(), "each line links to its source reading");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn t1_and_t2_are_billed_separately(pool: PgPool) {
        let w = world(&pool).await;
        let DraftOutcome::Created { bill_id, .. } =
            calculate_draft(&pool, w.period, w.owner).await.unwrap()
        else {
            panic!()
        };
        let rows: Vec<(String, Decimal)> = sqlx::query_as(
            "SELECT r.display_label, bl.amount FROM bill_line bl JOIN meter_register r ON r.id = bl.meter_register_id
              WHERE bl.bill_id = $1 AND r.display_label IN ('t1', 't2') ORDER BY 1",
        ).bind(bill_id).fetch_all(&pool).await.unwrap();
        assert_eq!(
            rows,
            vec![
                ("t1".to_string(), dec("600")),
                ("t2".to_string(), dec("60"))
            ]
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn missing_t2_blocks_the_bill(pool: PgPool) {
        let w = world(&pool).await;
        sqlx::query(
            "DELETE FROM meter_reading WHERE meter_register_id = $1 AND read_at > '2026-09-01'",
        )
        .bind(w.t2)
        .execute(&pool)
        .await
        .unwrap();
        let err = calculate_draft(&pool, w.period, w.owner).await.unwrap_err();
        assert!(
            matches!(err, CalculationError::Validation(BillingError::MissingPrevious(ref m)) if m == "t2"),
            "{err:?}"
        );
        assert_eq!(bills(&pool).await, 0);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn same_inputs_return_the_same_draft(pool: PgPool) {
        let w = world(&pool).await;
        let first = calculate_draft(&pool, w.period, w.owner).await.unwrap();
        let second = calculate_draft(&pool, w.period, w.owner).await.unwrap();
        let DraftOutcome::Created { bill_id, version } = first else {
            panic!()
        };
        assert_eq!(second, DraftOutcome::Unchanged { bill_id, version });
        assert_eq!(bills(&pool).await, 1);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn changed_reading_creates_new_version_and_voids_old_draft(pool: PgPool) {
        let w = world(&pool).await;
        calculate_draft(&pool, w.period, w.owner).await.unwrap();
        sqlx::query("UPDATE meter_reading SET revision = 2, value = 126 WHERE meter_register_id = $1 AND read_at = '2026-09-30T10:00:00Z'")
            .bind(w.cold).execute(&pool).await.unwrap();
        let out = calculate_draft(&pool, w.period, w.owner).await.unwrap();
        assert!(
            matches!(out, DraftOutcome::Created { version: 2, .. }),
            "{out:?}"
        );
        let voided: i64 =
            sqlx::query_scalar("SELECT count(*) FROM bill WHERE status = 'voided' AND version = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(voided, 1);
        let total: Decimal = sqlx::query_scalar("SELECT total FROM bill WHERE version = 2")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(total, dec("1925")); // 6*65 + 875 + 600 + 60
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn unconfirmed_readings_are_ignored(pool: PgPool) {
        let w = world(&pool).await;
        reading(
            &pool,
            w.cold,
            w.owner,
            "999",
            Some("2026-09-30T20:00:00Z"),
            "pending",
        )
        .await;
        let DraftOutcome::Created { bill_id, .. } =
            calculate_draft(&pool, w.period, w.owner).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(total_of(&pool, bill_id).await, dec("1860"));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn readings_after_period_end_are_not_used(pool: PgPool) {
        let w = world(&pool).await;
        reading(
            &pool,
            w.t1,
            w.owner,
            "5000",
            Some("2026-10-02T10:00:00Z"),
            "confirmed",
        )
        .await;
        let DraftOutcome::Created { bill_id, .. } =
            calculate_draft(&pool, w.period, w.owner).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(total_of(&pool, bill_id).await, dec("1860"));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn tariff_change_inside_the_period_needs_manual_split(pool: PgPool) {
        let w = world(&pool).await;
        tariff(&pool, w.t1, "7", "2026-09-15", None).await;
        let err = calculate_draft(&pool, w.period, w.owner).await.unwrap_err();
        assert!(
            matches!(err, CalculationError::Validation(BillingError::TariffChangedInPeriod(ref m)) if m == "t1"),
            "{err:?}"
        );
        assert_eq!(bills(&pool).await, 0);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn tariff_ending_before_the_period_does_not_block(pool: PgPool) {
        // тариф августа закончился 31.08, с 01.09 — новый: смена на границе периодов, не внутри
        let (owner, _, prop, ten) = base(&pool).await;
        let period = id(&pool, "INSERT INTO billing_period (tenancy_id, starts_on, ends_on) VALUES ($1, '2026-09-01', '2026-09-30') RETURNING id", &[ten]).await;
        let r = register(
            &pool,
            prop,
            "cold_water",
            "cold",
            "main",
            "cold",
            "m3",
            "exact",
        )
        .await;
        reading(
            &pool,
            r,
            owner,
            "10",
            Some("2026-08-25T10:00:00Z"),
            "confirmed",
        )
        .await;
        reading(
            &pool,
            r,
            owner,
            "12",
            Some("2026-09-25T10:00:00Z"),
            "confirmed",
        )
        .await;
        tariff(&pool, r, "60", "2026-01-01", Some("2026-08-31")).await;
        tariff(&pool, r, "70", "2026-09-01", None).await;
        let DraftOutcome::Created { bill_id, .. } =
            calculate_draft(&pool, period, owner).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(total_of(&pool, bill_id).await, dec("140"));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn negative_usage_is_rejected(pool: PgPool) {
        let w = world(&pool).await;
        sqlx::query("UPDATE meter_reading SET value = 90 WHERE meter_register_id = $1 AND read_at > '2026-09-01'").bind(w.cold).execute(&pool).await.unwrap();
        let err = calculate_draft(&pool, w.period, w.owner).await.unwrap_err();
        assert!(
            matches!(
                err,
                CalculationError::Validation(BillingError::NegativeUsage(_))
            ),
            "{err:?}"
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn stranger_is_forbidden(pool: PgPool) {
        let w = world(&pool).await;
        assert!(matches!(
            calculate_draft(&pool, w.period, w.stranger).await,
            Err(CalculationError::Forbidden)
        ));
        assert_eq!(bills(&pool).await, 0);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn approved_period_cannot_be_recalculated_and_snapshot_survives_tariff_edits(
        pool: PgPool,
    ) {
        let w = world(&pool).await;
        let DraftOutcome::Created { bill_id, .. } =
            calculate_draft(&pool, w.period, w.owner).await.unwrap()
        else {
            panic!()
        };
        approve_bill(&pool, bill_id, w.owner).await.unwrap();
        sqlx::query("UPDATE tariff SET unit_price = 1000 WHERE meter_register_id = $1")
            .bind(w.cold)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(total_of(&pool, bill_id).await, dec("1860"));
        assert!(matches!(
            calculate_draft(&pool, w.period, w.owner).await,
            Err(CalculationError::PeriodAlreadyBilled)
        ));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn sewer_is_off_unless_the_tenancy_opts_in(pool: PgPool) {
        let w = world(&pool).await;
        sewer_tariff(&pool, w.prop, "50", "2026-01-01", None).await;
        // тариф водоотведения заведён, но услуга арендатору не включена — строки нет
        let DraftOutcome::Created { bill_id, .. } =
            calculate_draft(&pool, w.period, w.owner).await.unwrap()
        else {
            panic!()
        };
        let sewer: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM bill_line WHERE bill_id = $1 AND kind = 'sewer'",
        )
        .bind(bill_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(sewer, 0);

        sqlx::query("UPDATE tenancy SET sewer_billed = true WHERE id = $1")
            .bind(w.ten)
            .execute(&pool)
            .await
            .unwrap();
        let DraftOutcome::Created { bill_id, .. } =
            calculate_draft(&pool, w.period, w.owner).await.unwrap()
        else {
            panic!()
        };
        // объём = 5 + 2.5 = 7.5; 7.5 * 50 = 375
        assert_eq!(total_of(&pool, bill_id).await, dec("1860") + dec("375"));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn enabled_sewer_without_a_tariff_blocks_the_bill(pool: PgPool) {
        let w = world(&pool).await;
        sqlx::query("UPDATE tenancy SET sewer_billed = true WHERE id = $1")
            .bind(w.ten)
            .execute(&pool)
            .await
            .unwrap();
        let err = calculate_draft(&pool, w.period, w.owner).await.unwrap_err();
        assert!(
            matches!(err, CalculationError::Validation(BillingError::MissingTariff(ref m)) if m == "sewer"),
            "{err:?}"
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn bootstrap_baseline_with_unknown_date_bills_nothing_and_date_is_not_invented(
        pool: PgPool,
    ) {
        let (owner, _, prop, ten) = base(&pool).await;
        let period = id(&pool, "INSERT INTO billing_period (tenancy_id, starts_on, ends_on) VALUES ($1, '2026-07-01', '2026-07-31') RETURNING id", &[ten]).await;
        let r = register(
            &pool,
            prop,
            "cold_water",
            "cold",
            "main",
            "cold",
            "m3",
            "floor_before_delta",
        )
        .await;
        reading(&pool, r, owner, "753", None, "confirmed").await; // baseline, дата не подтверждена
        tariff(&pool, r, "65.77", "2026-01-01", None).await;
        let err = calculate_draft(&pool, period, owner).await.unwrap_err();
        assert!(
            matches!(
                err,
                CalculationError::Validation(BillingError::MissingCurrent(_))
            ),
            "{err:?}"
        );
        assert_eq!(bills(&pool).await, 0);
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT read_at::text FROM meter_reading WHERE meter_register_id = $1",
        )
        .bind(r)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(stored.is_none(), "unknown baseline date stays NULL");
    }

    /// RF-GOLD-001 целиком через БД: baseline без даты, три периода, версионные тарифы,
    /// водоотведение, floor-before-delta, T1/T2 раздельно, утверждение всех трёх счетов.
    #[sqlx::test(migrations = "./migrations")]
    async fn golden_moscow_three_months_through_the_database(pool: PgPool) {
        let (owner, _, prop, ten) = base(&pool).await;
        sqlx::query("UPDATE tenancy SET sewer_billed = true WHERE id = $1")
            .bind(ten)
            .execute(&pool)
            .await
            .unwrap();
        let cold = register(
            &pool,
            prop,
            "cold_water",
            "cold",
            "main",
            "cold",
            "m3",
            "floor_before_delta",
        )
        .await;
        let hot = register(
            &pool,
            prop,
            "hot_water",
            "hot",
            "main",
            "hot",
            "m3",
            "floor_before_delta",
        )
        .await;
        let t1 = register(
            &pool,
            prop,
            "electricity",
            "elec",
            "T1",
            "t1",
            "kWh",
            "floor_before_delta",
        )
        .await;
        let t2 = register(
            &pool,
            prop,
            "electricity",
            "elec",
            "T2",
            "t2",
            "kWh",
            "floor_before_delta",
        )
        .await;

        // (регистр, baseline, июль, август, сентябрь) — даты съёмки 25-е; baseline без даты
        let data = [
            (cold, "753", "755", "759", "761"),
            (hot, "416", "417", "419", "420"),
            (t1, "3831", "3975.48", "4102", "4250"),
            (t2, "1118", "1179", "1222", "1282"),
        ];
        for (r, b, jul, aug, sep) in data {
            reading(&pool, r, owner, b, None, "confirmed").await;
            reading(
                &pool,
                r,
                owner,
                jul,
                Some("2026-07-25T12:00:00Z"),
                "confirmed",
            )
            .await;
            reading(
                &pool,
                r,
                owner,
                aug,
                Some("2026-08-25T12:00:00Z"),
                "confirmed",
            )
            .await;
            reading(
                &pool,
                r,
                owner,
                sep,
                Some("2026-09-25T12:00:00Z"),
                "confirmed",
            )
            .await;
        }
        // версионные тарифы: вода/водоотведение меняются в августе, электричество — нет
        tariff(&pool, cold, "65.77", "2026-01-01", Some("2026-07-31")).await;
        tariff(&pool, cold, "66.87", "2026-08-01", None).await;
        tariff(&pool, hot, "312.50", "2026-01-01", Some("2026-07-31")).await;
        tariff(&pool, hot, "317.71", "2026-08-01", None).await;
        sewer_tariff(&pool, prop, "51.62", "2026-01-01", Some("2026-07-31")).await;
        sewer_tariff(&pool, prop, "52.48", "2026-08-01", None).await;
        tariff(&pool, t1, "8.60", "2026-01-01", None).await;
        tariff(&pool, t2, "3.71", "2026-01-01", None).await;

        let mut totals = Vec::new();
        for (start, end, expected) in [
            ("2026-07-01", "2026-07-31", "2063.61"),
            ("2026-08-01", "2026-08-31", "2469.51"),
            ("2026-09-01", "2026-09-30", "2104.29"),
        ] {
            let period: Uuid = sqlx::query_scalar("INSERT INTO billing_period (tenancy_id, starts_on, ends_on) VALUES ($1, $2::date, $3::date) RETURNING id")
                .bind(ten).bind(start).bind(end).fetch_one(&pool).await.unwrap();
            let DraftOutcome::Created { bill_id, .. } =
                calculate_draft(&pool, period, owner).await.unwrap()
            else {
                panic!()
            };
            assert_eq!(total_of(&pool, bill_id).await, dec(expected), "{start}");
            let lines: i64 =
                sqlx::query_scalar("SELECT count(*) FROM bill_line WHERE bill_id = $1")
                    .bind(bill_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(lines, 5, "ХВС, ГВС, водоотведение, T1, T2");
            approve_bill(&pool, bill_id, owner).await.unwrap();
            totals.push(total_of(&pool, bill_id).await);
        }
        assert_eq!(totals.into_iter().sum::<Decimal>(), dec("6637.41"));

        // июльский T1: 3975.48 хранится как raw, в расчёте участвует 3975
        let raw: String = sqlx::query_scalar(
            "SELECT bl.current_raw::text FROM bill_line bl JOIN bill b ON b.id = bl.bill_id
               JOIN billing_period bp ON bp.id = b.billing_period_id
              WHERE bl.meter_register_id = $1 AND bp.starts_on = '2026-07-01'",
        )
        .bind(t1)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(raw, "3975.480");
        // в июле baseline без даты: источник предыдущего показания помечен «дата неизвестна»
        let prev_on: Option<String> = sqlx::query_scalar(
            "SELECT b.snapshot->'lines'->0->>'previous_read_on' FROM bill b
               JOIN billing_period bp ON bp.id = b.billing_period_id WHERE bp.starts_on = '2026-07-01'",
        ).fetch_one(&pool).await.unwrap();
        assert!(
            prev_on.is_none(),
            "baseline date must not be invented: {prev_on:?}"
        );
    }
}

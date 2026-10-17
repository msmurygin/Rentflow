//! Сбор входов расчёта из БД и создание черновика счёта (спека US-03, §6, §8).
//!
//! Правило дат (§6): расход считается между двумя **последовательными подтверждёнными**
//! показаниями счётчика. Текущее — последнее подтверждённое, снятое до конца периода
//! (включительно); предыдущее — подтверждённое показание перед ним. Месяц не считается
//! «полным» автоматически: в снимок попадают реальные даты снятия.

use crate::billing::{self, BillingError, MeterInput, MeterKind};
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

type MeterRow = (Uuid, String, String);

fn kind_of(s: &str) -> MeterKind {
    match s {
        "cold_water" => MeterKind::ColdWater,
        "hot_water" => MeterKind::HotWater,
        _ => MeterKind::Electricity,
    }
}

/// Считает черновик счёта за период. Повтор с теми же входами возвращает тот же черновик.
pub async fn calculate_draft(
    pool: &PgPool,
    period_id: Uuid,
    actor_id: Uuid,
) -> Result<DraftOutcome, CalculationError> {
    let mut tx = pool.begin().await?;

    let period: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT p.id, bp.ends_on::text, p.currency::text
           FROM billing_period bp
           JOIN tenancy t ON t.id = bp.tenancy_id
           JOIN property p ON p.id = t.property_id
          WHERE bp.id = $1
          FOR UPDATE OF bp",
    )
    .bind(period_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((property_id, ends_on, currency)) = period else {
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

    let meters: Vec<MeterRow> = sqlx::query_as(
        "SELECT id, kind, label FROM meter WHERE property_id = $1 AND active ORDER BY label",
    )
    .bind(property_id)
    .fetch_all(&mut *tx)
    .await?;
    if meters.is_empty() {
        return Err(CalculationError::NoMeters);
    }

    let mut inputs = Vec::with_capacity(meters.len());
    let mut sources = Vec::with_capacity(meters.len());
    for (meter_id, kind, label) in &meters {
        // curr: последнее подтверждённое на конец периода (включительно); prev: перед ним.
        let curr: Option<(Uuid, Decimal, String)> = sqlx::query_as(
            "SELECT id, value, read_at::date::text FROM meter_reading
              WHERE meter_id = $1 AND status = 'confirmed'
                AND read_at < ($2::date + 1)
              ORDER BY read_at DESC, revision DESC LIMIT 1",
        )
        .bind(meter_id)
        .bind(&ends_on)
        .fetch_optional(&mut *tx)
        .await?;
        let prev: Option<(Uuid, Decimal, String)> = match &curr {
            Some((curr_id, _, _)) => {
                sqlx::query_as(
                    "SELECT id, value, read_at::date::text FROM meter_reading
                      WHERE meter_id = $1 AND status = 'confirmed' AND id <> $2
                        AND read_at < (SELECT read_at FROM meter_reading WHERE id = $2)
                      ORDER BY read_at DESC, revision DESC LIMIT 1",
                )
                .bind(meter_id)
                .bind(curr_id)
                .fetch_optional(&mut *tx)
                .await?
            }
            None => None,
        };

        // Тариф на дату текущего показания; если за интервал действует больше одного — ручная разбивка.
        let (tariff, changed) = match (&prev, &curr) {
            (Some((_, _, from)), Some((_, _, to))) => {
                let rows: Vec<(Decimal, String)> = sqlx::query_as(
                    "SELECT unit_price, valid_from::text FROM tariff
                      WHERE meter_id = $1 AND valid_from <= $3::date
                        AND (valid_to IS NULL OR valid_to >= $2::date)
                      ORDER BY valid_from DESC",
                )
                .bind(meter_id)
                .bind(from)
                .bind(to)
                .fetch_all(&mut *tx)
                .await?;
                (rows.first().map(|r| r.0), rows.len() > 1)
            }
            _ => (None, false),
        };

        inputs.push(MeterInput {
            meter_id: label.clone(),
            kind: kind_of(kind),
            previous: prev.as_ref().map(|p| p.1),
            current: curr.as_ref().map(|c| c.1),
            tariff,
            tariff_changed_in_period: changed,
            replacement_adjustment: false,
        });
        sources.push((
            *meter_id,
            prev.map(|p| (p.0, p.2)),
            curr.map(|c| (c.0, c.2)),
        ));
    }

    let bill = billing::calculate(&inputs)?;

    let snapshot = json!({
        "rule": "consecutive_confirmed_readings",
        "currency": currency,
        "total": bill.total.to_string(),
        "lines": bill.lines.iter().zip(&sources).map(|(l, (meter_id, prev, curr))| json!({
            "meter_id": meter_id,
            "meter": l.meter_id,
            "previous": l.previous.to_string(),
            "previous_reading_id": prev.as_ref().map(|p| p.0),
            "previous_read_on": prev.as_ref().map(|p| p.1.clone()),
            "current": l.current.to_string(),
            "current_reading_id": curr.as_ref().map(|c| c.0),
            "current_read_on": curr.as_ref().map(|c| c.1.clone()),
            "usage": l.usage.to_string(),
            "unit_price": l.tariff.to_string(),
            "amount": l.amount.to_string(),
        })).collect::<Vec<_>>(),
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

    for (l, (meter_id, _, _)) in bill.lines.iter().zip(&sources) {
        sqlx::query(
            "INSERT INTO bill_line (bill_id, meter_id, previous, current, usage, unit_price, amount)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(bill_id)
        .bind(meter_id)
        .bind(l.previous)
        .bind(l.current)
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

    struct World {
        owner: Uuid,
        stranger: Uuid,
        period: Uuid,
        cold: Uuid,
        hot: Uuid,
        elec: Uuid,
    }

    async fn id(pool: &PgPool, sql: &'static str, binds: &[Uuid]) -> Uuid {
        let mut q = sqlx::query_scalar::<_, Uuid>(sql);
        for b in binds {
            q = q.bind(*b);
        }
        q.fetch_one(pool).await.unwrap()
    }

    async fn reading(pool: &PgPool, meter: Uuid, owner: Uuid, value: &str, at: &str, status: &str) {
        sqlx::query(
            "INSERT INTO meter_reading (meter_id, value, read_at, source, status, confirmed_by)
             VALUES ($1, $2::numeric, $3::timestamptz, 'manual', $4, CASE WHEN $4 = 'confirmed' THEN $5 END)",
        )
        .bind(meter)
        .bind(value)
        .bind(at)
        .bind(status)
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn tariff(pool: &PgPool, meter: Uuid, price: &str, from: &str) {
        sqlx::query("INSERT INTO tariff (meter_id, unit_price, valid_from) VALUES ($1, $2::numeric, $3::date)")
            .bind(meter)
            .bind(price)
            .bind(from)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Три счётчика, показания на 31 августа и 30 сентября, тарифы 65 / 350 / 6.
    async fn world(pool: &PgPool) -> World {
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
        let period = id(pool, "INSERT INTO billing_period (tenancy_id, starts_on, ends_on) VALUES ($1, '2026-09-01', '2026-09-30') RETURNING id", &[ten]).await;
        let cold = id(pool, "INSERT INTO meter (property_id, kind, label, unit) VALUES ($1, 'cold_water', 'cold', 'm3') RETURNING id", &[prop]).await;
        let hot = id(pool, "INSERT INTO meter (property_id, kind, label, unit) VALUES ($1, 'hot_water', 'hot', 'm3') RETURNING id", &[prop]).await;
        let elec = id(pool, "INSERT INTO meter (property_id, kind, label, unit) VALUES ($1, 'electricity', 'elec', 'kWh') RETURNING id", &[prop]).await;
        for (m, prev, curr, price) in [
            (cold, "120", "125", "65"),
            (hot, "50", "52.5", "350"),
            (elec, "1000", "1100", "6"),
        ] {
            reading(pool, m, owner, prev, "2026-08-31T10:00:00Z", "confirmed").await;
            reading(pool, m, owner, curr, "2026-09-30T10:00:00Z", "confirmed").await;
            tariff(pool, m, price, "2026-01-01").await;
        }
        World {
            owner,
            stranger,
            period,
            cold,
            hot,
            elec,
        }
    }

    async fn bills(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM bill")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn draft_has_three_lines_and_correct_total(pool: PgPool) {
        let w = world(&pool).await;
        let out = calculate_draft(&pool, w.period, w.owner).await.unwrap();
        let DraftOutcome::Created { bill_id, version } = out else {
            panic!("{out:?}")
        };
        assert_eq!(version, 1);
        // 5*65 + 2.5*350 + 100*6 = 325 + 875 + 600
        let total: Decimal = sqlx::query_scalar("SELECT total FROM bill WHERE id = $1")
            .bind(bill_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(total, Decimal::from(1800));
        let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM bill_line WHERE bill_id = $1")
            .bind(bill_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(lines, 3);
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
        // владелец исправил показание: новое подтверждённое значение того же дня позже по revision
        sqlx::query("UPDATE meter_reading SET revision = 2, value = 126 WHERE meter_id = $1 AND read_at = '2026-09-30T10:00:00Z'")
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
        assert_eq!(total, Decimal::from(1865)); // 6*65 + 875 + 600
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_single_confirmed_reading_blocks_the_whole_bill(pool: PgPool) {
        let w = world(&pool).await;
        sqlx::query("DELETE FROM meter_reading WHERE meter_id = $1 AND read_at > '2026-09-01'")
            .bind(w.hot)
            .execute(&pool)
            .await
            .unwrap();
        // остался единственный (августовский) — «текущее» без предыдущего
        let err = calculate_draft(&pool, w.period, w.owner).await.unwrap_err();
        assert!(
            matches!(err, CalculationError::Validation(BillingError::MissingPrevious(ref m)) if m == "hot"),
            "{err:?}"
        );
        assert_eq!(bills(&pool).await, 0);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn unconfirmed_readings_are_ignored(pool: PgPool) {
        let w = world(&pool).await;
        reading(
            &pool,
            w.cold,
            w.owner,
            "999",
            "2026-09-30T20:00:00Z",
            "pending",
        )
        .await;
        let out = calculate_draft(&pool, w.period, w.owner).await.unwrap();
        let DraftOutcome::Created { bill_id, .. } = out else {
            panic!()
        };
        let total: Decimal = sqlx::query_scalar("SELECT total FROM bill WHERE id = $1")
            .bind(bill_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(total, Decimal::from(1800));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn readings_after_period_end_are_not_used(pool: PgPool) {
        let w = world(&pool).await;
        reading(
            &pool,
            w.elec,
            w.owner,
            "5000",
            "2026-10-02T10:00:00Z",
            "confirmed",
        )
        .await;
        let out = calculate_draft(&pool, w.period, w.owner).await.unwrap();
        let DraftOutcome::Created { bill_id, .. } = out else {
            panic!()
        };
        let total: Decimal = sqlx::query_scalar("SELECT total FROM bill WHERE id = $1")
            .bind(bill_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(total, Decimal::from(1800));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn tariff_change_inside_interval_needs_manual_split(pool: PgPool) {
        let w = world(&pool).await;
        tariff(&pool, w.elec, "7", "2026-09-15").await;
        let err = calculate_draft(&pool, w.period, w.owner).await.unwrap_err();
        assert!(
            matches!(err, CalculationError::Validation(BillingError::TariffChangedInPeriod(ref m)) if m == "elec"),
            "{err:?}"
        );
        assert_eq!(bills(&pool).await, 0);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn negative_usage_is_rejected(pool: PgPool) {
        let w = world(&pool).await;
        sqlx::query(
            "UPDATE meter_reading SET value = 90 WHERE meter_id = $1 AND read_at > '2026-09-01'",
        )
        .bind(w.cold)
        .execute(&pool)
        .await
        .unwrap();
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

        // тариф задним числом не меняет утверждённый счёт (§10)
        sqlx::query("UPDATE tariff SET unit_price = 1000 WHERE meter_id = $1")
            .bind(w.cold)
            .execute(&pool)
            .await
            .unwrap();
        let total: Decimal = sqlx::query_scalar("SELECT total FROM bill WHERE id = $1")
            .bind(bill_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(total, Decimal::from(1800));

        assert!(matches!(
            calculate_draft(&pool, w.period, w.owner).await,
            Err(CalculationError::PeriodAlreadyBilled)
        ));
    }
}

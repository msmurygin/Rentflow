//! Жизненный цикл утверждённого счёта и ingress-идемпотентность (спека US-05, US-06, §6).
//!
//! * `mark_paid` — собственник фиксирует факт получения денег; бот не утверждает оплату раньше (§13.6).
//! * `void_bill` — единственный способ «исправить» выставленный счёт: аннулирование с причиной,
//!   после чего считается новая версия. Утверждённый счёт молча не редактируется.
//! * `register_telegram_update` — дедупликация webhook по `(bot_id, update_id)`.

use sqlx::PgPool;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayOutcome {
    Paid,
    AlreadyPaid,
}

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("счёт не найден")]
    NotFound,
    #[error("нет прав на этот счёт")]
    Forbidden,
    #[error("счёт в состоянии {0} — операция невозможна")]
    InvalidState(String),
    #[error("причина аннулирования обязательна")]
    ReasonRequired,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

struct Locked {
    status: String,
    property_id: Uuid,
    period_id: Uuid,
    total: rust_decimal::Decimal,
}

/// Блокирует счёт и проверяет, что `actor_id` — собственник/управляющий аккаунта объекта.
async fn lock_bill_for_actor(
    tx: &mut sqlx::PgConnection,
    bill_id: Uuid,
    actor_id: Uuid,
) -> Result<Locked, LifecycleError> {
    // Порядок блокировок везде один: период → счёт (как в calculate_draft), иначе возможна
    // взаимная блокировка с параллельным пересчётом.
    let period_locked: Option<(Uuid,)> = sqlx::query_as(
        "SELECT bp.id FROM billing_period bp
          WHERE bp.id = (SELECT billing_period_id FROM bill WHERE id = $1)
          FOR UPDATE",
    )
    .bind(bill_id)
    .fetch_optional(&mut *tx)
    .await?;
    if period_locked.is_none() {
        return Err(LifecycleError::NotFound);
    }

    let row: Option<(String, Uuid, Uuid, rust_decimal::Decimal)> = sqlx::query_as(
        "SELECT b.status, p.id, bp.id, b.total
           FROM bill b
           JOIN billing_period bp ON bp.id = b.billing_period_id
           JOIN tenancy t ON t.id = bp.tenancy_id
           JOIN property p ON p.id = t.property_id
          WHERE b.id = $1
          FOR UPDATE OF b",
    )
    .bind(bill_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((status, property_id, period_id, total)) = row else {
        return Err(LifecycleError::NotFound);
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
        return Err(LifecycleError::Forbidden);
    }
    Ok(Locked {
        status,
        property_id,
        period_id,
        total,
    })
}

/// Фиксирует полную оплату утверждённого счёта (R0: частичные платежи — R2).
pub async fn mark_paid(
    pool: &PgPool,
    bill_id: Uuid,
    actor_id: Uuid,
) -> Result<PayOutcome, LifecycleError> {
    let mut tx = pool.begin().await?;
    let b = lock_bill_for_actor(&mut tx, bill_id, actor_id).await?;
    match b.status.as_str() {
        "paid" => return Ok(PayOutcome::AlreadyPaid),
        "approved" => {}
        other => return Err(LifecycleError::InvalidState(other.to_string())),
    }
    sqlx::query("UPDATE bill SET status = 'paid' WHERE id = $1")
        .bind(bill_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO payment_record (bill_id, amount, paid_on, marked_by)
         VALUES ($1, $2, CURRENT_DATE, $3)",
    )
    .bind(bill_id)
    .bind(b.total)
    .bind(actor_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO audit_event (actor_id, action, object_type, object_id, details)
         VALUES ($1, 'bill.mark_paid', 'bill', $2, jsonb_build_object('amount', $3::text))",
    )
    .bind(actor_id)
    .bind(bill_id)
    .bind(b.total.to_string())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(PayOutcome::Paid)
}

/// Аннулирует черновик или утверждённый (ещё не оплаченный) счёт с обязательной причиной.
/// Период снова открыт для пересчёта; арендатору уходит уведомление об аннулировании.
pub async fn void_bill(
    pool: &PgPool,
    bill_id: Uuid,
    actor_id: Uuid,
    reason: &str,
) -> Result<(), LifecycleError> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(LifecycleError::ReasonRequired);
    }
    let mut tx = pool.begin().await?;
    let b = lock_bill_for_actor(&mut tx, bill_id, actor_id).await?;
    let was_final = match b.status.as_str() {
        "draft" => false,
        "approved" => true,
        // оплаченный счёт требует отдельной корректировки платежа — не R0
        other => return Err(LifecycleError::InvalidState(other.to_string())),
    };
    sqlx::query("UPDATE bill SET status = 'voided', void_reason = $2 WHERE id = $1")
        .bind(bill_id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
    if was_final {
        sqlx::query("UPDATE billing_period SET state = 'open' WHERE id = $1")
            .bind(b.period_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO outbox_event (dedup_key, kind, payload)
             VALUES ($1, 'bill.voided', jsonb_build_object('bill_id', $2::text, 'reason', $3::text))
             ON CONFLICT (dedup_key) DO NOTHING",
        )
        .bind(format!("bill.voided:{bill_id}"))
        .bind(bill_id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "INSERT INTO audit_event (actor_id, action, object_type, object_id, details)
         VALUES ($1, 'bill.void', 'bill', $2, jsonb_build_object('reason', $3::text, 'property_id', $4::text))",
    )
    .bind(actor_id)
    .bind(bill_id)
    .bind(reason)
    .bind(b.property_id.to_string())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// `true` — апдейт новый, обрабатываем; `false` — дубль доставки, пропускаем.
pub async fn register_telegram_update(
    pool: &PgPool,
    bot_id: i64,
    update_id: i64,
) -> Result<bool, sqlx::Error> {
    let r = sqlx::query(
        "INSERT INTO telegram_update (bot_id, update_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(bot_id)
    .bind(update_id)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::approve_bill;

    async fn id(pool: &PgPool, sql: &'static str, binds: &[Uuid]) -> Uuid {
        let mut q = sqlx::query_scalar::<_, Uuid>(sql);
        for b in binds {
            q = q.bind(*b);
        }
        q.fetch_one(pool).await.unwrap()
    }

    /// (owner, stranger, period, draft bill id)
    async fn seed(pool: &PgPool) -> (Uuid, Uuid, Uuid, Uuid) {
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
        let bill = id(pool, "INSERT INTO bill (billing_period_id, version, total, snapshot) VALUES ($1, 1, 325.00, '{}') RETURNING id", &[period]).await;
        (owner, stranger, period, bill)
    }

    async fn count(pool: &PgPool, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn mark_paid_records_payment_once(pool: PgPool) {
        let (owner, _, _, bill) = seed(&pool).await;
        approve_bill(&pool, bill, owner).await.unwrap();
        assert_eq!(
            mark_paid(&pool, bill, owner).await.unwrap(),
            PayOutcome::Paid
        );
        assert_eq!(
            mark_paid(&pool, bill, owner).await.unwrap(),
            PayOutcome::AlreadyPaid
        );
        assert_eq!(count(&pool, "SELECT count(*) FROM payment_record").await, 1);
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM audit_event WHERE action = 'bill.mark_paid'"
            )
            .await,
            1
        );
        let amount: rust_decimal::Decimal = sqlx::query_scalar("SELECT amount FROM payment_record")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(amount, rust_decimal::Decimal::from(325));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn draft_cannot_be_paid(pool: PgPool) {
        let (owner, _, _, bill) = seed(&pool).await;
        assert!(matches!(
            mark_paid(&pool, bill, owner).await,
            Err(LifecycleError::InvalidState(_))
        ));
        assert_eq!(count(&pool, "SELECT count(*) FROM payment_record").await, 0);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn concurrent_mark_paid_makes_one_payment(pool: PgPool) {
        let (owner, _, _, bill) = seed(&pool).await;
        approve_bill(&pool, bill, owner).await.unwrap();
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let p = pool.clone();
            tasks.push(tokio::spawn(async move {
                mark_paid(&p, bill, owner).await.unwrap()
            }));
        }
        let mut paid = 0;
        for t in tasks {
            if t.await.unwrap() == PayOutcome::Paid {
                paid += 1;
            }
        }
        assert_eq!(paid, 1);
        assert_eq!(count(&pool, "SELECT count(*) FROM payment_record").await, 1);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn stranger_cannot_pay_or_void(pool: PgPool) {
        let (owner, stranger, _, bill) = seed(&pool).await;
        approve_bill(&pool, bill, owner).await.unwrap();
        assert!(matches!(
            mark_paid(&pool, bill, stranger).await,
            Err(LifecycleError::Forbidden)
        ));
        assert!(matches!(
            void_bill(&pool, bill, stranger, "x").await,
            Err(LifecycleError::Forbidden)
        ));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn void_requires_a_reason(pool: PgPool) {
        let (owner, _, _, bill) = seed(&pool).await;
        assert!(matches!(
            void_bill(&pool, bill, owner, "   ").await,
            Err(LifecycleError::ReasonRequired)
        ));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn void_after_approval_reopens_period_and_notifies_once(pool: PgPool) {
        let (owner, _, period, bill) = seed(&pool).await;
        approve_bill(&pool, bill, owner).await.unwrap();
        void_bill(&pool, bill, owner, "ошибка в показании")
            .await
            .unwrap();

        let (status, reason): (String, String) =
            sqlx::query_as("SELECT status, void_reason FROM bill WHERE id = $1")
                .bind(bill)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (status.as_str(), reason.as_str()),
            ("voided", "ошибка в показании")
        );
        let state: String = sqlx::query_scalar("SELECT state FROM billing_period WHERE id = $1")
            .bind(period)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "open");
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM outbox_event WHERE kind = 'bill.voided'"
            )
            .await,
            1
        );

        // повтор не проходит и не создаёт второе уведомление
        assert!(matches!(
            void_bill(&pool, bill, owner, "ещё раз").await,
            Err(LifecycleError::InvalidState(_))
        ));
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM outbox_event WHERE kind = 'bill.voided'"
            )
            .await,
            1
        );
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn paid_bill_cannot_be_voided(pool: PgPool) {
        let (owner, _, _, bill) = seed(&pool).await;
        approve_bill(&pool, bill, owner).await.unwrap();
        mark_paid(&pool, bill, owner).await.unwrap();
        assert!(matches!(
            void_bill(&pool, bill, owner, "поздно").await,
            Err(LifecycleError::InvalidState(_))
        ));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn draft_void_sends_no_notification(pool: PgPool) {
        let (owner, _, period, bill) = seed(&pool).await;
        void_bill(&pool, bill, owner, "черновик не нужен")
            .await
            .unwrap();
        assert_eq!(count(&pool, "SELECT count(*) FROM outbox_event").await, 0);
        let state: String = sqlx::query_scalar("SELECT state FROM billing_period WHERE id = $1")
            .bind(period)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "open");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn duplicate_telegram_update_is_rejected(pool: PgPool) {
        assert!(register_telegram_update(&pool, 7, 100).await.unwrap());
        assert!(!register_telegram_update(&pool, 7, 100).await.unwrap());
        assert!(register_telegram_update(&pool, 7, 101).await.unwrap());
        assert!(
            register_telegram_update(&pool, 8, 100).await.unwrap(),
            "other bot, same update_id"
        );
    }
}

#[cfg(test)]
mod lock_order_tests {
    use super::*;
    use crate::approval::approve_bill;
    use crate::calculation::calculate_draft;

    async fn id(pool: &PgPool, sql: &'static str, binds: &[Uuid]) -> Uuid {
        let mut q = sqlx::query_scalar::<_, Uuid>(sql);
        for b in binds {
            q = q.bind(*b);
        }
        q.fetch_one(pool).await.unwrap()
    }

    /// Пересчёт, утверждение и аннулирование одного периода в параллель не должны
    /// взаимно блокироваться: везде порядок «период → счёт».
    #[sqlx::test(migrations = "./migrations")]
    async fn calculate_approve_and_void_do_not_deadlock(pool: PgPool) {
        let acc = id(
            &pool,
            "INSERT INTO account (name) VALUES ('A') RETURNING id",
            &[],
        )
        .await;
        let owner = id(&pool, "INSERT INTO app_user (account_id, telegram_id, role) VALUES ($1, 1, 'owner') RETURNING id", &[acc]).await;
        let tenant = id(&pool, "INSERT INTO app_user (account_id, telegram_id, role) VALUES ($1, 3, 'tenant') RETURNING id", &[acc]).await;
        let prop = id(
            &pool,
            "INSERT INTO property (account_id, name) VALUES ($1, 'Flat') RETURNING id",
            &[acc],
        )
        .await;
        let ten = id(&pool, "INSERT INTO tenancy (property_id, tenant_id, starts_on) VALUES ($1, $2, '2026-01-01') RETURNING id", &[prop, tenant]).await;
        let period = id(&pool, "INSERT INTO billing_period (tenancy_id, starts_on, ends_on) VALUES ($1, '2026-09-01', '2026-09-30') RETURNING id", &[ten]).await;
        let meter = id(&pool, "INSERT INTO meter (property_id, kind, label, unit) VALUES ($1, 'cold_water', 'cold', 'm3') RETURNING id", &[prop]).await;
        let register = id(&pool, "INSERT INTO meter_register (meter_id, register_code, display_label, unit) VALUES ($1, 'main', 'cold', 'm3') RETURNING id", &[meter]).await;
        for (v, at) in [
            ("120", "2026-08-31T10:00:00Z"),
            ("125", "2026-09-30T10:00:00Z"),
        ] {
            sqlx::query("INSERT INTO meter_reading (meter_register_id, value, read_at, source, status, confirmed_by) VALUES ($1, $2::numeric, $3::timestamptz, 'manual', 'confirmed', $4)")
                .bind(register).bind(v).bind(at).bind(owner).execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO tariff (meter_register_id, unit_price, valid_from) VALUES ($1, 65, '2026-01-01')").bind(register).execute(&pool).await.unwrap();

        for _ in 0..100 {
            let mut tasks = Vec::new();
            for i in 0..6 {
                let p = pool.clone();
                tasks.push(tokio::spawn(async move {
                    // любой исход допустим, кроме взаимной блокировки (sqlx вернул бы ошибку 40P01)
                    let latest: Option<Uuid> = sqlx::query_scalar("SELECT id FROM bill WHERE billing_period_id = $1 ORDER BY version DESC LIMIT 1")
                        .bind(period).fetch_optional(&p).await.unwrap();
                    let res: Result<(), String> = match (i % 3, latest) {
                        (0, _) => calculate_draft(&p, period, owner).await.map(|_| ()).map_err(|e| e.to_string()),
                        (1, Some(b)) => approve_bill(&p, b, owner).await.map(|_| ()).map_err(|e| e.to_string()),
                        (2, Some(b)) => void_bill(&p, b, owner, "тест").await.map_err(|e| e.to_string()),
                        _ => Ok(()),
                    };
                    res
                }));
            }
            for t in tasks {
                if let Err(msg) = t.await.unwrap() {
                    assert!(!msg.contains("deadlock"), "deadlock: {msg}");
                }
            }
        }
    }
}

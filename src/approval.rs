//! Утверждение счёта (спека §6, §8, §10): атомарно, идемпотентно, с проверкой прав.
//!
//! Одна транзакция: блокировка счёта → проверка прав → смена статуса → audit + outbox.
//! Повтор команды и параллельные нажатия дают ровно один переход и одно логическое уведомление.

use sqlx::PgPool;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApproveOutcome {
    /// Этот вызов перевёл черновик в «утверждён».
    Approved,
    /// Счёт уже утверждён (повтор команды или проигравший гонку) — состояние не менялось.
    AlreadyApproved,
}

#[derive(Debug, Error)]
pub enum ApprovalError {
    #[error("счёт не найден")]
    NotFound,
    #[error("нет прав утверждать этот счёт")]
    Forbidden,
    #[error("счёт в состоянии {0} — утвердить нельзя")]
    InvalidState(String),
    #[error("за этот период уже утверждён другой счёт — аннулируйте его с причиной")]
    PeriodAlreadyBilled,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Утверждает черновик `bill_id` от имени `actor_id` (собственник/управляющий аккаунта объекта).
pub async fn approve_bill(
    pool: &PgPool,
    bill_id: Uuid,
    actor_id: Uuid,
) -> Result<ApproveOutcome, ApprovalError> {
    let mut tx = pool.begin().await?;

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
        return Err(ApprovalError::NotFound);
    }

    // FOR UPDATE: второй параллельный approve ждёт здесь и затем видит уже утверждённый счёт.
    let bill: Option<(String, Uuid, rust_decimal::Decimal, String)> = sqlx::query_as(
        "SELECT b.status, p.id, b.total, b.currency::text
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
    let Some((status, property_id, total, currency)) = bill else {
        return Err(ApprovalError::NotFound);
    };

    // Права проверяются на каждом запросе, по аккаунту объекта (спека §9).
    let allowed: Option<(bool,)> = sqlx::query_as(
        "SELECT true
           FROM app_user u
           JOIN property p ON p.account_id = u.account_id
          WHERE u.id = $1 AND p.id = $2 AND u.role IN ('owner', 'manager')",
    )
    .bind(actor_id)
    .bind(property_id)
    .fetch_optional(&mut *tx)
    .await?;
    if allowed.is_none() {
        return Err(ApprovalError::Forbidden);
    }

    match status.as_str() {
        "approved" | "paid" => return Ok(ApproveOutcome::AlreadyApproved),
        "draft" => {}
        other => return Err(ApprovalError::InvalidState(other.to_string())),
    }

    let updated = sqlx::query(
        "UPDATE bill SET status = 'approved', approved_by = $2, approved_at = now() WHERE id = $1",
    )
    .bind(bill_id)
    .bind(actor_id)
    .execute(&mut *tx)
    .await;
    match updated {
        Ok(_) => {}
        // bill_one_final_per_period: в периоде уже есть другой утверждённый счёт.
        Err(sqlx::Error::Database(e)) if e.constraint() == Some("bill_one_final_per_period") => {
            return Err(ApprovalError::PeriodAlreadyBilled);
        }
        Err(e) => return Err(e.into()),
    }

    sqlx::query(
        "UPDATE billing_period SET state = 'billed'
          WHERE id = (SELECT billing_period_id FROM bill WHERE id = $1)",
    )
    .bind(bill_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO audit_event (actor_id, action, object_type, object_id, details)
         VALUES ($1, 'bill.approve', 'bill', $2, jsonb_build_object('total', $3::text, 'currency', $4::text))",
    )
    .bind(actor_id)
    .bind(bill_id)
    .bind(total.to_string())
    .bind(&currency)
    .execute(&mut *tx)
    .await?;

    // dedup_key: одно логическое уведомление на счёт, что бы ни происходило с повторами.
    sqlx::query(
        "INSERT INTO outbox_event (dedup_key, kind, payload)
         VALUES ($1, 'bill.approved', jsonb_build_object('bill_id', $2::text))
         ON CONFLICT (dedup_key) DO NOTHING",
    )
    .bind(format!("bill.approved:{bill_id}"))
    .bind(bill_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(ApproveOutcome::Approved)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Seed {
        owner: Uuid,
        stranger: Uuid,
        tenant: Uuid,
        period: Uuid,
        bill: Uuid,
    }

    async fn scalar_uuid(pool: &PgPool, sql: &'static str, binds: &[Uuid]) -> Uuid {
        let mut q = sqlx::query_scalar::<_, Uuid>(sql);
        for b in binds {
            q = q.bind(*b);
        }
        q.fetch_one(pool).await.unwrap()
    }

    async fn seed(pool: &PgPool) -> Seed {
        let acc = scalar_uuid(
            pool,
            "INSERT INTO account (name) VALUES ('A') RETURNING id",
            &[],
        )
        .await;
        let other = scalar_uuid(
            pool,
            "INSERT INTO account (name) VALUES ('B') RETURNING id",
            &[],
        )
        .await;
        let owner = scalar_uuid(pool, "INSERT INTO app_user (account_id, telegram_id, role) VALUES ($1, 1, 'owner') RETURNING id", &[acc]).await;
        let stranger = scalar_uuid(pool, "INSERT INTO app_user (account_id, telegram_id, role) VALUES ($1, 2, 'owner') RETURNING id", &[other]).await;
        let tenant = scalar_uuid(pool, "INSERT INTO app_user (account_id, telegram_id, role) VALUES ($1, 3, 'tenant') RETURNING id", &[acc]).await;
        let prop = scalar_uuid(
            pool,
            "INSERT INTO property (account_id, name) VALUES ($1, 'Flat') RETURNING id",
            &[acc],
        )
        .await;
        let ten = scalar_uuid(pool, "INSERT INTO tenancy (property_id, tenant_id, starts_on) VALUES ($1, $2, '2026-01-01') RETURNING id", &[prop, tenant]).await;
        let period = scalar_uuid(pool, "INSERT INTO billing_period (tenancy_id, starts_on, ends_on) VALUES ($1, '2026-09-01', '2026-09-30') RETURNING id", &[ten]).await;
        let bill = new_bill(pool, period, 1).await;
        Seed {
            owner,
            stranger,
            tenant,
            period,
            bill,
        }
    }

    async fn new_bill(pool: &PgPool, period: Uuid, version: i32) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO bill (billing_period_id, version, total, snapshot) VALUES ($1, $2, 325.00, '{}') RETURNING id",
        )
        .bind(period)
        .bind(version)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn count(pool: &PgPool, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn approve_moves_draft_to_approved_with_audit_and_one_outbox(pool: PgPool) {
        let s = seed(&pool).await;
        assert_eq!(
            approve_bill(&pool, s.bill, s.owner).await.unwrap(),
            ApproveOutcome::Approved
        );

        let status: String = sqlx::query_scalar("SELECT status FROM bill WHERE id = $1")
            .bind(s.bill)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "approved");
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM audit_event WHERE action = 'bill.approve'"
            )
            .await,
            1
        );
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM outbox_event WHERE kind = 'bill.approved'"
            )
            .await,
            1
        );
        let state: String = sqlx::query_scalar("SELECT state FROM billing_period WHERE id = $1")
            .bind(s.period)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "billed");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn repeated_approve_is_a_noop(pool: PgPool) {
        let s = seed(&pool).await;
        approve_bill(&pool, s.bill, s.owner).await.unwrap();
        assert_eq!(
            approve_bill(&pool, s.bill, s.owner).await.unwrap(),
            ApproveOutcome::AlreadyApproved
        );
        assert_eq!(count(&pool, "SELECT count(*) FROM audit_event").await, 1);
        assert_eq!(count(&pool, "SELECT count(*) FROM outbox_event").await, 1);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn concurrent_approves_yield_one_transition_and_one_notification(pool: PgPool) {
        let s = seed(&pool).await;
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let (p, bill, owner) = (pool.clone(), s.bill, s.owner);
            tasks.push(tokio::spawn(async move {
                approve_bill(&p, bill, owner).await.unwrap()
            }));
        }
        let mut approved = 0;
        for t in tasks {
            if t.await.unwrap() == ApproveOutcome::Approved {
                approved += 1;
            }
        }
        assert_eq!(approved, 1);
        assert_eq!(count(&pool, "SELECT count(*) FROM audit_event").await, 1);
        assert_eq!(count(&pool, "SELECT count(*) FROM outbox_event").await, 1);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn foreign_account_and_tenant_cannot_approve(pool: PgPool) {
        let s = seed(&pool).await;
        assert!(matches!(
            approve_bill(&pool, s.bill, s.stranger).await,
            Err(ApprovalError::Forbidden)
        ));
        assert!(matches!(
            approve_bill(&pool, s.bill, s.tenant).await,
            Err(ApprovalError::Forbidden)
        ));
        let status: String = sqlx::query_scalar("SELECT status FROM bill WHERE id = $1")
            .bind(s.bill)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "draft");
        assert_eq!(count(&pool, "SELECT count(*) FROM outbox_event").await, 0);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn forbidden_check_precedes_idempotent_answer(pool: PgPool) {
        // Чужой пользователь не должен узнать, что счёт уже утверждён.
        let s = seed(&pool).await;
        approve_bill(&pool, s.bill, s.owner).await.unwrap();
        assert!(matches!(
            approve_bill(&pool, s.bill, s.stranger).await,
            Err(ApprovalError::Forbidden)
        ));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn second_version_cannot_be_approved_while_first_is_final(pool: PgPool) {
        let s = seed(&pool).await;
        approve_bill(&pool, s.bill, s.owner).await.unwrap();
        let v2 = new_bill(&pool, s.period, 2).await;
        assert!(matches!(
            approve_bill(&pool, v2, s.owner).await,
            Err(ApprovalError::PeriodAlreadyBilled)
        ));
        assert_eq!(count(&pool, "SELECT count(*) FROM outbox_event").await, 1);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn voided_bill_cannot_be_approved_and_correction_after_void_can(pool: PgPool) {
        let s = seed(&pool).await;
        approve_bill(&pool, s.bill, s.owner).await.unwrap();
        sqlx::query(
            "UPDATE bill SET status = 'voided', void_reason = 'ошибка в показании' WHERE id = $1",
        )
        .bind(s.bill)
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            approve_bill(&pool, s.bill, s.owner).await,
            Err(ApprovalError::InvalidState(_))
        ));

        let v2 = new_bill(&pool, s.period, 2).await;
        assert_eq!(
            approve_bill(&pool, v2, s.owner).await.unwrap(),
            ApproveOutcome::Approved
        );
        assert_eq!(count(&pool, "SELECT count(*) FROM outbox_event").await, 2);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn unknown_bill_is_not_found(pool: PgPool) {
        let s = seed(&pool).await;
        assert!(matches!(
            approve_bill(&pool, Uuid::new_v4(), s.owner).await,
            Err(ApprovalError::NotFound)
        ));
    }
}

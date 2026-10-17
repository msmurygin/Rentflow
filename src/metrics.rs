//! Метрики закрытой беты: только счётчики по таблицам, без имён, адресов и показаний.
//! Демо-пользователи (`telegram_id < 0`) в них не входят.

use crate::app::{App, now_iso};
use chrono::{Duration, NaiveDateTime};
use serde::Serialize;

#[derive(Debug, Serialize, PartialEq)]
pub struct Metrics {
    pub owners: i64,
    pub properties: i64,
    pub tenants_joined: i64,
    pub invites_created: i64,
    pub invites_used: i64,
    pub bills_approved_or_paid: i64,
    pub bills_paid: i64,
    /// Воронка первого цикла: владельцы, дошедшие до очередного шага.
    pub funnel_with_property: i64,
    pub funnel_with_tenant: i64,
    pub funnel_with_approved_bill: i64,
    /// Пользователи с действием в API за 7 суток.
    pub active_7d: i64,
}

impl App {
    pub async fn metrics(&self, now: NaiveDateTime) -> Result<Metrics, sqlx::Error> {
        let one = |sql: &'static str| async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(self.pool())
                .await
        };
        let since = now_iso(now - Duration::days(7));
        Ok(Metrics {
            owners: one("SELECT COUNT(DISTINCT m.user_id) FROM membership m JOIN app_user u ON u.id = m.user_id WHERE m.role = 'owner' AND u.telegram_id >= 0").await?,
            properties: one("SELECT COUNT(DISTINCT m.property_id) FROM membership m JOIN app_user u ON u.id = m.user_id WHERE m.role = 'owner' AND u.telegram_id >= 0").await?,
            tenants_joined: one("SELECT COUNT(*) FROM membership m JOIN app_user u ON u.id = m.user_id WHERE m.role = 'tenant' AND u.telegram_id >= 0").await?,
            invites_created: one("SELECT COUNT(*) FROM invite i WHERE i.property_id IN (SELECT m.property_id FROM membership m JOIN app_user u ON u.id = m.user_id WHERE m.role = 'owner' AND u.telegram_id >= 0)").await?,
            invites_used: one("SELECT COUNT(*) FROM invite i WHERE i.used_by IS NOT NULL AND i.property_id IN (SELECT m.property_id FROM membership m JOIN app_user u ON u.id = m.user_id WHERE m.role = 'owner' AND u.telegram_id >= 0)").await?,
            bills_approved_or_paid: one("SELECT COUNT(*) FROM bill b WHERE b.status IN ('approved','paid') AND b.property_id IN (SELECT m.property_id FROM membership m JOIN app_user u ON u.id = m.user_id WHERE m.role = 'owner' AND u.telegram_id >= 0)").await?,
            bills_paid: one("SELECT COUNT(*) FROM bill b WHERE b.status = 'paid' AND b.property_id IN (SELECT m.property_id FROM membership m JOIN app_user u ON u.id = m.user_id WHERE m.role = 'owner' AND u.telegram_id >= 0)").await?,
            funnel_with_property: one("SELECT COUNT(DISTINCT m.user_id) FROM membership m JOIN app_user u ON u.id = m.user_id WHERE m.role = 'owner' AND u.telegram_id >= 0").await?,
            funnel_with_tenant: one("SELECT COUNT(DISTINCT o.user_id) FROM membership o JOIN app_user u ON u.id = o.user_id JOIN membership t ON t.property_id = o.property_id AND t.role = 'tenant' WHERE o.role = 'owner' AND u.telegram_id >= 0").await?,
            funnel_with_approved_bill: one("SELECT COUNT(DISTINCT o.user_id) FROM membership o JOIN app_user u ON u.id = o.user_id JOIN bill b ON b.property_id = o.property_id AND b.status IN ('approved','paid') WHERE o.role = 'owner' AND u.telegram_id >= 0").await?,
            active_7d: sqlx::query_scalar::<_, i64>("SELECT COUNT(DISTINCT actor) FROM audit_log WHERE actor >= 0 AND at >= ?")
                .bind(since)
                .fetch_one(self.pool())
                .await?,
        })
    }
}

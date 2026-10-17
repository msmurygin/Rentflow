//! Миграция 0008 на данных «как в production»: один объект, владелец, арендатор, утверждённый и оплаченный счета.
//! Права переносятся из того, что уже записано в базе; никому не назначаются по знанию Telegram ID.

use rentflow::app::{App, Config, Incoming, Sender};
use rentflow::ocr::NoExtractor;
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use std::sync::Arc;

const OLD: [&str; 7] = [
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_seen_updates.sql"),
    include_str!("../migrations/0003_entry_form.sql"),
    include_str!("../migrations/0004_proof_photos.sql"),
    include_str!("../migrations/0005_monthly_tariffs.sql"),
    include_str!("../migrations/0006_api_sessions_audit.sql"),
    include_str!("../migrations/0007_reading_schedule.sql"),
];
const NEW: &str = include_str!("../migrations/0008_membership.sql");
const SELF_SERVICE: &str = include_str!("../migrations/0009_selfservice.sql");

async fn legacy() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    for sql in OLD {
        sqlx::raw_sql(sql).execute(&pool).await.unwrap();
    }
    sqlx::raw_sql(
        "INSERT INTO app_user (id, telegram_id, chat_id, role, created_at) VALUES (7, 307, 307, 'landlord', '2026-10-01'), (9, 555, 555, 'tenant', '2026-10-02'), (11, 999, 999, 'tenant', '2026-10-03');
         INSERT INTO property (id, landlord_id, name, created_at, reading_day) VALUES (3, 7, 'Волжский 25', '2026-10-01', 25);
         INSERT INTO tenancy (id, property_id, tenant_user_id, tenant_name) VALUES (4, 3, 9, 'Максим');
         INSERT INTO register (id, property_id, code, label, unit) VALUES (1, 3, 'cold', 'ХВС', 'м³'), (2, 3, 'hot', 'ГВС', 'м³'), (3, 3, 't1', 'T1', 'кВт·ч'), (4, 3, 't2', 'T2', 'кВт·ч');
         INSERT INTO tariff (property_id, code, price, valid_from, created_at) VALUES (3, 't1', '8', '2026-10-01', 'x');
         INSERT INTO bill (id, property_id, period, version, status, total, snapshot, created_at) VALUES (1, 3, '2026-09', 1, 'paid', '4487.00', '{\"keep\":1}', 'x'), (2, 3, '2026-10', 1, 'approved', '2100.00', '{\"keep\":2}', 'x');",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

#[tokio::test]
async fn existing_owner_tenant_and_bills_survive_with_the_same_ids() {
    let pool = legacy().await;
    let before: Vec<(i64, String, String, String)> =
        sqlx::query_as("SELECT id, status, total, snapshot FROM bill ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    sqlx::raw_sql(NEW).execute(&pool).await.unwrap();

    let members: Vec<(i64, i64, String)> =
        sqlx::query_as("SELECT user_id, property_id, role FROM membership ORDER BY user_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        members,
        [(7, 3, "owner".into()), (9, 3, "tenant".into())],
        "только записанные в базе: владелец и арендатор; пользователь 11 — никто"
    );
    let after: Vec<(i64, String, String, String)> =
        sqlx::query_as("SELECT id, status, total, snapshot FROM bill ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(before, after, "счета не тронуты");
    let (props, tenancy, tariffs): (i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM property), (SELECT count(*) FROM tenancy), (SELECT count(*) FROM tariff)").fetch_one(&pool).await.unwrap();
    assert_eq!((props, tenancy, tariffs), (1, 1, 1));
    let active: Option<i64> =
        sqlx::query_scalar("SELECT active_property_id FROM app_user WHERE id = 7")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        active, None,
        "активный объект выводится из членства, а не записывается задним числом"
    );
    // идемпотентность на повторном применении логики вставки: уникальность не даёт задвоить права
    let dup = sqlx::query("INSERT INTO membership (user_id, property_id, role, created_at) VALUES (7, 3, 'owner', 'x')").execute(&pool).await;
    assert!(dup.is_err());
    let bad = sqlx::query("INSERT INTO membership (user_id, property_id, role, created_at) VALUES (11, 3, 'admin', 'x')").execute(&pool).await;
    assert!(bad.is_err(), "роль проверяется ограничением");
}

#[tokio::test]
async fn after_migration_the_old_owner_works_without_any_telegram_id_config_and_strangers_do_not() {
    let pool = legacy().await;
    sqlx::raw_sql(NEW).execute(&pool).await.unwrap();
    sqlx::raw_sql(SELF_SERVICE).execute(&pool).await.unwrap();
    let app = App::new(
        pool,
        Config {
            bot_username: "b".into(),
            ..Default::default()
        },
        Arc::new(NoExtractor),
    );
    let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 17)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    let from = |id: i64| Sender {
        telegram_id: id,
        chat_id: id,
        name: "x".into(),
    };

    // прежний владелец видит свой объект и счёт — права идут из membership
    let r = app
        .handle(&from(307), Incoming::Text("/status".into()), now)
        .await;
    assert!(r[0].text.contains("Волжский 25"), "{r:?}");
    let r = app
        .handle(&from(307), Incoming::Text("/reissue".into()), now)
        .await;
    assert!(
        !r[0].buttons.is_empty(),
        "утверждённый счёт доступен владельцу: {r:?}"
    );
    // прежний арендатор остаётся арендатором: не владелец
    let r = app
        .handle(&from(555), Incoming::Text("/tariffs".into()), now)
        .await;
    assert!(!r[0].text.contains("История тарифов"), "{r:?}");
    // пользователь без членства (11) и вообще чужой (1) — не участники
    for id in [999, 1] {
        let r = app
            .handle(&from(id), Incoming::Text("/status".into()), now)
            .await;
        assert!(!r[0].text.contains("Волжский 25"), "{id}: {r:?}");
        let r = app
            .handle(&from(id), Incoming::Text("/reissue".into()), now)
            .await;
        assert!(
            r[0].buttons
                .iter()
                .flatten()
                .all(|b| !b.data.starts_with("ri:")),
            "{id}: чужой счёт в списке: {r:?}"
        );
    }
}

#[tokio::test]
async fn existing_registers_stay_enabled_after_the_self_service_migration() {
    let pool = legacy().await;
    sqlx::raw_sql(NEW).execute(&pool).await.unwrap();
    sqlx::raw_sql(SELF_SERVICE).execute(&pool).await.unwrap();
    let rows: Vec<(String, i64, Option<String>)> =
        sqlx::query_as("SELECT code, enabled, meter_group FROM register ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        [
            ("cold".to_string(), 1, Some("cold".to_string())),
            ("hot".to_string(), 1, Some("hot".to_string())),
            ("t1".to_string(), 1, Some("electricity".to_string())),
            ("t2".to_string(), 1, Some("electricity".to_string())),
        ]
    );
    let tz: i64 = sqlx::query_scalar("SELECT tz_offset_hours FROM property WHERE id = 3")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tz, 3, "существующая московская квартира получает +3");
}

//! Миграция 0005: дневные тарифы нормализуются в месячные без сброса базы; утверждённые счета не меняются.

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

const OLD: [&str; 4] = [
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_seen_updates.sql"),
    include_str!("../migrations/0003_entry_form.sql"),
    include_str!("../migrations/0004_proof_photos.sql"),
];
const NEW: &str = include_str!("../migrations/0005_monthly_tariffs.sql");

async fn legacy_db() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    for sql in OLD {
        sqlx::raw_sql(sql).execute(&pool).await.unwrap();
    }
    pool
}

#[tokio::test]
async fn daily_tariffs_become_monthly_and_approved_bills_survive() {
    let pool = legacy_db().await;
    sqlx::raw_sql(
        "INSERT INTO app_user (id, telegram_id, chat_id, role, created_at) VALUES (1, 1, 1, 'landlord', '2026-06-01');
         INSERT INTO property (id, landlord_id, name, sewer_enabled, created_at) VALUES (1, 1, 'A', 1, '2026-06-01'), (2, 1, 'B', 0, '2026-06-01');
         INSERT INTO tariff (property_id, code, price, valid_from, created_at) VALUES
           (1, 't1', '8.60', '2026-06-15', 'x'), (1, 't1', '9', '2026-10-03', 'x'), (1, 't1', '9.5', '2026-10-17', 'x'),
           (1, 'sewer', '51.62', '2026-06-15', 'x'), (2, 'sewer', '40', '2026-06-15', 'x');
         INSERT INTO bill (id, property_id, period, version, status, total, snapshot, created_at)
           VALUES (7, 1, '2026-09', 1, 'approved', '2104.29', '{\"keep\":\"me\"}', '2026-09-26');",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::raw_sql(NEW).execute(&pool).await.unwrap();

    let t1: Vec<(String, String)> = sqlx::query_as("SELECT valid_from, price FROM tariff WHERE property_id = 1 AND code = 't1' ORDER BY valid_from")
        .fetch_all(&pool)
        .await
        .unwrap();
    // июнь сохранён, два октябрьских слились в одну (введённую последней)
    assert_eq!(
        t1,
        [
            ("2026-06-01".into(), "8.60".into()),
            ("2026-10-01".into(), "9.5".into())
        ]
    );
    // выключенное водоотведение стало месячной версией с прежней ценой
    let off: (String, i64) = sqlx::query_as("SELECT price, enabled FROM tariff WHERE property_id = 2 AND code = 'sewer' ORDER BY valid_from DESC")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(off, ("40".into(), 0));
    // утверждённый счёт не изменился
    let bill: (String, String, String) =
        sqlx::query_as("SELECT status, total, snapshot FROM bill WHERE id = 7")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        bill,
        (
            "approved".into(),
            "2104.29".into(),
            "{\"keep\":\"me\"}".into()
        )
    );
    // новый статус разрешён, а второй действующий счёт за период по-прежнему запрещён
    sqlx::query("INSERT INTO bill (property_id, period, version, status, total, snapshot, created_at) VALUES (1, '2026-09', 2, 'void', '0', '{}', 'x')")
        .execute(&pool)
        .await
        .unwrap();
    let dup = sqlx::query("INSERT INTO bill (property_id, period, version, status, total, snapshot, created_at) VALUES (1, '2026-09', 3, 'draft', '0', '{}', 'x')")
        .execute(&pool)
        .await;
    assert!(dup.is_err());
    // и два тарифа одного месяца больше не заводятся
    let dup = sqlx::query("INSERT INTO tariff (property_id, code, price, valid_from, created_at) VALUES (1, 't1', '1', '2026-10-20', 'x')")
        .execute(&pool)
        .await;
    assert!(dup.is_err());
}

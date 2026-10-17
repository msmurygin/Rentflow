//! Резервная копия → проверка → восстановление: база, фото и связи между ними.

use chrono::NaiveDate;
use rentflow::app::{App, Config};
use rentflow::backup::{create_backup, restore_backup, verify_backup};
use rentflow::ocr::NoExtractor;
use rentflow::setup::NewProperty;
use std::{path::PathBuf, sync::Arc};

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("rentflow-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn now() -> chrono::NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 10, 17)
        .unwrap()
        .and_hms_opt(3, 0, 0)
        .unwrap()
}

/// Рабочая база в файле (WAL, как в production) с квартирой, показаниями и фото.
async fn live() -> (App, PathBuf, PathBuf) {
    let data = tmp("data");
    let photos = data.join("photos");
    std::fs::create_dir_all(&photos).unwrap();
    let pool = rentflow::db::connect(&format!("sqlite://{}/rentflow.db", data.display()))
        .await
        .unwrap();
    let app = App::new(
        pool.clone(),
        Config {
            photo_dir: Some(photos.clone()),
            ..Default::default()
        },
        Arc::new(NoExtractor),
    );
    sqlx::query("INSERT INTO app_user (telegram_id, chat_id, role, created_at) VALUES (1, 1, 'landlord', 'x')").execute(&pool).await.unwrap();
    let uid: i64 = sqlx::query_scalar("SELECT id FROM app_user")
        .fetch_one(&pool)
        .await
        .unwrap();
    let pid = app
        .create_property(
            uid,
            &NewProperty {
                name: "Квартира".into(),
                tenant_name: "Иван".into(),
                base_date: None,
                baselines: ["cold", "hot", "t1", "t2"]
                    .map(|c| (c.to_string(), "1".to_string()))
                    .to_vec(),
                prices: ["cold", "hot", "t1", "t2"]
                    .map(|c| (c.to_string(), "2".to_string()))
                    .to_vec(),
                sewer_price: Some("3".into()),
            },
            now(),
        )
        .await
        .unwrap();
    // подтверждённое показание с фото на диске
    std::fs::write(photos.join("abc.jpg"), [0xFF, 0xD8, 0xFF, 1, 2, 3]).unwrap();
    sqlx::query(
        "INSERT INTO reading (register_id, period, kind, value, source, photo_path, status, created_at)
         SELECT id, '2026-10', 'submitted', '5', 'manual', ?, 'confirmed', 'x' FROM register WHERE property_id = ? AND code = 'cold'",
    )
    .bind(photos.join("abc.jpg").to_string_lossy().to_string())
    .bind(pid)
    .execute(&pool)
    .await
    .unwrap();
    (app, data, photos)
}

#[tokio::test]
async fn backup_verifies_and_restores_database_photos_and_links() {
    let (app, data, photos) = live().await;
    let out = tmp("out");
    let dir = create_backup(app.pool(), Some(&photos), &out, now())
        .await
        .unwrap();
    let report = verify_backup(&dir).await.unwrap();
    assert!(report.ok(), "{:?}", report.problems);
    assert!(report.rows["reading"] >= 5 && report.rows["property"] == 1);
    assert!(
        out.join("last_backup.json").is_file(),
        "статус для мониторинга записан"
    );

    // восстановление в пустой каталог и запуск приложения на копии (миграции применяются поверх без изменений)
    let target = tmp("restored");
    restore_backup(&dir, &target).await.unwrap();
    let pool = rentflow::db::connect(&format!("sqlite://{}/rentflow.db", target.display()))
        .await
        .unwrap();
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM reading WHERE photo_path LIKE '%abc.jpg'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(n, 1);
    assert!(target.join("photos/abc.jpg").is_file());
    let tariffs: i64 = sqlx::query_scalar("SELECT count(*) FROM tariff")
        .fetch_one(app.pool())
        .await
        .unwrap();
    let restored_tariffs: i64 = sqlx::query_scalar("SELECT count(*) FROM tariff")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tariffs, restored_tariffs);
    let _ = data;
}

#[tokio::test]
async fn a_tampered_or_incomplete_backup_is_detected_and_not_restored() {
    let (app, _data, photos) = live().await;
    let out = tmp("out");
    let dir = create_backup(app.pool(), Some(&photos), &out, now())
        .await
        .unwrap();

    // повреждённое фото
    std::fs::write(dir.join("photos/abc.jpg"), b"corrupted").unwrap();
    let r = verify_backup(&dir).await.unwrap();
    assert!(
        r.problems.iter().any(|p| p.contains("abc.jpg")),
        "{:?}",
        r.problems
    );
    assert!(restore_backup(&dir, &tmp("t1")).await.is_err());

    // потерянное фото
    std::fs::remove_file(dir.join("photos/abc.jpg")).unwrap();
    let r = verify_backup(&dir).await.unwrap();
    assert!(!r.ok());
}

#[tokio::test]
async fn restore_never_overwrites_a_non_empty_target() {
    let (app, _data, photos) = live().await;
    let dir = create_backup(app.pool(), Some(&photos), &tmp("out"), now())
        .await
        .unwrap();
    let busy = tmp("busy");
    std::fs::write(busy.join("rentflow.db"), b"production data").unwrap();
    let err = restore_backup(&dir, &busy).await.unwrap_err().to_string();
    assert!(err.contains("не пуст"), "{err}");
    assert_eq!(
        std::fs::read(busy.join("rentflow.db")).unwrap(),
        b"production data"
    );
}

#[tokio::test]
async fn backup_is_consistent_while_the_database_is_being_written() {
    let (app, _data, photos) = live().await;
    let pool = app.pool().clone();
    let writer = tokio::spawn(async move {
        for i in 0..200 {
            sqlx::query(
                "INSERT INTO audit_log (at, actor, action, entity) VALUES ('x', 1, 'noise', ?)",
            )
            .bind(i.to_string())
            .execute(&pool)
            .await
            .unwrap();
        }
    });
    let dir = create_backup(app.pool(), Some(&photos), &tmp("out"), now())
        .await
        .unwrap();
    writer.await.unwrap();
    let r = verify_backup(&dir).await.unwrap();
    assert!(r.ok(), "{:?}", r.problems);
}

#[tokio::test]
async fn restored_copy_accepts_the_newest_migrations() {
    let (app, _data, photos) = live().await;
    let dir = create_backup(app.pool(), Some(&photos), &tmp("out"), now())
        .await
        .unwrap();
    let target = tmp("restored");
    restore_backup(&dir, &target).await.unwrap();
    let report =
        rentflow::backup::migrate_check(&format!("sqlite://{}/rentflow.db", target.display()))
            .await
            .unwrap();
    assert!(report.ok(), "{:?}", report.problems);
    assert_eq!(report.rows["property"], 1);
}

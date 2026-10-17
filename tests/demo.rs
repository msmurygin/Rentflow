//! Демо-квартира: видна без регистрации, только для чтения, изолирована от настоящих данных и метрик.
mod common;
use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;

async fn start(w: &World) -> String {
    let (s, v) = w
        .call(Method::POST, "/api/demo/start", None, None, None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["demo"], true);
    v["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn the_demo_has_history_and_is_created_once() {
    let w = World::new().await;
    let t = start(&w).await;
    let t2 = start(&w).await;
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM property")
        .fetch_one(w.app.pool())
        .await
        .unwrap();
    assert_eq!(n, 1, "демо-квартира создаётся один раз");
    for tok in [&t, &t2] {
        let (s, v) = w
            .call(Method::GET, "/api/bills", Some(tok), None, None)
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let statuses: Vec<_> = v["bills"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["status"].as_str().unwrap().to_string())
            .collect();
        assert!(statuses.contains(&"paid".to_string()), "{statuses:?}");
        assert!(statuses.contains(&"draft".to_string()), "{statuses:?}");
    }
}

#[tokio::test]
async fn the_demo_cannot_write_anything() {
    let w = World::new().await;
    let t = start(&w).await;
    let (_, v) = w
        .call(Method::GET, "/api/bills", Some(&t), None, None)
        .await;
    let draft = v["bills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["status"] == "draft")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    for (m, p, b) in [
        (
            Method::POST,
            format!("/api/bills/{draft}/approve"),
            json!({}),
        ),
        (
            Method::POST,
            format!("/api/bills/{draft}/void"),
            json!({"reason": "x"}),
        ),
        (Method::POST, "/api/invitations".to_string(), json!({})),
        (
            Method::POST,
            "/api/properties".to_string(),
            json!({"name": "x"}),
        ),
        (
            Method::PUT,
            "/api/tariffs".to_string(),
            json!({"changes": []}),
        ),
    ] {
        let (s, _) = w.call(m.clone(), &p, Some(&t), None, Some(b)).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p}");
    }
    let (_, v) = w
        .call(Method::GET, "/api/bills", Some(&t), None, None)
        .await;
    assert!(
        v["bills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"] == draft && b["status"] == "draft"),
        "счёт не изменился"
    );
}

#[tokio::test]
async fn the_demo_is_invisible_to_real_users_and_metrics_and_reminders() {
    let w = World::new().await;
    start(&w).await;
    let demo_prop: i64 = sqlx::query_scalar("SELECT id FROM property")
        .fetch_one(w.app.pool())
        .await
        .unwrap();
    let a = w.owner(OWNER_A).await;
    let (s, _) = w
        .call(Method::GET, "/api/bills", Some(&a), Some(demo_prop), None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, v) = w
        .call(Method::GET, "/api/properties", Some(&a), None, None)
        .await;
    assert!(v["properties"].as_array().unwrap().is_empty(), "{v}");

    let m = w.app.metrics(now()).await.unwrap();
    assert_eq!(
        (
            m.owners,
            m.properties,
            m.tenants_joined,
            m.bills_approved_or_paid,
            m.active_7d
        ),
        (0, 0, 0, 0, 1), // активен только настоящий владелец A
        "демо не входит в метрики"
    );

    // напоминания демо-пользователям не создаются ни в какой день месяца
    for day in 1..=31 {
        let t = chrono::NaiveDate::from_ymd_opt(2026, 8, day.min(31))
            .unwrap()
            .and_hms_opt(20, 0, 0)
            .unwrap();
        for r in w.app.due_reminders(t).await.unwrap() {
            assert!(r.chat_id > 0, "демо получило напоминание: {r:?}");
        }
    }
}

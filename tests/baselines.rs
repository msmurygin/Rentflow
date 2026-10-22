//! Исправление стартовых показаний уже созданной квартиры: пока нет счетов — можно, после первого счёта — нельзя.
mod common;
use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;

async fn put(
    w: &World,
    tok: &str,
    prop: i64,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    w.call(
        Method::PUT,
        "/api/property/baselines",
        Some(tok),
        Some(prop),
        Some(body),
    )
    .await
}

async fn baselines(w: &World, tok: &str, prop: i64) -> Vec<(String, String)> {
    let (_, v) = w
        .call(Method::GET, "/api/property", Some(tok), Some(prop), None)
        .await;
    v["property"]["registers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["code"].as_str().unwrap().into(),
                r["baseline"].as_str().unwrap().into(),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_owner_resets_baselines_and_the_first_bill_counts_from_them() {
    let w = World::new().await;
    let o = w.owner(OWNER_A).await;
    let p = w.create_property(&o, "Кв").await;
    let t = w.join_tenant(&o, p, TENANT_A).await;
    let (_, v) = w
        .call(Method::GET, "/api/property", Some(&o), Some(p), None)
        .await;
    assert_eq!(v["property"]["baselines_editable"], true);

    let (s, r) = put(
        &w, &o, p,
        json!({"baselines": {"cold": "100", "hot": "50,5", "t1": "1000", "t2": "400"}, "base_date": "2026-09-30"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let b = baselines(&w, &o, p).await;
    assert!(
        b.contains(&("cold".into(), "100".into())) && b.contains(&("hot".into(), "50.5".into())),
        "{b:?}"
    );

    // история не затёрта: старое показание осталось как superseded
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reading WHERE kind = 'baseline' AND status = 'superseded'",
    )
    .fetch_one(w.app.pool())
    .await
    .unwrap();
    assert_eq!(n, 4);
    let audit: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'baselines_update'")
            .fetch_one(w.app.pool())
            .await
            .unwrap();
    assert_eq!(audit, 1);

    // первый расчёт идёт от новых показаний: расход 2 / 1.5 / 10 / 5
    let bill = w.bill(&o, &t, p, ["102", "52", "1010", "405"]).await;
    let (_, d) = w
        .call(
            Method::GET,
            &format!("/api/bills/{bill}"),
            Some(&o),
            Some(p),
            None,
        )
        .await;
    let lines = d["snapshot"].to_string();
    assert!(lines.contains("102") && lines.contains("100"), "{lines}");
    assert!(
        !lines.contains("\"1\""),
        "в расчёте остались старые показания: {lines}"
    );
}

#[tokio::test]
async fn after_the_first_bill_baselines_are_frozen() {
    let w = World::new().await;
    let o = w.owner(OWNER_A).await;
    let p = w.create_property(&o, "Кв").await;
    let t = w.join_tenant(&o, p, TENANT_A).await;
    w.bill(&o, &t, p, ["2", "2", "10", "10"]).await;
    let before = baselines(&w, &o, p).await;
    let (s, _) = put(&w, &o, p, json!({"baselines": {"cold": "0"}})).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(baselines(&w, &o, p).await, before);
    let (_, v) = w
        .call(Method::GET, "/api/property", Some(&o), Some(p), None)
        .await;
    assert_eq!(v["property"]["baselines_editable"], false);
}

#[tokio::test]
async fn bad_input_changes_nothing_and_only_the_owner_may_edit() {
    let w = World::new().await;
    let o = w.owner(OWNER_A).await;
    let p = w.create_property(&o, "Кв").await;
    let t = w.join_tenant(&o, p, TENANT_A).await;
    let before = baselines(&w, &o, p).await;
    for body in [
        json!({"baselines": {"cold": "abc"}}),
        json!({"baselines": {"cold": "-5"}}),
        json!({"baselines": {"gas": "5"}}),
        json!({"baselines": {}}),
        json!({"baselines": {"cold": "5"}, "base_date": "вчера"}),
        // одна ошибка в наборе — не применяется ни одно значение
        json!({"baselines": {"cold": "5", "hot": "oops"}}),
    ] {
        let (s, _) = put(&w, &o, p, body.clone()).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    assert_eq!(baselines(&w, &o, p).await, before);

    let (s, _) = put(&w, &t, p, json!({"baselines": {"cold": "5"}})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let b = w.owner(OWNER_B).await;
    let (s, _) = put(&w, &b, p, json!({"baselines": {"cold": "5"}})).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(baselines(&w, &o, p).await, before);
}

#[tokio::test]
async fn a_disabled_register_cannot_be_given_a_baseline() {
    let w = World::new().await;
    let o = w.owner(OWNER_A).await;
    let (s, v) = w
        .call(
            Method::POST,
            "/api/properties",
            Some(&o),
            None,
            Some(json!({
                "name": "Однотарифная", "tenant_name": "Ж",
                "baselines": {"cold":"1","hot":"1","t1":"1"},
                "prices": {"cold":"10","hot":"20","t1":"5"},
                "sewer_price": "7", "disabled": ["t2"], "proof_required": false
            })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let p = v["property"]["id"].as_i64().unwrap();
    let (s, _) = put(&w, &o, p, json!({"baselines": {"t2": "5"}})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

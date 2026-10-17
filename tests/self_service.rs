//! Самостоятельная работа новичка: регистрация, мастер с возобновлением, отключённые регистры, приглашение из приложения.
mod common;
use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;

fn full_body(name: &str) -> serde_json::Value {
    json!({
        "name": name, "tenant_name": "Жилец",
        "baselines": {"cold":"1","hot":"1","t1":"1","t2":"1"},
        "prices": {"cold":"10","hot":"20","t1":"5","t2":"3"},
        "sewer_price": "7", "proof_required": false
    })
}

async fn count(w: &World, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(w.app.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_newcomer_sees_nothing_until_they_register() {
    let w = World::new().await;
    let t = w.login(OWNER_A).await;
    let (s, v) = w.call(Method::GET, "/api/me", Some(&t), None, None).await;
    assert_eq!((s, v["role"].as_str()), (StatusCode::OK, Some("new")));
    for (m, p) in [
        (Method::GET, "/api/setup/draft"),
        (Method::PUT, "/api/setup/draft"),
        (Method::POST, "/api/setup/finish"),
        (Method::POST, "/api/properties"),
        (Method::GET, "/api/home"),
    ] {
        let (s, _) = w
            .call(
                m.clone(),
                p,
                Some(&t),
                None,
                Some(json!({"step":1,"data":{}})),
            )
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p}");
    }
    // регистрация идемпотентна
    for _ in 0..2 {
        let (s, _) = w
            .call(
                Method::POST,
                "/api/register",
                Some(&t),
                None,
                Some(json!({})),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
    }
    assert_eq!(count(&w, "SELECT count(*) FROM app_user").await, 1);
    let (_, v) = w.call(Method::GET, "/api/me", Some(&t), None, None).await;
    assert_eq!(
        (v["role"].as_str(), v["property_configured"].clone()),
        (Some("landlord"), json!(false))
    );
}

#[tokio::test]
async fn the_wizard_resumes_and_never_creates_properties_before_finish() {
    let w = World::new().await;
    let t = w.owner(OWNER_A).await;
    let (_, v) = w
        .call(Method::GET, "/api/setup/draft", Some(&t), None, None)
        .await;
    assert!(v["draft"].is_null());

    // шаги сохраняются, «Далее» можно жать сколько угодно — квартир не появляется
    for step in [1, 1, 2, 2, 3] {
        let (s, _) = w
            .call(
                Method::PUT,
                "/api/setup/draft",
                Some(&t),
                None,
                Some(json!({"step": step, "data": {"name": "Моя квартира"}})),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
    }
    assert_eq!(count(&w, "SELECT count(*) FROM property").await, 0);

    // повторный вход (новая сессия) продолжает с того же места
    let t2 = w.login(OWNER_A).await;
    let (_, v) = w
        .call(Method::GET, "/api/setup/draft", Some(&t2), None, None)
        .await;
    assert_eq!(
        (
            v["draft"]["step"].clone(),
            v["draft"]["data"]["name"].clone()
        ),
        (json!(3), json!("Моя квартира"))
    );

    // «Готово» с неполными данными: ошибка, черновик цел, квартир по-прежнему нет
    let (s, _) = w
        .call(
            Method::POST,
            "/api/setup/finish",
            Some(&t2),
            None,
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(count(&w, "SELECT count(*) FROM property").await, 0);
    let (_, v) = w
        .call(Method::GET, "/api/setup/draft", Some(&t2), None, None)
        .await;
    assert!(
        v["draft"]["property_id"].is_null(),
        "черновик остаётся доступным для исправления"
    );

    // полные данные → квартира; повторное «Готово» и параллельное — та же квартира
    w.call(
        Method::PUT,
        "/api/setup/draft",
        Some(&t2),
        None,
        Some(json!({"step": 6, "data": full_body("Моя квартира")})),
    )
    .await;
    let (a, b) = tokio::join!(
        w.call(
            Method::POST,
            "/api/setup/finish",
            Some(&t2),
            None,
            Some(json!({}))
        ),
        w.call(
            Method::POST,
            "/api/setup/finish",
            Some(&t2),
            None,
            Some(json!({}))
        )
    );
    let ok: Vec<_> = [&a, &b]
        .into_iter()
        .filter(|r| r.0 == StatusCode::OK)
        .collect();
    assert!(!ok.is_empty(), "{a:?} {b:?}");
    assert!(
        [&a, &b]
            .iter()
            .all(|r| r.0 == StatusCode::OK || r.0 == StatusCode::CONFLICT)
    );
    let (s, v) = w
        .call(
            Method::POST,
            "/api/setup/finish",
            Some(&t2),
            None,
            Some(json!({})),
        )
        .await;
    assert_eq!((s, v["already"].clone()), (StatusCode::OK, json!(true)));
    assert_eq!(
        count(&w, "SELECT count(*) FROM property").await,
        1,
        "ровно одна квартира"
    );
    assert_eq!(
        count(&w, "SELECT count(*) FROM bill").await,
        0,
        "bootstrap не создаёт начислений"
    );

    // новый запуск мастера после завершения — это уже следующая квартира
    w.call(
        Method::PUT,
        "/api/setup/draft",
        Some(&t2),
        None,
        Some(json!({"step": 6, "data": full_body("Вторая")})),
    )
    .await;
    let (s, v) = w
        .call(
            Method::POST,
            "/api/setup/finish",
            Some(&t2),
            None,
            Some(json!({})),
        )
        .await;
    assert_eq!((s, v["already"].clone()), (StatusCode::OK, json!(false)));
    assert_eq!(count(&w, "SELECT count(*) FROM property").await, 2);
}

#[tokio::test]
async fn a_property_without_t2_bills_only_what_is_enabled() {
    let w = World::new().await;
    let o = w.owner(OWNER_A).await;
    let (s, v) = w
        .call(
            Method::POST,
            "/api/properties",
            Some(&o),
            None,
            Some(json!({
                "name": "Однотарифная", "disabled": ["t2"],
                "baselines": {"cold":"1","hot":"1","t1":"1"},
                "prices": {"cold":"10","hot":"20","t1":"5"},
                "sewer_price": "7", "proof_required": false, "tz_offset_hours": 5
            })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let pid = v["property"]["id"].as_i64().unwrap();
    assert_eq!(v["property"]["registers"].as_array().unwrap().len(), 3);
    let tz: i64 = sqlx::query_scalar("SELECT tz_offset_hours FROM property WHERE id = ?")
        .bind(pid)
        .fetch_one(w.app.pool())
        .await
        .unwrap();
    assert_eq!(tz, 5);
    // отключённый регистр существует, но без показаний и тарифов; T1 и T2 — один физический прибор
    let t2: (i64, Option<String>) = sqlx::query_as(
        "SELECT enabled, meter_group FROM register WHERE property_id = ? AND code = 't2'",
    )
    .bind(pid)
    .fetch_one(w.app.pool())
    .await
    .unwrap();
    assert_eq!(t2, (0, Some("electricity".into())));
    assert_eq!(
        count(&w, "SELECT count(*) FROM tariff WHERE code = 't2'").await,
        0
    );

    let t = w.join_tenant(&o, pid, TENANT_A).await;
    let (_, f) = w
        .call(
            Method::GET,
            "/api/readings/draft",
            Some(&t),
            Some(pid),
            None,
        )
        .await;
    assert_eq!(f["registers"].as_array().unwrap().len(), 3);
    // три значения — комплект полон; четвёртого регистра нет
    for (code, val) in [("cold", "3"), ("hot", "2"), ("t1", "11")] {
        let (s, r) = w
            .call(
                Method::PUT,
                "/api/readings/draft",
                Some(&t),
                Some(pid),
                Some(json!({"code": code, "value": val})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{r}");
    }
    let (s, _) = w
        .call(
            Method::PUT,
            "/api/readings/draft",
            Some(&t),
            Some(pid),
            Some(json!({"code": "t2", "value": "5"})),
        )
        .await;
    assert_ne!(
        s,
        StatusCode::OK,
        "отключённый регистр не принимает показания"
    );
    let (s, r) = w
        .call(
            Method::POST,
            "/api/readings/submit",
            Some(&t),
            Some(pid),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let (_, v) = w
        .call(Method::GET, "/api/bills", Some(&o), Some(pid), None)
        .await;
    let id = v["bills"][0]["id"].as_i64().unwrap();
    let (_, d) = w
        .call(
            Method::GET,
            &format!("/api/bills/{id}"),
            Some(&o),
            Some(pid),
            None,
        )
        .await;
    // ХВС 2×10 + ГВС 1×20 + T1 10×5 + водоотведение (2+1)×7 = 20+20+50+21
    assert_eq!(
        d["total"].as_str().map(|t| t.parse::<f64>().unwrap()),
        Some(111.0),
        "{d}"
    );
    assert_eq!(d["snapshot"]["lines"].as_array().unwrap().len(), 4);
    let (_, h) = w
        .call(Method::GET, "/api/home", Some(&o), Some(pid), None)
        .await;
    assert_eq!(h["total_registers"], 3);
}

#[tokio::test]
async fn invalid_register_and_timezone_settings_are_refused() {
    let w = World::new().await;
    let o = w.owner(OWNER_A).await;
    let base = |extra: serde_json::Value| {
        let mut b = full_body("X");
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    };
    for body in [
        base(json!({"disabled": ["cold", "hot", "t1", "t2"]})),
        base(json!({"disabled": ["gas"]})),
        base(json!({"tz_offset_hours": 20})),
    ] {
        let (s, _) = w
            .call(
                Method::POST,
                "/api/properties",
                Some(&o),
                None,
                Some(body.clone()),
            )
            .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    assert_eq!(count(&w, "SELECT count(*) FROM property").await, 0);
}

#[tokio::test]
async fn a_tenant_joins_from_inside_the_app() {
    let w = World::new().await;
    let o = w.owner(OWNER_A).await;
    let pid = w.create_property(&o, "Кв").await;
    let (_, inv) = w
        .call(
            Method::POST,
            "/api/invitations",
            Some(&o),
            Some(pid),
            Some(json!({})),
        )
        .await;
    let token = inv["url"]
        .as_str()
        .unwrap()
        .split("start=")
        .nth(1)
        .unwrap()
        .to_string();

    let t = w.login(TENANT_A).await;
    let (_, me) = w.call(Method::GET, "/api/me", Some(&t), None, None).await;
    assert_eq!(me["role"], "new");
    // выдуманный токен и пустой — отказ, ничего не создаётся
    for bad in ["deadbeef", ""] {
        let (s, _) = w
            .call(
                Method::POST,
                "/api/invitations/accept",
                Some(&t),
                None,
                Some(json!({"token": bad})),
            )
            .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let (s, v) = w
        .call(
            Method::POST,
            "/api/invitations/accept",
            Some(&t),
            None,
            Some(json!({"token": token})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, me) = w.call(Method::GET, "/api/me", Some(&t), None, None).await;
    assert_eq!(
        (me["role"].as_str(), me["properties"][0]["role"].as_str()),
        (Some("tenant"), Some("tenant"))
    );
    // владелец получил уведомление; повторное нажатие не задваивает членство
    assert!(
        w.outbox
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.chat_id == OWNER_A && r.text.contains("подключился"))
    );
    let (s, _) = w
        .call(
            Method::POST,
            "/api/invitations/accept",
            Some(&t),
            None,
            Some(json!({"token": token})),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        count(&w, "SELECT count(*) FROM membership WHERE role = 'tenant'").await,
        1
    );
}

//! HTTP API Mini App: вход через подписанный `initData`, права ролей и полный сценарий
//! владелец ↔ арендатор через HTTP (настройка → приглашение → показания с фото → расчёт → тариф → утверждение → оплата).

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use chrono::{TimeZone, Utc};
use http_body_util::BodyExt;
use rentflow::api::{ApiState, Notifier, auth::sign_init_data, router};
use rentflow::app::{App, Config, Incoming, Reply, Sender};
use rentflow::db;
use rentflow::ocr::{Candidate, Extractor};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const LANDLORD: i64 = 1;
const TENANT: i64 = 2;
const STRANGER: i64 = 3;
const TOKEN: &str = "123456:TEST-BOT-TOKEN";

#[derive(Default)]
struct FakeOcr(Mutex<VecDeque<Result<Vec<Candidate>, String>>>);

#[async_trait]
impl Extractor for FakeOcr {
    async fn extract(&self, _image: &[u8]) -> Result<Vec<Candidate>, String> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err("нет ответа".into()))
    }
}

#[derive(Default)]
struct Outbox(Mutex<Vec<Reply>>);

#[async_trait]
impl Notifier for Outbox {
    async fn send(&self, replies: Vec<Reply>) {
        self.0.lock().unwrap().extend(replies);
    }
}

struct Api {
    router: Router,
    app: Arc<App>,
    ocr: Arc<FakeOcr>,
    outbox: Arc<Outbox>,
    now_unix: i64,
}

fn jpeg(seed: u8) -> Vec<u8> {
    vec![0xFF, 0xD8, 0xFF, 0xE0, seed, seed, seed]
}

impl Api {
    async fn new() -> Self {
        let pool = db::connect("sqlite::memory:").await.unwrap();
        let ocr = Arc::new(FakeOcr::default());
        let photos = std::env::temp_dir().join(format!("rentflow-api-{}", uuid::Uuid::new_v4()));
        let app = Arc::new(App::new(
            pool,
            Config {
                landlord_telegram_id: LANDLORD,
                bot_username: "rentflow_bot".into(),
                photo_dir: Some(photos),
                invite_ttl_days: 7,
                ocr_enabled: true,
            },
            ocr.clone(),
        ));
        let outbox = Arc::new(Outbox::default());
        // 16 октября 2026, 12:00 по Москве (UTC+3)
        let clock = Utc.with_ymd_and_hms(2026, 10, 16, 9, 0, 0).unwrap();
        let mut state = ApiState::new(app.clone(), outbox.clone(), TOKEN.into(), 3);
        state.clock = Arc::new(move || clock);
        Self {
            router: router(Arc::new(state), None),
            app,
            ocr,
            outbox,
            now_unix: clock.timestamp(),
        }
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        self.finish(req).await
    }

    async fn upload(&self, token: &str, code: &str, bytes: Vec<u8>) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(Method::POST)
            .uri(format!("/api/readings/photo?code={code}"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "image/jpeg")
            .body(Body::from(bytes))
            .unwrap();
        self.finish(req).await
    }

    async fn finish(&self, req: Request<Body>) -> (StatusCode, Value) {
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn init_data(&self, user_id: i64, auth_date: i64) -> String {
        let user = json!({ "id": user_id, "first_name": format!("U{user_id}") }).to_string();
        sign_init_data(
            &[("auth_date", &auth_date.to_string()), ("user", &user)],
            TOKEN,
        )
    }

    async fn login(&self, user_id: i64) -> String {
        let (s, v) = self
            .call(
                Method::POST,
                "/api/auth/telegram",
                None,
                Some(json!({ "init_data": self.init_data(user_id, self.now_unix) })),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["token"].as_str().unwrap().to_string()
    }

    async fn setup_property(&self, landlord: &str) {
        let (s, v) = self
            .call(
                Method::PUT,
                "/api/property",
                Some(landlord),
                Some(json!({
                    "name": "Квартира на Тверской", "tenant_name": "Иван",
                    "baselines": { "cold": "753", "hot": "416", "t1": "3831", "t2": "1118" },
                    "prices": { "cold": "65,77", "hot": "312,50", "t1": "8,60", "t2": "3,71" },
                    "sewer_price": "51,62",
                    "proof_required": true,
                })),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }

    /// Приглашение владельца принимает бот (как и раньше), затем арендатор входит в Mini App.
    async fn connect_tenant(&self, landlord: &str) -> String {
        let (s, v) = self
            .call(
                Method::POST,
                "/api/invitations",
                Some(landlord),
                Some(json!({})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let token = v["url"]
            .as_str()
            .unwrap()
            .split("start=")
            .nth(1)
            .unwrap()
            .to_string();
        let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 16)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let from = Sender {
            telegram_id: TENANT,
            chat_id: TENANT,
            name: "Иван".into(),
        };
        let r = self
            .app
            .handle(&from, Incoming::Start(Some(token)), now)
            .await;
        assert!(r[0].text.contains("Вы подключены"), "{r:?}");
        self.login(TENANT).await
    }
}

fn cand(register: &str, value: &str) -> Candidate {
    Candidate {
        register: Some(register.into()),
        value: Some(value.into()),
        confidence: "high".into(),
    }
}

// ---------- авторизация ----------

#[tokio::test]
async fn forged_expired_and_missing_credentials_get_401() {
    let api = Api::new().await;
    // без токена
    let (s, _) = api.call(Method::GET, "/api/me", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // выдуманный токен
    let (s, _) = api.call(Method::GET, "/api/me", Some("nope"), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // подпись чужим токеном бота
    let user = json!({ "id": LANDLORD, "first_name": "X" }).to_string();
    let forged = sign_init_data(
        &[("auth_date", &api.now_unix.to_string()), ("user", &user)],
        "999:OTHER",
    );
    let (s, _) = api
        .call(
            Method::POST,
            "/api/auth/telegram",
            None,
            Some(json!({ "init_data": forged })),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // подмена id в подписанных данных
    let good = api.init_data(TENANT, api.now_unix);
    let swapped = good.replace("%22id%22%3A2", "%22id%22%3A1");
    assert_ne!(good, swapped);
    let (s, _) = api
        .call(
            Method::POST,
            "/api/auth/telegram",
            None,
            Some(json!({ "init_data": swapped })),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // просроченный initData
    let (s, _) = api
        .call(
            Method::POST,
            "/api/auth/telegram",
            None,
            Some(json!({ "init_data": api.init_data(LANDLORD, api.now_unix - 2 * 24 * 3600) })),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // верная подпись, но человек не подключён ни к какому объекту
    let (s, _) = api
        .call(
            Method::POST,
            "/api/auth/telegram",
            None,
            Some(json!({ "init_data": api.init_data(STRANGER, api.now_unix) })),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // корректный вход работает
    let t = api.login(LANDLORD).await;
    let (s, v) = api.call(Method::GET, "/api/me", Some(&t), None).await;
    assert_eq!((s, v["role"].as_str()), (StatusCode::OK, Some("landlord")));
}

#[tokio::test]
async fn tenant_cannot_do_landlord_things() {
    let api = Api::new().await;
    let landlord = api.login(LANDLORD).await;
    api.setup_property(&landlord).await;
    let tenant = api.connect_tenant(&landlord).await;
    let (s, v) = api.call(Method::GET, "/api/me", Some(&tenant), None).await;
    assert_eq!((s, v["role"].as_str()), (StatusCode::OK, Some("tenant")));

    for (m, p, b) in [
        (
            Method::PUT,
            "/api/tariffs",
            Some(json!({ "changes": [{ "code": "t1", "price": "1" }] })),
        ),
        (Method::GET, "/api/tariffs", None),
        (Method::PUT, "/api/property", Some(json!({ "name": "Моя" }))),
        (Method::POST, "/api/invitations", Some(json!({}))),
        (Method::POST, "/api/bills/1/approve", Some(json!({}))),
        (Method::POST, "/api/bills/1/reject", Some(json!({}))),
        (Method::POST, "/api/bills/1/paid", Some(json!({}))),
        (Method::POST, "/api/bills/1/reissue", Some(json!({}))),
    ] {
        let (s, v) = api.call(m.clone(), p, Some(&tenant), b).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p}: {v}");
    }
    // а владелец не отправляет показания за арендатора
    let (s, _) = api
        .call(
            Method::POST,
            "/api/readings/submit",
            Some(&landlord),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // тарифы остались прежними
    let (_, v) = api
        .call(Method::GET, "/api/tariffs", Some(&landlord), None)
        .await;
    let t1 = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["code"] == "t1")
        .unwrap();
    assert_eq!(t1["effective"]["price"], "8.60");
}

#[tokio::test]
async fn property_is_created_once_without_any_bill() {
    let api = Api::new().await;
    let landlord = api.login(LANDLORD).await;
    let (_, v) = api
        .call(Method::GET, "/api/property", Some(&landlord), None)
        .await;
    assert!(v["property"].is_null());
    // неполные данные не создают объект
    let (s, _) = api
        .call(
            Method::PUT,
            "/api/property",
            Some(&landlord),
            Some(json!({ "name": "A", "tenant_name": "B" })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    api.setup_property(&landlord).await;
    let (_, v) = api
        .call(Method::GET, "/api/property", Some(&landlord), None)
        .await;
    assert_eq!(v["property"]["registers"].as_array().unwrap().len(), 4);
    assert_eq!(v["property"]["registers"][2]["baseline"], "3831");
    let bills: i64 = sqlx::query_scalar("SELECT count(*) FROM bill")
        .fetch_one(api.app.pool())
        .await
        .unwrap();
    assert_eq!(bills, 0, "стартовые показания не создают начислений");
    // повторная «настройка» только переименовывает, а не создаёт второй объект
    let (s, v) = api
        .call(
            Method::PUT,
            "/api/property",
            Some(&landlord),
            Some(json!({ "name": "Новое имя" })),
        )
        .await;
    assert_eq!(
        (s, v["property"]["name"].as_str()),
        (StatusCode::OK, Some("Новое имя"))
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM property")
        .fetch_one(api.app.pool())
        .await
        .unwrap();
    assert_eq!(n, 1);
}

// ---------- полный сценарий ----------

#[tokio::test]
async fn full_month_over_http() {
    let api = Api::new().await;
    let landlord = api.login(LANDLORD).await;
    api.setup_property(&landlord).await;
    let tenant = api.connect_tenant(&landlord).await;

    // форма: четыре пустых регистра с прошлыми показаниями
    let (s, v) = api
        .call(
            Method::POST,
            "/api/readings/draft",
            Some(&tenant),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["period"], "2026-10");
    assert_eq!(v["registers"][2]["previous"], "3831");
    assert_eq!(v["complete"], false);

    // меньше прошлого — нельзя
    let (s, v) = api
        .call(
            Method::PUT,
            "/api/readings/draft",
            Some(&tenant),
            Some(json!({ "code": "t1", "value": "3000" })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");

    // фото 1: ИИ ошибается (читает 999 вместо 755) — человек исправляет
    api.ocr
        .0
        .lock()
        .unwrap()
        .push_back(Ok(vec![cand("cold", "999")]));
    let (s, v) = api.upload(&tenant, "cold", jpeg(1)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["registers"][0]["value"], "999");
    assert_eq!(v["registers"][0]["source"], "photo");
    let (_, v) = api
        .call(
            Method::PUT,
            "/api/readings/draft",
            Some(&tenant),
            Some(json!({ "code": "cold", "value": "755" })),
        )
        .await;
    assert_eq!(v["registers"][0]["value"], "755");
    assert_eq!(
        v["registers"][0]["has_photo"], true,
        "правка числа не теряет фото"
    );
    assert_eq!(
        v["registers"][0]["ai_value"], "999",
        "исходное предложение ИИ сохраняется"
    );

    // то же фото второй раз — отказ
    let (s, _) = api.upload(&tenant, "hot", jpeg(1)).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    // не JPEG — отказ
    let (s, _) = api.upload(&tenant, "hot", b"GIF89a".to_vec()).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // ИИ недоступен — ручной ввод продолжает работать, фото остаётся подтверждением
    for (code, value, seed) in [("hot", "417", 2u8), ("t1", "3975,48", 3), ("t2", "1179", 4)] {
        let (s, v) = api.upload(&tenant, code, jpeg(seed)).await; // очередь OCR пуста → «сбой»
        assert_eq!(s, StatusCode::OK, "{v}");
        let (s, _) = api
            .call(
                Method::PUT,
                "/api/readings/draft",
                Some(&tenant),
                Some(json!({ "code": code, "value": value })),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
    }

    // неполный комплект: уберём одну фотографию, отправка запрещена и ничего не создаёт
    let (_, v) = api
        .call(Method::GET, "/api/readings/draft", Some(&tenant), None)
        .await;
    assert_eq!(v["complete"], true);
    sqlx::query("UPDATE pending_reading SET file_id = NULL, unique_id = NULL, photo_path = NULL WHERE register_code = 't2'")
        .execute(api.app.pool())
        .await
        .unwrap();
    let (s, _) = api
        .call(
            Method::POST,
            "/api/readings/submit",
            Some(&tenant),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let bills: i64 = sqlx::query_scalar("SELECT count(*) FROM bill")
        .fetch_one(api.app.pool())
        .await
        .unwrap();
    assert_eq!(bills, 0);
    let (s, _) = api.upload(&tenant, "t2", jpeg(4)).await;
    assert_eq!(s, StatusCode::OK);

    // отправка: владельцу уходит уведомление с расчётом, арендатор суммы не видит
    let (s, v) = api
        .call(
            Method::POST,
            "/api/readings/submit",
            Some(&tenant),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        (s, v["already_submitted"].clone()),
        (StatusCode::OK, json!(false)),
        "{v}"
    );
    {
        let out = api.outbox.0.lock().unwrap();
        assert!(
            out.iter().any(|r| r.text.contains("Итого: 2063,61 ₽")),
            "{out:?}"
        );
        assert!(
            out.iter()
                .all(|r| r.chat_id != TENANT || !r.text.contains("Итого")),
            "{out:?}"
        );
        assert_eq!(out.iter().filter(|r| !r.photos.is_empty()).count(), 1);
        assert!(
            out.iter()
                .any(|r| r.photos.iter().any(|p| p.0.starts_with("local:")))
        );
    }
    // «Квартира» показывает последнее принятое показание, а не только начальное
    let (_, v) = api
        .call(Method::GET, "/api/property", Some(&tenant), None)
        .await;
    let cold = &v["property"]["registers"][0];
    assert_eq!(
        (cold["baseline"].as_str(), cold["last"]["value"].as_str()),
        (Some("753"), Some("755"))
    );
    // повторная отправка не создаёт дублей
    let (s, v) = api
        .call(
            Method::POST,
            "/api/readings/submit",
            Some(&tenant),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        (s, v["already_submitted"].clone()),
        (StatusCode::OK, json!(true))
    );

    // черновик виден владельцу, арендатору — нет
    let (_, v) = api
        .call(Method::GET, "/api/bills", Some(&landlord), None)
        .await;
    let bills = v["bills"].as_array().unwrap();
    assert_eq!(bills.len(), 1);
    assert_eq!(
        (bills[0]["status"].as_str(), bills[0]["total"].as_str()),
        (Some("draft"), Some("2063.61"))
    );
    let draft_id = bills[0]["id"].as_i64().unwrap();
    let (_, v) = api
        .call(Method::GET, "/api/bills", Some(&tenant), None)
        .await;
    assert!(v["bills"].as_array().unwrap().is_empty());
    let (s, _) = api
        .call(
            Method::GET,
            &format!("/api/bills/{draft_id}"),
            Some(&tenant),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = api
        .call(
            Method::POST,
            &format!("/api/bills/{draft_id}/approve"),
            Some(&tenant),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // детали счёта: формула, даты и фото
    let (_, d) = api
        .call(
            Method::GET,
            &format!("/api/bills/{draft_id}"),
            Some(&landlord),
            None,
        )
        .await;
    assert_eq!(d["snapshot"]["lines"].as_array().unwrap().len(), 5);
    assert_eq!(d["can_approve"], true);
    let rid = d["readings"][0]["reading_id"].as_i64().unwrap();
    let (s, _) = api
        .call(
            Method::GET,
            &format!("/api/photos/{rid}"),
            Some(&landlord),
            None,
        )
        .await;
    assert_ne!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = api
        .call(Method::GET, &format!("/api/photos/{rid}"), None, None)
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // владелец меняет T1 за текущий месяц: черновик пересчитан, прежний заменён
    let (s, v) = api
        .call(
            Method::PUT,
            "/api/tariffs",
            Some(&landlord),
            Some(json!({ "changes": [{ "code": "t1", "price": "9" }] })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let new_draft = v["draft_id"].as_i64().unwrap();
    assert_ne!(new_draft, draft_id);
    let (_, d) = api
        .call(
            Method::GET,
            &format!("/api/bills/{new_draft}"),
            Some(&landlord),
            None,
        )
        .await;
    assert_eq!(d["total"], "2121.21");
    let (s, v) = api
        .call(
            Method::POST,
            &format!("/api/bills/{draft_id}/approve"),
            Some(&landlord),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "устаревший черновик нельзя утвердить: {v}"
    );
    // тот же тариф ещё раз — менять нечего
    let (s, _) = api
        .call(
            Method::PUT,
            "/api/tariffs",
            Some(&landlord),
            Some(json!({ "changes": [{ "code": "t1", "price": "9" }] })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // утверждение: арендатор получает уведомление и видит счёт
    api.outbox.0.lock().unwrap().clear();
    let (s, v) = api
        .call(
            Method::POST,
            &format!("/api/bills/{new_draft}/approve"),
            Some(&landlord),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        (s, v["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{v}"
    );
    assert!(
        api.outbox
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.chat_id == TENANT && r.text.contains("Итого: 2121,21 ₽"))
    );
    let (s, _) = api
        .call(
            Method::POST,
            &format!("/api/bills/{new_draft}/approve"),
            Some(&landlord),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "двойное утверждение");
    let (_, v) = api
        .call(Method::GET, "/api/bills", Some(&tenant), None)
        .await;
    assert_eq!(v["bills"][0]["total"], "2121.21");

    // утверждённый счёт неизменяем: новый тариф его не трогает
    let before: String = sqlx::query_scalar("SELECT snapshot FROM bill WHERE id = ?")
        .bind(new_draft)
        .fetch_one(api.app.pool())
        .await
        .unwrap();
    api.call(
        Method::PUT,
        "/api/tariffs",
        Some(&landlord),
        Some(json!({ "changes": [{ "code": "t1", "price": "10" }] })),
    )
    .await;
    let after: String = sqlx::query_scalar("SELECT snapshot FROM bill WHERE id = ?")
        .bind(new_draft)
        .fetch_one(api.app.pool())
        .await
        .unwrap();
    assert_eq!(before, after);

    // оплата
    let (s, v) = api
        .call(
            Method::POST,
            &format!("/api/bills/{new_draft}/paid"),
            Some(&landlord),
            Some(json!({})),
        )
        .await;
    assert_eq!((s, v["status"].as_str()), (StatusCode::OK, Some("paid")));
    let (_, h) = api
        .call(Method::GET, "/api/home", Some(&tenant), None)
        .await;
    assert_eq!(h["last_bill"]["status"], "paid");

    // журнал фиксирует изменения
    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM audit_log ORDER BY id")
        .fetch_all(api.app.pool())
        .await
        .unwrap();
    for a in [
        "property_create",
        "invitation_create",
        "readings_submit",
        "tariffs_update",
        "bill_approve",
        "bill_paid",
    ] {
        assert!(
            actions.iter().any(|x| x == a),
            "нет записи {a}: {actions:?}"
        );
    }
}

#[tokio::test]
async fn golden_totals_come_out_of_the_api_too() {
    let api = Api::new().await;
    let landlord = api.login(LANDLORD).await;
    api.setup_property(&landlord).await;
    sqlx::query("UPDATE property SET proof_required = 0")
        .execute(api.app.pool())
        .await
        .unwrap();
    let tenant = api.connect_tenant(&landlord).await;
    for (code, value) in [
        ("cold", "755"),
        ("hot", "417"),
        ("t1", "3975,48"),
        ("t2", "1179"),
    ] {
        let (s, v) = api
            .call(
                Method::PUT,
                "/api/readings/draft",
                Some(&tenant),
                Some(json!({ "code": code, "value": value })),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let (s, _) = api
        .call(
            Method::POST,
            "/api/readings/submit",
            Some(&tenant),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = api
        .call(Method::GET, "/api/bills", Some(&landlord), None)
        .await;
    assert_eq!(v["bills"][0]["total"], "2063.61");
}

#[tokio::test]
async fn session_dies_when_access_is_revoked() {
    let api = Api::new().await;
    let landlord = api.login(LANDLORD).await;
    api.setup_property(&landlord).await;
    let tenant = api.connect_tenant(&landlord).await;
    sqlx::query("DELETE FROM api_session WHERE telegram_id = ?")
        .bind(TENANT)
        .execute(api.app.pool())
        .await
        .unwrap();
    let (s, _) = api.call(Method::GET, "/api/me", Some(&tenant), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn static_frontend_and_health_are_served_without_auth() {
    let dir = std::env::temp_dir().join(format!("rentflow-web-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("index.html"),
        "<!doctype html><title>RentFlow</title>",
    )
    .unwrap();
    let api = Api::new().await;
    let state = Arc::new(ApiState::new(
        api.app.clone(),
        api.outbox.clone(),
        TOKEN.into(),
        3,
    ));
    let app = router(state, Some(dir));
    for path in ["/", "/healthz", "/some/client/route"] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
    }
    // API по-прежнему закрыт
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn schedule_and_void_work_through_the_api() {
    let api = Api::new().await;
    let landlord = api.login(LANDLORD).await;
    api.setup_property(&landlord).await;
    let tenant = api.connect_tenant(&landlord).await;
    sqlx::query("UPDATE property SET proof_required = 0")
        .execute(api.app.pool())
        .await
        .unwrap();

    // расписание: 1–28, 0 снимает; арендатор не меняет
    let (s, v) = api
        .call(
            Method::PUT,
            "/api/property",
            Some(&landlord),
            Some(json!({ "reading_day": 25 })),
        )
        .await;
    assert_eq!(
        (s, v["property"]["reading_day"].clone()),
        (StatusCode::OK, json!(25)),
        "{v}"
    );
    let (s, _) = api
        .call(
            Method::PUT,
            "/api/property",
            Some(&landlord),
            Some(json!({ "reading_day": 31 })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = api
        .call(
            Method::PUT,
            "/api/property",
            Some(&tenant),
            Some(json!({ "reading_day": 5 })),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, h) = api
        .call(Method::GET, "/api/home", Some(&tenant), None)
        .await;
    assert_eq!(h["due_date"], "2026-10-25");

    // счёт утверждён и оплачен
    for (code, value) in [
        ("cold", "755"),
        ("hot", "417"),
        ("t1", "3975,48"),
        ("t2", "1179"),
    ] {
        api.call(
            Method::PUT,
            "/api/readings/draft",
            Some(&tenant),
            Some(json!({ "code": code, "value": value })),
        )
        .await;
    }
    api.call(
        Method::POST,
        "/api/readings/submit",
        Some(&tenant),
        Some(json!({})),
    )
    .await;
    let (_, v) = api
        .call(Method::GET, "/api/bills", Some(&landlord), None)
        .await;
    let id = v["bills"][0]["id"].as_i64().unwrap();
    api.call(
        Method::POST,
        &format!("/api/bills/{id}/approve"),
        Some(&landlord),
        Some(json!({})),
    )
    .await;
    api.call(
        Method::POST,
        &format!("/api/bills/{id}/paid"),
        Some(&landlord),
        Some(json!({})),
    )
    .await;
    let (_, h) = api
        .call(Method::GET, "/api/home", Some(&tenant), None)
        .await;
    assert_eq!(
        h["due_date"], "2026-11-25",
        "после отправки срок — следующий месяц"
    );

    // арендатор не аннулирует; без причины и без явного подтверждения оплаченного — нет
    let url = format!("/api/bills/{id}/void");
    let (s, _) = api
        .call(
            Method::POST,
            &url,
            Some(&tenant),
            Some(json!({ "reason": "x", "confirm_paid": true })),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = api
        .call(
            Method::POST,
            &url,
            Some(&landlord),
            Some(json!({ "reason": "  " })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = api
        .call(
            Method::POST,
            &url,
            Some(&landlord),
            Some(json!({ "reason": "тест" })),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "оплаченный без confirm_paid"
    );
    let (_, d) = api
        .call(
            Method::GET,
            &format!("/api/bills/{id}"),
            Some(&landlord),
            None,
        )
        .await;
    assert_eq!(
        (d["status"].as_str(), d["can_void"].clone()),
        (Some("paid"), json!(true))
    );

    // с подтверждением — аннулирован, арендатор уведомлён, показания открыты; повтор — отказ
    api.outbox.0.lock().unwrap().clear();
    let (s, v) = api
        .call(
            Method::POST,
            &url,
            Some(&landlord),
            Some(json!({ "reason": "тест", "confirm_paid": true })),
        )
        .await;
    assert_eq!(
        (s, v["status"].as_str()),
        (StatusCode::OK, Some("void")),
        "{v}"
    );
    assert!(
        api.outbox
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.chat_id == TENANT && r.text.contains("аннулирован"))
    );
    let (s, _) = api
        .call(
            Method::POST,
            &url,
            Some(&landlord),
            Some(json!({ "reason": "тест", "confirm_paid": true })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, f) = api
        .call(Method::GET, "/api/readings/draft", Some(&tenant), None)
        .await;
    assert_eq!(f["billed"], false);
    let (_, v) = api
        .call(Method::GET, "/api/tariffs", Some(&landlord), None)
        .await;
    assert_eq!(v["items"].as_array().unwrap().len(), 5, "тарифы на месте");
}

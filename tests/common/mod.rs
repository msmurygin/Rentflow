#![allow(dead_code, unused_imports)]
// Все проверки идут через публичные каналы (бот и HTTP API), а не через внутренние функции.

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use http_body_util::BodyExt;
use rentflow::api::{ApiState, Notifier, auth::sign_init_data, router};
use rentflow::app::{App, Config, Incoming, Reply, Sender};
use rentflow::ocr::NoExtractor;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

pub const TOKEN: &str = "1:ISO";
pub const OWNER_A: i64 = 11;
pub const OWNER_B: i64 = 22;
pub const TENANT_A: i64 = 33;
pub const TENANT_B: i64 = 44;

#[derive(Default)]
pub struct Outbox(pub Mutex<Vec<Reply>>);
#[async_trait]
impl Notifier for Outbox {
    async fn send(&self, r: Vec<Reply>) {
        self.0.lock().unwrap().extend(r);
    }
}

pub fn now() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 10, 16)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
}

pub struct World {
    pub router: Router,
    pub app: Arc<App>,
    pub outbox: Arc<Outbox>,
    pub unix: i64,
}

pub fn sender(id: i64) -> Sender {
    Sender {
        telegram_id: id,
        chat_id: id,
        name: format!("u{id}"),
    }
}

impl World {
    pub async fn new() -> Self {
        let pool = rentflow::db::connect("sqlite::memory:").await.unwrap();
        let app = Arc::new(App::new(
            pool,
            Config {
                bot_username: "b".into(),
                invite_ttl_days: 7,
                ..Default::default()
            },
            Arc::new(NoExtractor),
        ));
        let outbox = Arc::new(Outbox::default());
        let clock = Utc.with_ymd_and_hms(2026, 10, 16, 9, 0, 0).unwrap();
        let mut st = ApiState::new(app.clone(), outbox.clone(), TOKEN.into(), 3);
        st.clock = Arc::new(move || clock);
        Self {
            router: router(Arc::new(st), None),
            app,
            outbox,
            unix: clock.timestamp(),
        }
    }

    pub async fn call(
        &self,
        m: Method,
        path: &str,
        tok: Option<&str>,
        prop: Option<i64>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder().method(m).uri(path);
        if let Some(t) = tok {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        if let Some(p) = prop {
            req = req.header("x-property-id", p.to_string());
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    pub async fn login(&self, id: i64) -> String {
        let user = json!({ "id": id, "first_name": format!("U{id}") }).to_string();
        let init = sign_init_data(
            &[("auth_date", &self.unix.to_string()), ("user", &user)],
            TOKEN,
        );
        let (s, v) = self
            .call(
                Method::POST,
                "/api/auth/telegram",
                None,
                None,
                Some(json!({ "init_data": init })),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["token"].as_str().unwrap().to_string()
    }

    pub async fn owner(&self, id: i64) -> String {
        let t = self.login(id).await;
        let (s, _) = self
            .call(
                Method::POST,
                "/api/register",
                Some(&t),
                None,
                Some(json!({})),
            )
            .await;
        assert_eq!(s, StatusCode::OK);
        t
    }

    pub async fn create_property(&self, tok: &str, name: &str) -> i64 {
        let (s, v) = self
            .call(
                Method::POST,
                "/api/properties",
                Some(tok),
                None,
                Some(json!({
                    "name": name, "tenant_name": "Жилец",
                    "baselines": {"cold":"1","hot":"1","t1":"1","t2":"1"},
                    "prices": {"cold":"10","hot":"20","t1":"5","t2":"3"},
                    "sewer_price": "7", "proof_required": false
                })),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["property"]["id"].as_i64().unwrap()
    }

    /// Приглашает арендатора в объект `prop` и принимает приглашение так, как это делает бот.
    pub async fn join_tenant(&self, owner_tok: &str, prop: i64, tenant: i64) -> String {
        let (s, v) = self
            .call(
                Method::POST,
                "/api/invitations",
                Some(owner_tok),
                Some(prop),
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
        let r = self
            .app
            .handle(&sender(tenant), Incoming::Start(Some(token)), now())
            .await;
        assert!(r[0].text.contains("Вы подключены"), "{r:?}");
        self.login(tenant).await
    }

    /// Месяц: арендатор вводит показания и отправляет; владелец утверждает. Возвращает id счёта.
    pub async fn bill(
        &self,
        owner_tok: &str,
        tenant_tok: &str,
        prop: i64,
        values: [&str; 4],
    ) -> i64 {
        for (code, v) in ["cold", "hot", "t1", "t2"].iter().zip(values) {
            let (s, r) = self
                .call(
                    Method::PUT,
                    "/api/readings/draft",
                    Some(tenant_tok),
                    Some(prop),
                    Some(json!({"code": code, "value": v})),
                )
                .await;
            assert_eq!(s, StatusCode::OK, "{r}");
        }
        let (s, r) = self
            .call(
                Method::POST,
                "/api/readings/submit",
                Some(tenant_tok),
                Some(prop),
                Some(json!({})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{r}");
        let (_, v) = self
            .call(Method::GET, "/api/bills", Some(owner_tok), Some(prop), None)
            .await;
        v["bills"][0]["id"].as_i64().unwrap()
    }
}

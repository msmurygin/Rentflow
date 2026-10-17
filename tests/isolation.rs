//! Изоляция данных: два независимых владельца, несколько квартир, смешанные роли, приглашения.
//! Все проверки идут через публичные каналы (бот и HTTP API), а не через внутренние функции.

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

const TOKEN: &str = "1:ISO";
const OWNER_A: i64 = 11;
const OWNER_B: i64 = 22;
const TENANT_A: i64 = 33;
const TENANT_B: i64 = 44;

#[derive(Default)]
struct Outbox(Mutex<Vec<Reply>>);
#[async_trait]
impl Notifier for Outbox {
    async fn send(&self, r: Vec<Reply>) {
        self.0.lock().unwrap().extend(r);
    }
}

fn now() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 10, 16)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
}

struct World {
    router: Router,
    app: Arc<App>,
    outbox: Arc<Outbox>,
    unix: i64,
}

fn sender(id: i64) -> Sender {
    Sender {
        telegram_id: id,
        chat_id: id,
        name: format!("u{id}"),
    }
}

impl World {
    async fn new() -> Self {
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

    async fn call(
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

    async fn login(&self, id: i64) -> String {
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

    async fn owner(&self, id: i64) -> String {
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

    async fn create_property(&self, tok: &str, name: &str) -> i64 {
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
    async fn join_tenant(&self, owner_tok: &str, prop: i64, tenant: i64) -> String {
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
    async fn bill(&self, owner_tok: &str, tenant_tok: &str, prop: i64, values: [&str; 4]) -> i64 {
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

#[tokio::test]
async fn two_owners_cannot_read_or_change_each_others_data() {
    let w = World::new().await;
    let (a, b) = (w.owner(OWNER_A).await, w.owner(OWNER_B).await);
    let (pa, pb) = (
        w.create_property(&a, "Квартира A").await,
        w.create_property(&b, "Квартира B").await,
    );
    assert_ne!(pa, pb);
    let ta = w.join_tenant(&a, pa, TENANT_A).await;
    let tb = w.join_tenant(&b, pb, TENANT_B).await;
    let bill_b = w.bill(&b, &tb, pb, ["3", "3", "9", "5"]).await;

    // B пытается явно выбрать объект A через заголовок: 404, неотличимо от несуществующего
    for (m, path) in [
        (Method::GET, "/api/property"),
        (Method::GET, "/api/home"),
        (Method::GET, "/api/bills"),
        (Method::GET, "/api/tariffs"),
        (Method::GET, "/api/meters"),
    ] {
        let (s, _) = w.call(m.clone(), path, Some(&b), Some(pa), None).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{m} {path} с чужим объектом");
        let (s2, v2) = w.call(m, path, Some(&b), Some(99_999), None).await;
        assert_eq!(s2, StatusCode::NOT_FOUND);
        assert!(v2["error"].is_string());
    }
    // запись в чужой объект: тарифы, приглашение, настройки
    let (s, _) = w
        .call(
            Method::PUT,
            "/api/tariffs",
            Some(&b),
            Some(pa),
            Some(json!({"changes":[{"code":"t1","price":"99"}]})),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = w
        .call(
            Method::POST,
            "/api/invitations",
            Some(&b),
            Some(pa),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = w
        .call(
            Method::PUT,
            "/api/property",
            Some(&b),
            Some(pa),
            Some(json!({"name":"взломано"})),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let name: String = sqlx::query_scalar("SELECT name FROM property WHERE id = ?")
        .bind(pa)
        .fetch_one(w.app.pool())
        .await
        .unwrap();
    assert_eq!(name, "Квартира A");
    let t1: String =
        sqlx::query_scalar("SELECT price FROM tariff WHERE property_id = ? AND code = 't1'")
            .bind(pa)
            .fetch_one(w.app.pool())
            .await
            .unwrap();
    assert_eq!(t1, "5");

    // A не видит и не трогает счёт B ни под своим контекстом, ни под чужим; все ответы одинаковы
    for p in [None, Some(pa)] {
        for action in ["", "/approve", "/reject", "/paid", "/reissue", "/void"] {
            let (m, body) = if action.is_empty() {
                (Method::GET, None)
            } else {
                (
                    Method::POST,
                    Some(json!({"reason": "x", "confirm_paid": true})),
                )
            };
            let (s, _) = w
                .call(
                    m,
                    &format!("/api/bills/{bill_b}{action}"),
                    Some(&a),
                    p,
                    body,
                )
                .await;
            assert_eq!(s, StatusCode::NOT_FOUND, "A -> bill B {action} {p:?}");
        }
    }
    let status: String = sqlx::query_scalar("SELECT status FROM bill WHERE id = ?")
        .bind(bill_b)
        .fetch_one(w.app.pool())
        .await
        .unwrap();
    assert_eq!(status, "draft");

    // арендатор A не видит данные B и не отправляет за него
    let (s, _) = w
        .call(
            Method::GET,
            "/api/readings/draft",
            Some(&ta),
            Some(pb),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = w
        .call(
            Method::PUT,
            "/api/readings/draft",
            Some(&ta),
            Some(pb),
            Some(json!({"code":"cold","value":"5"})),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // свои данные у каждого на месте
    let (_, v) = w
        .call(Method::GET, "/api/bills", Some(&a), Some(pa), None)
        .await;
    assert!(v["bills"].as_array().unwrap().is_empty());
    let (_, v) = w
        .call(Method::GET, "/api/property", Some(&b), None, None)
        .await;
    assert_eq!(v["property"]["name"], "Квартира B");
}

async fn photo_status(w: &World, url: String, tok: String, p: Option<i64>) -> StatusCode {
    let mut r = Request::builder()
        .uri(url)
        .header("authorization", format!("Bearer {tok}"));
    if let Some(p) = p {
        r = r.header("x-property-id", p.to_string());
    }
    w.router
        .clone()
        .oneshot(r.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn photos_are_served_only_to_members_of_that_property() {
    let w = World::new().await;
    let (a, b) = (w.owner(OWNER_A).await, w.owner(OWNER_B).await);
    let (pa, pb) = (
        w.create_property(&a, "A").await,
        w.create_property(&b, "B").await,
    );
    let tb = w.join_tenant(&b, pb, TENANT_B).await;
    w.bill(&b, &tb, pb, ["3", "3", "9", "5"]).await;
    let dir = std::env::temp_dir().join(format!("iso-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("p.jpg");
    std::fs::write(&file, [0xFF, 0xD8, 0xFF, 9]).unwrap();
    sqlx::query("UPDATE reading SET photo_path = ? WHERE id = (SELECT max(id) FROM reading WHERE kind = 'submitted')").bind(file.to_string_lossy().to_string()).execute(w.app.pool()).await.unwrap();
    let rid: i64 = sqlx::query_scalar("SELECT max(id) FROM reading WHERE kind = 'submitted'")
        .fetch_one(w.app.pool())
        .await
        .unwrap();
    let url = format!("/api/photos/{rid}");
    let get = |tok: &str, p: Option<i64>| photo_status(&w, url.clone(), tok.to_string(), p);
    assert_eq!(
        get(&b, None).await,
        StatusCode::OK,
        "владелец своего объекта"
    );
    assert_eq!(
        get(&tb, None).await,
        StatusCode::OK,
        "арендатор своего объекта"
    );
    assert_eq!(
        get(&a, None).await,
        StatusCode::NOT_FOUND,
        "владелец другого объекта"
    );
    assert_eq!(get(&a, Some(pa)).await, StatusCode::NOT_FOUND);
    let _ = pb;
}

#[tokio::test]
async fn one_owner_with_two_properties_switches_context_safely() {
    let w = World::new().await;
    let a = w.owner(OWNER_A).await;
    let (p1, p2) = (
        w.create_property(&a, "Первая").await,
        w.create_property(&a, "Вторая").await,
    );
    let (_, v) = w
        .call(Method::GET, "/api/properties", Some(&a), None, None)
        .await;
    assert_eq!(v["properties"].as_array().unwrap().len(), 2);
    // тариф меняется ровно в выбранном объекте
    let (s, _) = w
        .call(
            Method::PUT,
            "/api/tariffs",
            Some(&a),
            Some(p1),
            Some(json!({"changes":[{"code":"t1","price":"50"}]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let price = |p: i64| {
        let pool = w.app.pool().clone();
        async move {
            sqlx::query_scalar::<_, String>(
                "SELECT price FROM tariff WHERE property_id = ? AND code = 't1'",
            )
            .bind(p)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(price(p1).await, "50");
    assert_eq!(price(p2).await, "5", "вторая квартира не изменилась");
    // у каждой квартиры свой набор счетчиков и свои приглашения
    let t1 = w.join_tenant(&a, p1, TENANT_A).await;
    let (_, v) = w
        .call(Method::GET, "/api/property", Some(&a), Some(p2), None)
        .await;
    assert_eq!(v["property"]["tenant_connected"], false);
    let (_, v) = w
        .call(Method::GET, "/api/property", Some(&a), Some(p1), None)
        .await;
    assert_eq!(v["property"]["tenant_connected"], true);
    let _ = t1;
}

#[tokio::test]
async fn mixed_roles_owner_in_one_property_tenant_in_another() {
    let w = World::new().await;
    let (a, b) = (w.owner(OWNER_A).await, w.owner(OWNER_B).await);
    let (pa, pb) = (
        w.create_property(&a, "Квартира A").await,
        w.create_property(&b, "Квартира B").await,
    );
    // владелец A снимает квартиру B: принимает приглашение B как арендатор
    let (_, inv) = w
        .call(
            Method::POST,
            "/api/invitations",
            Some(&b),
            Some(pb),
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
    let r = w
        .app
        .handle(&sender(OWNER_A), Incoming::Start(Some(token)), now())
        .await;
    assert!(r[0].text.contains("Вы подключены"), "{r:?}");
    let a = w.login(OWNER_A).await;
    let (_, v) = w.call(Method::GET, "/api/me", Some(&a), None, None).await;
    let roles: Vec<(i64, String)> = v["properties"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["id"].as_i64().unwrap(),
                p["role"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        roles.contains(&(pa, "owner".into())) && roles.contains(&(pb, "tenant".into())),
        "{roles:?}"
    );
    // в своей квартире — владелец: тарифы доступны
    let (s, _) = w
        .call(Method::GET, "/api/tariffs", Some(&a), Some(pa), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    // в чужой — арендатор: тарифы и права владельца недоступны (403), форма показаний доступна
    let (s, _) = w
        .call(Method::GET, "/api/tariffs", Some(&a), Some(pb), None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = w
        .call(
            Method::POST,
            "/api/invitations",
            Some(&a),
            Some(pb),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = w
        .call(Method::GET, "/api/readings/draft", Some(&a), Some(pb), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["registers"].as_array().unwrap().len(), 4);
    // и обратное: форма показаний недоступна в собственной квартире (там он владелец)
    let (s, _) = w
        .call(Method::GET, "/api/readings/draft", Some(&a), Some(pa), None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn invites_are_single_use_expire_and_bind_to_their_property() {
    let w = World::new().await;
    let (a, b) = (w.owner(OWNER_A).await, w.owner(OWNER_B).await);
    let (pa, pb) = (
        w.create_property(&a, "A").await,
        w.create_property(&b, "B").await,
    );
    let (_, inv) = w
        .call(
            Method::POST,
            "/api/invitations",
            Some(&a),
            Some(pa),
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

    // владелец B не может присоединиться по приглашению A ... он может, но только как арендатор, и это не даёт ему прав владельца
    let first = w
        .app
        .handle(
            &sender(TENANT_A),
            Incoming::Start(Some(token.clone())),
            now(),
        )
        .await;
    assert!(first[0].text.contains("Вы подключены"), "{first:?}");
    // повтор тем же человеком безопасен, ничего не дублирует
    let again = w
        .app
        .handle(
            &sender(TENANT_A),
            Incoming::Start(Some(token.clone())),
            now(),
        )
        .await;
    assert!(again[0].text.contains("уже подключены"), "{again:?}");
    // другой человек той же ссылкой — отказ (одноразовость)
    let other = w
        .app
        .handle(
            &sender(TENANT_B),
            Incoming::Start(Some(token.clone())),
            now(),
        )
        .await;
    assert!(other[0].text.contains("недействительна"), "{other:?}");
    let members: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM membership WHERE property_id = ? AND role = 'tenant'",
    )
    .bind(pa)
    .fetch_one(w.app.pool())
    .await
    .unwrap();
    assert_eq!(members, 1);
    // выдуманный токен
    let fake = w
        .app
        .handle(
            &sender(TENANT_B),
            Incoming::Start(Some("deadbeef".into())),
            now(),
        )
        .await;
    assert!(fake[0].text.contains("недействительна"), "{fake:?}");
    // истёкшее приглашение
    sqlx::query("UPDATE invite SET used_at = NULL, used_by = NULL, expires_at = '2000-01-01T00:00:00' WHERE token = ?").bind(&token).execute(w.app.pool()).await.unwrap();
    sqlx::query("UPDATE tenancy SET tenant_user_id = NULL WHERE property_id = ?")
        .bind(pa)
        .execute(w.app.pool())
        .await
        .unwrap();
    let late = w
        .app
        .handle(&sender(TENANT_B), Incoming::Start(Some(token)), now())
        .await;
    assert!(
        late[0].text.contains("истекла") || late[0].text.contains("недействительна"),
        "{late:?}"
    );
    // владелец не может принять приглашение в собственный объект
    let (_, inv) = w
        .call(
            Method::POST,
            "/api/invitations",
            Some(&b),
            Some(pb),
            Some(json!({})),
        )
        .await;
    let t = inv["url"]
        .as_str()
        .unwrap()
        .split("start=")
        .nth(1)
        .unwrap()
        .to_string();
    let own = w
        .app
        .handle(&sender(OWNER_B), Incoming::Start(Some(t)), now())
        .await;
    assert!(own[0].text.contains("владелец этого объекта"), "{own:?}");
    let _ = &w.outbox;
}

#[tokio::test]
async fn bot_commands_work_on_the_active_property_of_the_caller_only() {
    let w = World::new().await;
    let (a, b) = (w.owner(OWNER_A).await, w.owner(OWNER_B).await);
    let (_pa, pb) = (
        w.create_property(&a, "Квартира A").await,
        w.create_property(&b, "Квартира B").await,
    );
    let t = w.join_tenant(&b, pb, TENANT_B).await;
    let approved = w.bill(&b, &t, pb, ["3", "3", "9", "5"]).await;
    let (s, _) = w
        .call(
            Method::POST,
            &format!("/api/bills/{approved}/approve"),
            Some(&b),
            Some(pb),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    // у владельца A в боте нет ни счетов, ни тарифов владельца B
    let r = w
        .app
        .handle(&sender(OWNER_A), Incoming::Text("/status".into()), now())
        .await;
    assert!(
        r[0].text.contains("Квартира A") && !r[0].text.contains("Квартира B"),
        "{r:?}"
    );
    for cmd in ["/reissue", "/void", "/paid"] {
        let r = w
            .app
            .handle(&sender(OWNER_A), Incoming::Text(cmd.into()), now())
            .await;
        assert!(
            r[0].buttons.is_empty(),
            "{cmd}: чужой утверждённый счёт попал в список: {r:?}"
        );
        // у владельца B тот же счёт в списке есть: тест проверяет границу, а не пустоту
        let rb = w
            .app
            .handle(&sender(OWNER_B), Incoming::Text(cmd.into()), now())
            .await;
        assert!(
            !rb[0].buttons.is_empty(),
            "{cmd}: свой счёт должен быть в списке: {rb:?}"
        );
    }
    // кнопка с чужим id счёта не срабатывает
    let bill_b: i64 = sqlx::query_scalar("SELECT id FROM bill")
        .fetch_one(w.app.pool())
        .await
        .unwrap();
    for data in [
        format!("ap:{bill_b}"),
        format!("rj:{bill_b}"),
        format!("pd:{bill_b}"),
        format!("ri:{bill_b}"),
        format!("vo:{bill_b}"),
        format!("sw:{pb}"),
    ] {
        let r = w
            .app
            .handle(&sender(OWNER_A), Incoming::Callback(data.clone()), now())
            .await;
        assert!(
            r[0].text.contains("не найден") || r[0].text.contains("не ваш"),
            "{data}: {r:?}"
        );
    }
    let status: String = sqlx::query_scalar("SELECT status FROM bill WHERE id = ?")
        .bind(bill_b)
        .fetch_one(w.app.pool())
        .await
        .unwrap();
    assert_eq!(status, "approved", "чужие кнопки ничего не изменили");
}

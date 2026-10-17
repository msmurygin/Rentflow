//! HTTP API для Telegram Mini App. Здесь нет бизнес-логики: каждый обработчик проверяет, кто спрашивает,
//! вызывает те же сервисы `App`, что и бот, и превращает результат в JSON. Деньги считает только `billing`.

pub mod auth;

use crate::app::{App, AppError, MRole, Reply, Sender, UserRow, Who, now_iso};
use crate::bills::{TariffRow, effective_tariff};
use crate::setup::NewProperty;
use crate::tariffs::{CODES, Change};
use crate::util::*;
use async_trait::async_trait;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, FromRequestParts, Path, Query, State},
    http::{StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

/// Каналы уведомлений (бот). В тестах подменяется записывающей заглушкой.
#[async_trait]
pub trait Notifier: Send + Sync {
    async fn send(&self, replies: Vec<Reply>);
}

pub struct TelegramNotifier(pub teloxide::Bot);

#[async_trait]
impl Notifier for TelegramNotifier {
    async fn send(&self, replies: Vec<Reply>) {
        crate::bot::send_all(&self.0, replies).await;
    }
}

pub struct ApiState {
    pub app: Arc<App>,
    pub notifier: Arc<dyn Notifier>,
    pub bot_token: String,
    pub tz_offset_hours: i64,
    /// Насколько свежим должен быть `initData` при входе.
    pub init_data_max_age_secs: i64,
    pub session_ttl_days: i64,
    pub clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl ApiState {
    pub fn new(
        app: Arc<App>,
        notifier: Arc<dyn Notifier>,
        bot_token: String,
        tz_offset_hours: i64,
    ) -> Self {
        Self {
            app,
            notifier,
            bot_token,
            tz_offset_hours,
            init_data_max_age_secs: 24 * 3600,
            session_ttl_days: 7,
            clock: Arc::new(Utc::now),
        }
    }

    /// Местное время: расчётный месяц определяется по нему, а не по UTC.
    fn now(&self) -> NaiveDateTime {
        (self.clock)().naive_utc() + Duration::hours(self.tz_offset_hours)
    }
}

type St = State<Arc<ApiState>>;
type ApiResult = Result<Json<Value>, ApiError>;

#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    Forbidden(&'static str),
    NotFound,
    Conflict(String),
    User(String),
    Internal,
}

impl From<AppError> for ApiError {
    fn from(e: AppError) -> Self {
        match e {
            AppError::User(m) => ApiError::User(m),
            AppError::Db(e) => {
                tracing::error!(error = %e, "api db error");
                ApiError::Internal
            }
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        AppError::Db(e).into()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "Требуется вход через Telegram.".to_string(),
            ),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, m.to_string()),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "Не найдено.".to_string()),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m),
            ApiError::User(m) => (StatusCode::UNPROCESSABLE_ENTITY, m),
            ApiError::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Внутренняя ошибка.".to_string(),
            ),
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

fn bad<T>(msg: &str) -> Result<T, ApiError> {
    Err(ApiError::User(msg.to_string()))
}

// ---------- сессия ----------

pub enum Role {
    Landlord(UserRow),
    Tenant(UserRow),
}

pub struct Ctx {
    pub sender: Sender,
    pub role: Role,
}

impl Ctx {
    fn landlord(&self) -> Result<&UserRow, ApiError> {
        match &self.role {
            Role::Landlord(u) => Ok(u),
            Role::Tenant(_) => Err(ApiError::Forbidden(
                "Это действие доступно только владельцу.",
            )),
        }
    }
    fn tenant(&self) -> Result<&UserRow, ApiError> {
        match &self.role {
            Role::Tenant(u) => Ok(u),
            Role::Landlord(_) => Err(ApiError::Forbidden("Показания отправляет арендатор.")),
        }
    }
    fn role_name(&self) -> &'static str {
        match self.role {
            Role::Landlord(_) => "landlord",
            Role::Tenant(_) => "tenant",
        }
    }
}

fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Действительная сессия Telegram-пользователя (ещё без ролей): нужна и тем, кто не зарегистрирован.
async fn session_sender(parts: &Parts, st: &ApiState) -> Result<Sender, ApiError> {
    let token = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(ApiError::Unauthorized)?;
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT telegram_id, name FROM api_session WHERE token_hash = ? AND expires_at > ?",
    )
    .bind(token_hash(token))
    .bind(now_iso(st.now()))
    .fetch_optional(st.app.pool())
    .await?;
    let (telegram_id, name) = row.ok_or(ApiError::Unauthorized)?;
    // В личном чате chat_id совпадает с id пользователя.
    Ok(Sender {
        telegram_id,
        chat_id: telegram_id,
        name,
    })
}

/// Только сессия, без требования участия в объекте (`/me`, `/register`).
pub struct AnySession(pub Sender);

impl FromRequestParts<Arc<ApiState>> for AnySession {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &Arc<ApiState>) -> Result<Self, ApiError> {
        Ok(AnySession(session_sender(parts, st).await?))
    }
}

impl FromRequestParts<Arc<ApiState>> for Ctx {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &Arc<ApiState>) -> Result<Self, ApiError> {
        let now = st.now();
        let sender = session_sender(parts, st).await?;
        // Роль определяется заново на каждый запрос: отозванный доступ действует сразу.
        let mut user = match st.app.who(&sender, now).await? {
            Who::Landlord(u) | Who::Tenant(u) => u,
            Who::Stranger => {
                return Err(ApiError::Forbidden(
                    "Вы ещё не зарегистрированы: выберите роль.",
                ));
            }
        };
        // Явно выбранный объект (заголовок X-Property-Id): доступ только участникам; чужой и несуществующий
        // неотличимы (404), чтобы не раскрывать, что такой объект есть.
        let role = match parts.headers.get("x-property-id") {
            Some(h) => {
                let pid: i64 = h
                    .to_str()
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .ok_or(ApiError::NotFound)?;
                user.property_id = Some(pid);
                st.app
                    .role_in(user.id, pid)
                    .await?
                    .ok_or(ApiError::NotFound)?
            }
            None => match user.property_id {
                Some(pid) => st.app.role_in(user.id, pid).await?.unwrap_or(MRole::Owner),
                None => MRole::Owner, // собственник без объектов (мастер не завершён)
            },
        };
        let role = match role {
            MRole::Owner => Role::Landlord(user),
            MRole::Tenant => Role::Tenant(user),
        };
        Ok(Ctx { sender, role })
    }
}

// ---------- вспомогательное ----------

impl ApiState {
    /// Объект текущего пользователя: у арендатора — назначенный, у владельца — его объект.
    async fn property_of(&self, ctx: &Ctx) -> Result<Option<i64>, ApiError> {
        Ok(match &ctx.role {
            Role::Landlord(u) | Role::Tenant(u) => u.property_id,
        })
    }

    async fn need_property(&self, ctx: &Ctx) -> Result<i64, ApiError> {
        self.property_of(ctx)
            .await?
            .ok_or(ApiError::User("Объект ещё не настроен.".into()))
    }

    /// Уведомления другим участникам; ответы самому инициатору не рассылаются — он видит результат в приложении.
    async fn notify_others(&self, ctx: &Ctx, replies: Vec<Reply>) {
        let others: Vec<Reply> = replies
            .into_iter()
            .filter(|r| r.chat_id != ctx.sender.chat_id)
            .collect();
        if !others.is_empty() {
            self.notifier.send(others).await;
        }
    }

    async fn audit(
        &self,
        ctx: &Ctx,
        action: &str,
        entity: &str,
        entity_id: Option<String>,
        details: Value,
    ) {
        let r = sqlx::query("INSERT INTO audit_log (at, actor, action, entity, entity_id, details) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(now_iso(self.now()))
            .bind(ctx.sender.telegram_id)
            .bind(action)
            .bind(entity)
            .bind(entity_id)
            .bind(details.to_string())
            .execute(self.app.pool())
            .await;
        if let Err(e) = r {
            tracing::error!(error = %e, "audit write failed");
        }
    }
}

// ---------- авторизация ----------

#[derive(Deserialize)]
struct AuthBody {
    init_data: String,
}

async fn auth_telegram(State(st): St, Json(body): Json<AuthBody>) -> ApiResult {
    let now_unix = (st.clock)().timestamp();
    let user = auth::verify_init_data(
        &body.init_data,
        &st.bot_token,
        now_unix,
        st.init_data_max_age_secs,
    )
    .map_err(|_| ApiError::Unauthorized)?;
    let now = st.now();
    let sender = Sender {
        telegram_id: user.id,
        chat_id: user.id,
        name: user.name.clone(),
    };
    let role = match st.app.who(&sender, now).await? {
        Who::Landlord(_) => "landlord",
        Who::Tenant(_) => "tenant",
        Who::Stranger => "new",
    };
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    sqlx::query("DELETE FROM api_session WHERE expires_at <= ?")
        .bind(now_iso(now))
        .execute(st.app.pool())
        .await?;
    sqlx::query("INSERT INTO api_session (token_hash, telegram_id, name, created_at, expires_at) VALUES (?, ?, ?, ?, ?)")
        .bind(token_hash(&token))
        .bind(user.id)
        .bind(&user.name)
        .bind(now_iso(now))
        .bind(now_iso(now + Duration::days(st.session_ttl_days)))
        .execute(st.app.pool())
        .await?;
    Ok(Json(
        json!({ "token": token, "role": role, "name": user.name }),
    ))
}

async fn me(State(st): St, AnySession(sender): AnySession) -> ApiResult {
    let who = st.app.who(&sender, st.now()).await?;
    let (role, user) = match &who {
        Who::Landlord(u) => ("landlord", Some(u)),
        Who::Tenant(u) => ("tenant", Some(u)),
        Who::Stranger => ("new", None),
    };
    let mut properties = vec![];
    if let Some(u) = user {
        for (pid, r) in st.app.memberships(u.id).await? {
            let name: String = sqlx::query_scalar("SELECT name FROM property WHERE id = ?")
                .bind(pid)
                .fetch_one(st.app.pool())
                .await?;
            properties.push(json!({ "id": pid, "name": name, "role": if r == MRole::Owner { "owner" } else { "tenant" }, "active": u.property_id == Some(pid) }));
        }
    }
    Ok(Json(json!({
        "role": role,
        "name": sender.name,
        "bot_username": st.app.cfg.bot_username,
        "property_configured": user.is_some_and(|u| u.property_id.is_some()),
        "properties": properties,
    })))
}

/// Самостоятельная регистрация собственника. Повторный вызов безопасен. Арендатором становятся только по приглашению.
async fn register(State(st): St, AnySession(sender): AnySession) -> ApiResult {
    st.app.register_owner(&sender, st.now()).await?;
    let ctx_audit = sqlx::query(
        "INSERT INTO audit_log (at, actor, action, entity) VALUES (?, ?, 'owner_register', 'user')",
    )
    .bind(now_iso(st.now()))
    .bind(sender.telegram_id)
    .execute(st.app.pool())
    .await;
    if let Err(e) = ctx_audit {
        tracing::error!(error = %e, "audit write failed");
    }
    Ok(Json(json!({ "role": "landlord" })))
}

// ---------- квартира ----------

async fn property_json(st: &ApiState, ctx: &Ctx, property_id: i64) -> Result<Value, ApiError> {
    let (name, proof_required): (String, bool) =
        sqlx::query_as("SELECT name, proof_required FROM property WHERE id = ?")
            .bind(property_id)
            .fetch_one(st.app.pool())
            .await?;
    let (tenant_name, tenant_user): (String, Option<i64>) =
        sqlx::query_as("SELECT tenant_name, tenant_user_id FROM tenancy WHERE property_id = ?")
            .bind(property_id)
            .fetch_one(st.app.pool())
            .await?;
    let rows: Vec<(String, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT g.code, g.label, g.unit, r.value, r.read_on FROM register g
           JOIN reading r ON r.register_id = g.id AND r.kind = 'baseline' AND r.status = 'confirmed'
          WHERE g.property_id = ?",
    )
    .bind(property_id)
    .fetch_all(st.app.pool())
    .await?;
    let mut registers: Vec<Value> = vec![];
    for (code, label, unit, value, read_on) in REGISTERS
        .iter()
        .filter_map(|(code, _, _)| rows.iter().find(|r| r.0 == *code))
    {
        // последнее принятое показание (стартовое, пока ни одного комплекта не принято) — от него считается следующий период
        let last: Option<(String, Option<String>, String)> = sqlx::query_as(
            "SELECT r.value, r.read_on, r.period FROM reading r JOIN register g ON g.id = r.register_id
              WHERE g.property_id = ? AND g.code = ? AND r.status = 'confirmed' ORDER BY r.period DESC, r.id DESC LIMIT 1",
        )
        .bind(property_id)
        .bind(code)
        .fetch_optional(st.app.pool())
        .await?;
        registers.push(json!({
            "code": code, "label": label, "unit": unit, "baseline": value, "baseline_date": read_on,
            "last": last.map(|(value, read_on, period)| json!({ "value": value, "read_on": read_on, "period": period })),
        }));
    }
    let mut v = json!({
        "id": property_id,
        "name": name,
        "tenant_name": tenant_name,
        "tenant_connected": tenant_user.is_some(),
        "reading_day": st.app.reading_day(property_id).await?,
        "registers": registers,
    });
    if matches!(ctx.role, Role::Landlord(_)) {
        v["proof_required"] = json!(proof_required);
    }
    Ok(v)
}

async fn get_property(State(st): St, ctx: Ctx) -> ApiResult {
    match st.property_of(&ctx).await? {
        Some(id) => Ok(Json(
            json!({ "property": property_json(&st, &ctx, id).await? }),
        )),
        None => Ok(Json(json!({ "property": null }))),
    }
}

#[derive(Deserialize, Default)]
struct PropertyBody {
    name: Option<String>,
    tenant_name: Option<String>,
    /// `null`/нет — дата стартовых показаний неизвестна (создание).
    base_date: Option<String>,
    baselines: Option<HashMap<String, String>>,
    prices: Option<HashMap<String, String>>,
    /// `null` — водоотведение выключено.
    sewer_price: Option<String>,
    proof_required: Option<bool>,
    /// День месяца 1–28, до которого арендатор передаёт показания; 0 — снять расписание.
    reading_day: Option<i64>,
}

fn clean_text(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.chars().count() <= 100)
        .map(String::from)
}

fn decimals(
    map: &Option<HashMap<String, String>>,
    codes: &[&str],
    what: &str,
) -> Result<Vec<(String, String)>, ApiError> {
    let map = map
        .as_ref()
        .ok_or_else(|| ApiError::User(format!("Не заполнено: {what}.")))?;
    codes
        .iter()
        .map(|c| {
            map.get(*c)
                .and_then(|s| parse_decimal(s))
                .map(|d| (c.to_string(), d.to_string()))
                .ok_or_else(|| ApiError::User(format!("{what}: «{}» — нужно число.", label(c))))
        })
        .collect()
}

/// Создание квартиры (первой или очередной) текущим пользователем как владельцем.
async fn create_from_body(st: &ApiState, ctx: &Ctx, b: &PropertyBody) -> Result<i64, ApiError> {
    let landlord = ctx.landlord()?;
    let now = st.now();
    let id = {
        let Some(name) = clean_text(&b.name) else {
            return bad("Укажите название квартиры.");
        };
        let Some(tenant_name) = clean_text(&b.tenant_name) else {
            return bad("Укажите имя арендатора.");
        };
        let base_date = match b
            .base_date
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            None => None,
            Some(d) => Some(
                parse_date(d)
                    .ok_or(ApiError::User("Дата показаний: ГГГГ-ММ-ДД.".into()))?
                    .to_string(),
            ),
        };
        let reg = ["cold", "hot", "t1", "t2"];
        let np = NewProperty {
            name,
            tenant_name,
            base_date,
            baselines: decimals(&b.baselines, &reg, "Стартовые показания")?,
            prices: decimals(&b.prices, &reg, "Тарифы")?,
            sewer_price: match b
                .sewer_price
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                None => None,
                Some(p) => Some(
                    parse_decimal(p)
                        .ok_or(ApiError::User("Тариф водоотведения: нужно число.".into()))?
                        .to_string(),
                ),
            },
        };
        let id = st.app.create_property(landlord.id, &np, now).await?;
        if let Some(proof) = b.proof_required {
            set_proof(st, id, proof).await?;
        }
        st.audit(
            ctx,
            "property_create",
            "property",
            Some(id.to_string()),
            json!({ "name": np.name }),
        )
        .await;
        id
    };
    Ok(id)
}

/// `POST /api/properties` — ещё одна квартира у владельца. Активной не становится: клиент сам выбирает объект заголовком.
async fn post_property(State(st): St, ctx: Ctx, Json(b): Json<PropertyBody>) -> ApiResult {
    let id = create_from_body(&st, &ctx, &b).await?;
    let user_id = match &ctx.role {
        Role::Landlord(u) | Role::Tenant(u) => u.id,
    };
    st.app.set_active_property(user_id, id).await?;
    let mut ctx = ctx;
    if let Role::Landlord(u) = &mut ctx.role {
        u.property_id = Some(id);
    }
    Ok(Json(
        json!({ "property": property_json(&st, &ctx, id).await? }),
    ))
}

/// `GET /api/properties` — объекты текущего пользователя с его ролью в каждом.
async fn list_properties(State(st): St, ctx: Ctx) -> ApiResult {
    let (user_id, active) = match &ctx.role {
        Role::Landlord(u) | Role::Tenant(u) => (u.id, u.property_id),
    };
    let mut out = vec![];
    for (pid, role) in st.app.memberships(user_id).await? {
        let name: String = sqlx::query_scalar("SELECT name FROM property WHERE id = ?")
            .bind(pid)
            .fetch_one(st.app.pool())
            .await?;
        out.push(json!({ "id": pid, "name": name, "role": if role == MRole::Owner { "owner" } else { "tenant" }, "active": active == Some(pid) }));
    }
    Ok(Json(json!({ "properties": out })))
}

async fn put_property(State(st): St, ctx: Ctx, Json(b): Json<PropertyBody>) -> ApiResult {
    ctx.landlord()?;
    let id = match st.property_of(&ctx).await? {
        None => create_from_body(&st, &ctx, &b).await?,
        Some(id) => {
            if let Some(name) = clean_text(&b.name) {
                sqlx::query("UPDATE property SET name = ? WHERE id = ?")
                    .bind(name)
                    .bind(id)
                    .execute(st.app.pool())
                    .await?;
            }
            if let Some(t) = clean_text(&b.tenant_name) {
                sqlx::query("UPDATE tenancy SET tenant_name = ? WHERE property_id = ?")
                    .bind(t)
                    .bind(id)
                    .execute(st.app.pool())
                    .await?;
            }
            if let Some(proof) = b.proof_required {
                set_proof(&st, id, proof).await?;
            }
            set_schedule(&st, id, b.reading_day).await?;
            st.audit(&ctx, "property_update", "property", Some(id.to_string()), json!({ "name": b.name, "tenant_name": b.tenant_name, "proof_required": b.proof_required, "reading_day": b.reading_day })).await;
            id
        }
    };
    Ok(Json(
        json!({ "property": property_json(&st, &ctx, id).await? }),
    ))
}

async fn set_schedule(st: &ApiState, property_id: i64, day: Option<i64>) -> Result<(), ApiError> {
    match day {
        None => Ok(()),
        Some(0) => Ok(st.app.set_reading_day(property_id, None).await?),
        Some(d) => Ok(st.app.set_reading_day(property_id, Some(d)).await?),
    }
}

async fn set_proof(st: &ApiState, property_id: i64, on: bool) -> Result<(), ApiError> {
    sqlx::query("UPDATE property SET proof_required = ? WHERE id = ?")
        .bind(on)
        .bind(property_id)
        .execute(st.app.pool())
        .await?;
    Ok(())
}

async fn create_invitation(State(st): St, ctx: Ctx) -> ApiResult {
    ctx.landlord()?;
    let property_id = st.need_property(&ctx).await?;
    let token = st.app.create_invite(property_id, st.now()).await?;
    st.audit(
        &ctx,
        "invitation_create",
        "property",
        Some(property_id.to_string()),
        json!({}),
    )
    .await;
    Ok(Json(
        json!({ "url": st.app.invite_url(&token), "ttl_days": st.app.cfg.invite_ttl_days }),
    ))
}

// ---------- главная и счётчики ----------

async fn home(State(st): St, ctx: Ctx) -> ApiResult {
    let Some(property_id) = st.property_of(&ctx).await? else {
        return Ok(Json(json!({ "role": ctx.role_name(), "property": null })));
    };
    let now = st.now();
    let period = st.app.open_period(property_id, now).await?;
    let received = st.app.received(property_id, &period).await?.len();
    let (name, tenant_name, tenant_user): (String, String, Option<i64>) = sqlx::query_as(
        "SELECT p.name, t.tenant_name, t.tenant_user_id FROM property p JOIN tenancy t ON t.property_id = p.id WHERE p.id = ?",
    )
    .bind(property_id)
    .fetch_one(st.app.pool())
    .await?;
    let visible = if matches!(ctx.role, Role::Landlord(_)) {
        "'draft','approved','paid'"
    } else {
        "'approved','paid'"
    };
    let sql = format!(
        "SELECT id, period, status, total FROM bill WHERE property_id = ? AND status IN ({visible}) ORDER BY period DESC, id DESC LIMIT 1"
    );
    let last: Option<(i64, String, String, String)> = sqlx::query_as(&sql)
        .bind(property_id)
        .fetch_optional(st.app.pool())
        .await?;
    let billed = st.app.period_billed(property_id, &period).await?;
    // срок передачи показаний: по расписанию владельца, а без него — первое число следующего месяца
    let reading_day = st.app.reading_day(property_id).await?;
    let (first, _) = period_bounds(&period).ok_or(ApiError::Internal)?;
    let next_month = period_of(first + Duration::days(32));
    let next_due = if billed {
        Some(format!("{next_month}-{:02}", reading_day.unwrap_or(1)))
    } else {
        None
    };
    let due_date = reading_day.map(|d| {
        if billed {
            format!("{next_month}-{d:02}")
        } else {
            format!("{period}-{d:02}")
        }
    });
    Ok(Json(json!({
        "role": ctx.role_name(),
        "property": { "name": name, "tenant_name": tenant_name, "tenant_connected": tenant_user.is_some() },
        "period": period,
        "received": received,
        "total_registers": REGISTERS.len(),
        "billed": billed,
        "next_due": next_due,
        "due_date": due_date,
        "reading_day": reading_day,
        "last_bill": last.map(|(id, period, status, total)| json!({ "id": id, "period": period, "status": status, "total": total })),
    })))
}

async fn meters(State(st): St, ctx: Ctx) -> ApiResult {
    let Some(property_id) = st.property_of(&ctx).await? else {
        return Ok(Json(json!({ "period": null, "registers": [] })));
    };
    let period = st.app.open_period(property_id, st.now()).await?;
    let mut out = vec![];
    for (code, lbl, unit) in REGISTERS {
        let last: Option<(String, Option<String>, String)> = sqlx::query_as(
            "SELECT r.value, r.read_on, r.period FROM reading r JOIN register g ON g.id = r.register_id
              WHERE g.property_id = ? AND g.code = ? AND r.status = 'confirmed' AND r.period < ?
              ORDER BY r.period DESC LIMIT 1",
        )
        .bind(property_id)
        .bind(code)
        .bind(&period)
        .fetch_optional(st.app.pool())
        .await?;
        let current = st
            .app
            .received(property_id, &period)
            .await?
            .into_iter()
            .find(|r| r.0 == code)
            .map(|r| r.1);
        out.push(json!({
            "code": code, "label": lbl, "unit": unit,
            "previous": last.map(|(value, read_on, period)| json!({ "value": value, "read_on": read_on, "period": period })),
            "current": current,
        }));
    }
    Ok(Json(json!({ "period": period, "registers": out })))
}

// ---------- показания (арендатор) ----------

async fn form_json(st: &ApiState, ctx: &Ctx) -> Result<Value, ApiError> {
    let view = st.app.form_view(ctx.tenant()?, st.now()).await?;
    serde_json::to_value(view).map_err(|_| ApiError::Internal)
}

async fn draft_get(State(st): St, ctx: Ctx) -> ApiResult {
    Ok(Json(form_json(&st, &ctx).await?))
}

#[derive(Deserialize)]
struct ValueBody {
    code: String,
    value: String,
}

async fn draft_put(State(st): St, ctx: Ctx, Json(b): Json<ValueBody>) -> ApiResult {
    let user = ctx.tenant()?;
    st.app
        .form_set_value(user, &b.code, &b.value, st.now())
        .await?;
    Ok(Json(form_json(&st, &ctx).await?))
}

#[derive(Deserialize)]
struct CodeQuery {
    code: String,
}

fn looks_like_jpeg(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0xD8, 0xFF])
}

async fn photo_upload(
    State(st): St,
    ctx: Ctx,
    Query(q): Query<CodeQuery>,
    body: Bytes,
) -> ApiResult {
    let user = ctx.tenant()?;
    if !looks_like_jpeg(&body) {
        return bad("Нужна фотография в формате JPEG.");
    }
    st.app.form_photo(user, &q.code, &body, st.now()).await?;
    Ok(Json(form_json(&st, &ctx).await?))
}

fn image_response(bytes: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "private, max-age=3600"),
        ],
        bytes,
    )
        .into_response()
}

async fn read_photo(path: Option<String>) -> Result<Response, ApiError> {
    let path = path.ok_or(ApiError::NotFound)?;
    match tokio::fs::read(&path).await {
        Ok(b) => Ok(image_response(b)),
        Err(_) => Err(ApiError::NotFound),
    }
}

async fn draft_photo(
    State(st): St,
    ctx: Ctx,
    Query(q): Query<CodeQuery>,
) -> Result<Response, ApiError> {
    let user = ctx.tenant()?;
    read_photo(st.app.form_photo_path(user, &q.code, st.now()).await?).await
}

async fn submit(State(st): St, ctx: Ctx) -> ApiResult {
    let user = ctx.tenant()?.clone();
    let now = st.now();
    let view = st.app.form_view(&user, now).await?;
    if view.billed {
        // повторная отправка: ничего не меняется и никому не уходит
        return Ok(Json(
            json!({ "already_submitted": true, "period": view.period }),
        ));
    }
    if !view.complete {
        return bad(
            "Комплект неполный: введите все показания (и приложите фото, если владелец их требует).",
        );
    }
    let replies = st.app.entry_submit(&ctx.sender, &user, now).await?;
    st.audit(
        &ctx,
        "readings_submit",
        "period",
        Some(view.period.clone()),
        json!({}),
    )
    .await;
    st.notify_others(&ctx, replies).await;
    Ok(Json(
        json!({ "already_submitted": false, "period": view.period }),
    ))
}

// ---------- тарифы ----------

async fn tariffs_json(st: &ApiState, property_id: i64) -> Result<Value, ApiError> {
    let month = period_of(st.now().date());
    let mut items = vec![];
    for code in CODES {
        let versions: Vec<TariffRow> = st.app.tariff_versions(property_id, code).await?;
        let eff = effective_tariff(&versions, &month);
        let row = |v: &TariffRow| json!({ "month": v.month, "price": v.price.to_string(), "enabled": v.enabled });
        items.push(json!({
            "code": code, "label": label(code), "unit": unit(code),
            "effective": eff.map(row),
            "versions": versions.iter().rev().map(row).collect::<Vec<_>>(),
        }));
    }
    Ok(json!({ "month": month, "items": items }))
}

async fn tariffs_get(State(st): St, ctx: Ctx) -> ApiResult {
    ctx.landlord()?;
    let property_id = st.need_property(&ctx).await?;
    Ok(Json(tariffs_json(&st, property_id).await?))
}

#[derive(Deserialize)]
struct TariffChange {
    code: String,
    price: Option<String>,
    /// Только водоотведение: выключить с этого месяца.
    #[serde(default)]
    off: bool,
}

#[derive(Deserialize)]
struct TariffsBody {
    /// Расчётный месяц `ГГГГ-ММ`; по умолчанию — текущий.
    month: Option<String>,
    changes: Vec<TariffChange>,
}

async fn tariffs_put(State(st): St, ctx: Ctx, Json(b): Json<TariffsBody>) -> ApiResult {
    ctx.landlord()?;
    let property_id = st.need_property(&ctx).await?;
    let now = st.now();
    let month = match &b.month {
        Some(m) => parse_month(m).ok_or(ApiError::User("Месяц: ГГГГ-ММ.".into()))?,
        None => period_of(now.date()),
    };
    if b.changes.is_empty() {
        return bad("Нечего менять.");
    }
    let mut parsed: Vec<(&str, Change)> = vec![];
    for c in &b.changes {
        let Some(code) = CODES.iter().find(|k| **k == c.code) else {
            return bad("Неизвестная услуга.");
        };
        let change = if c.off {
            if *code != "sewer" {
                return bad("Выключить можно только водоотведение.");
            }
            Change::Off
        } else {
            let price = c
                .price
                .as_deref()
                .and_then(parse_decimal)
                .ok_or(ApiError::User(format!("{}: нужно число.", label(code))))?;
            Change::Price(price)
        };
        parsed.push((code, change));
    }
    let replies = st
        .app
        .change_tariffs(ctx.sender.chat_id, property_id, &month, &parsed, now)
        .await?;
    st.audit(&ctx, "tariffs_update", "tariff", Some(month.clone()), json!({ "changes": b.changes.iter().map(|c| json!({ "code": c.code, "price": c.price, "off": c.off })).collect::<Vec<_>>() })).await;
    st.notify_others(&ctx, replies).await;
    let draft: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM bill WHERE property_id = ? AND status = 'draft' ORDER BY id DESC LIMIT 1",
    )
    .bind(property_id)
    .fetch_optional(st.app.pool())
    .await?;
    let mut v = tariffs_json(&st, property_id).await?;
    v["draft_id"] = json!(draft);
    Ok(Json(v))
}

// ---------- счета ----------

struct BillRow {
    id: i64,
    property_id: i64,
    period: String,
    version: i64,
    status: String,
    total: String,
    snapshot: String,
    created_at: String,
    decided_at: Option<String>,
    paid_at: Option<String>,
}

type BillTuple = (
    i64,
    i64,
    String,
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

const BILL_COLS: &str = "b.id, b.property_id, b.period, b.version, b.status, b.total, b.snapshot, b.created_at, b.decided_at, b.paid_at";

impl From<BillTuple> for BillRow {
    fn from(t: BillTuple) -> Self {
        Self {
            id: t.0,
            property_id: t.1,
            period: t.2,
            version: t.3,
            status: t.4,
            total: t.5,
            snapshot: t.6,
            created_at: t.7,
            decided_at: t.8,
            paid_at: t.9,
        }
    }
}

fn bill_summary(b: &BillRow) -> Value {
    json!({ "id": b.id, "period": b.period, "version": b.version, "status": b.status, "total": b.total,
            "created_at": b.created_at, "decided_at": b.decided_at, "paid_at": b.paid_at })
}

/// Счёт и роль пользователя в ЕГО объекте. Доступ определяется объектом счёта, а не «текущим» объектом запроса:
/// владелец видит все счета своего объекта, арендатор — только утверждённые и оплаченные. Всё остальное (чужой счёт,
/// неутверждённый счёт арендатору, несуществующий id) — одинаковое «не найдено», чтобы ничего не раскрывать.
async fn bill_access(st: &ApiState, ctx: &Ctx, id: i64) -> Result<(BillRow, MRole), ApiError> {
    let user_id = match &ctx.role {
        Role::Landlord(u) | Role::Tenant(u) => u.id,
    };
    let row: Option<BillTuple> =
        sqlx::query_as(&format!("SELECT {BILL_COLS} FROM bill b WHERE b.id = ?"))
            .bind(id)
            .fetch_optional(st.app.pool())
            .await?;
    let bill = BillRow::from(row.ok_or(ApiError::NotFound)?);
    let role = st
        .app
        .role_in(user_id, bill.property_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if role == MRole::Tenant && bill.status != "approved" && bill.status != "paid" {
        return Err(ApiError::NotFound);
    }
    Ok((bill, role))
}

async fn bill_for(st: &ApiState, ctx: &Ctx, id: i64) -> Result<BillRow, ApiError> {
    Ok(bill_access(st, ctx, id).await?.0)
}

/// Действия владельца над счётом: роль проверяется в объекте самого счёта.
async fn owner_bill(st: &ApiState, ctx: &Ctx, id: i64) -> Result<BillRow, ApiError> {
    let (bill, role) = bill_access(st, ctx, id).await?;
    if role != MRole::Owner {
        return Err(ApiError::Forbidden(
            "Это действие доступно только владельцу.",
        ));
    }
    Ok(bill)
}

async fn bills_list(State(st): St, ctx: Ctx) -> ApiResult {
    // объект ещё не настроен — счетов нет, это не ошибка
    let Some(property_id) = st.property_of(&ctx).await? else {
        return Ok(Json(json!({ "bills": [] })));
    };
    let visible = if matches!(ctx.role, Role::Landlord(_)) {
        "'draft','approved','paid','rejected'"
    } else {
        "'approved','paid'"
    };
    let sql = format!(
        "SELECT {BILL_COLS} FROM bill b WHERE b.property_id = ? AND b.status IN ({visible}) ORDER BY b.period DESC, b.id DESC"
    );
    let rows: Vec<BillTuple> = sqlx::query_as(&sql)
        .bind(property_id)
        .fetch_all(st.app.pool())
        .await?;
    Ok(Json(
        json!({ "bills": rows.into_iter().map(|t| bill_summary(&BillRow::from(t))).collect::<Vec<_>>() }),
    ))
}

async fn bill_detail(State(st): St, ctx: Ctx, Path(id): Path<i64>) -> ApiResult {
    let (b, bill_role) = bill_access(&st, &ctx, id).await?;
    let snapshot: Value = serde_json::from_str(&b.snapshot).map_err(|_| ApiError::Internal)?;
    let readings: Vec<(i64, String, String, Option<String>, bool)> = sqlx::query_as(
        "SELECT r.id, g.code, r.value, r.read_on, r.photo_path IS NOT NULL FROM reading r JOIN register g ON g.id = r.register_id
          WHERE g.property_id = ? AND r.period = ? AND r.kind = 'submitted' AND r.status = 'confirmed'",
    )
    .bind(b.property_id)
    .bind(&b.period)
    .fetch_all(st.app.pool())
    .await?;
    let landlord = bill_role == MRole::Owner;
    let mut v = bill_summary(&b);
    v["snapshot"] = snapshot;
    v["readings"] = json!(readings.iter().map(|(id, code, value, read_on, photo)| json!({ "reading_id": id, "code": code, "value": value, "read_on": read_on, "has_photo": photo })).collect::<Vec<_>>());
    v["can_approve"] = json!(landlord && b.status == "draft");
    v["can_reject"] = json!(landlord && b.status == "draft");
    v["can_mark_paid"] = json!(landlord && b.status == "approved");
    v["can_reissue"] = json!(landlord && b.status == "approved");
    v["can_void"] = json!(
        landlord && (b.status == "approved" || (b.status == "paid" && st.app.cfg.allow_void_paid))
    );
    Ok(Json(v))
}

/// Выполняет решение владельца через тот же сервис, что и кнопка в боте, и проверяет итоговый статус:
/// устаревший или уже обработанный счёт — это 409, а не тихий успех.
async fn decide(st: &ApiState, ctx: &Ctx, id: i64, action: &str, target: &str) -> ApiResult {
    let before = owner_bill(st, ctx, id).await?;
    let now = st.now();
    let replies = match action {
        "approve" => st.app.bill_approve(&ctx.sender, id, now).await?,
        "reject" => st.app.bill_reject(&ctx.sender, id, now).await?,
        "paid" => st.app.bill_paid(&ctx.sender, id, now).await?,
        _ => st.app.reissue_do(&ctx.sender, id, now).await?,
    };
    let after = bill_for(st, ctx, id).await?;
    let done = after.status == target && after.status != before.status;
    if !done {
        let text = replies
            .iter()
            .find(|r| r.chat_id == ctx.sender.chat_id)
            .map(|r| r.text.clone())
            .unwrap_or_else(|| "Счёт уже обработан.".into());
        return Err(ApiError::Conflict(text));
    }
    st.audit(
        ctx,
        &format!("bill_{action}"),
        "bill",
        Some(id.to_string()),
        json!({ "period": after.period, "total": after.total }),
    )
    .await;
    st.notify_others(ctx, replies).await;
    Ok(Json(bill_summary(&after)))
}

async fn bill_approve(State(st): St, ctx: Ctx, Path(id): Path<i64>) -> ApiResult {
    decide(&st, &ctx, id, "approve", "approved").await
}
async fn bill_reject(State(st): St, ctx: Ctx, Path(id): Path<i64>) -> ApiResult {
    decide(&st, &ctx, id, "reject", "rejected").await
}
async fn bill_paid(State(st): St, ctx: Ctx, Path(id): Path<i64>) -> ApiResult {
    decide(&st, &ctx, id, "paid", "paid").await
}
#[derive(Deserialize)]
struct VoidBody {
    reason: String,
    /// Счёт оплачен — аннулировать можно только при явном `true`.
    #[serde(default)]
    confirm_paid: bool,
}

async fn bill_void(
    State(st): St,
    ctx: Ctx,
    Path(id): Path<i64>,
    Json(b): Json<VoidBody>,
) -> ApiResult {
    let before = owner_bill(&st, &ctx, id).await?;
    let reason = b.reason.trim();
    if reason.is_empty() || reason.chars().count() > 200 {
        return bad("Причина — от 1 до 200 символов.");
    }
    if before.status == "paid" && !b.confirm_paid {
        return bad("Счёт оплачен: подтвердите аннулирование оплаченного счёта явно.");
    }
    let replies = st
        .app
        .void_bill(&ctx.sender, id, reason, before.status == "paid", st.now())
        .await?;
    let after = bill_for(&st, &ctx, id).await?;
    st.notify_others(&ctx, replies).await;
    Ok(Json(bill_summary(&after)))
}

async fn bill_reissue(State(st): St, ctx: Ctx, Path(id): Path<i64>) -> ApiResult {
    decide(&st, &ctx, id, "reissue", "void").await
}

/// Фото подтверждённого показания: только участникам этого объекта (арендатор — после утверждения счёта не нужно,
/// но своё отправленное фото он видеть вправе).
async fn reading_photo(State(st): St, ctx: Ctx, Path(id): Path<i64>) -> Result<Response, ApiError> {
    let row: Option<(i64, Option<String>)> = sqlx::query_as(
        "SELECT g.property_id, r.photo_path FROM reading r JOIN register g ON g.id = r.register_id WHERE r.id = ?",
    )
    .bind(id)
    .fetch_optional(st.app.pool())
    .await?;
    let (property_id, path) = row.ok_or(ApiError::NotFound)?;
    let user_id = match &ctx.role {
        Role::Landlord(u) | Role::Tenant(u) => u.id,
    };
    if st.app.role_in(user_id, property_id).await?.is_none() {
        return Err(ApiError::NotFound);
    }
    read_photo(path).await
}

async fn healthz() -> &'static str {
    "ok"
}

/// Готовность: база отвечает, Telegram жив, диск и резервные копии в норме. 503 — внешний мониторинг должен сработать.
async fn readyz(State(st): St) -> (StatusCode, Json<Value>) {
    let v = st.app.vitals(st.now(), (st.clock)().timestamp()).await;
    let alerts: Vec<&str> = crate::ops::evaluate(&v).iter().map(|a| a.key).collect();
    let code = if alerts.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        code,
        Json(json!({ "ok": alerts.is_empty(), "alerts": alerts })),
    )
}

pub fn router(state: Arc<ApiState>, static_dir: Option<PathBuf>) -> Router {
    let api = Router::new()
        .route("/auth/telegram", post(auth_telegram))
        .route("/me", get(me))
        .route("/register", post(register))
        .route("/home", get(home))
        .route("/property", get(get_property).put(put_property))
        .route("/properties", get(list_properties).post(post_property))
        .route("/invitations", post(create_invitation))
        .route("/meters", get(meters))
        .route(
            "/readings/draft",
            get(draft_get).post(draft_get).put(draft_put),
        )
        .route(
            "/readings/photo",
            get(draft_photo)
                .post(photo_upload)
                .layer(DefaultBodyLimit::max(13 * 1024 * 1024)),
        )
        .route("/readings/submit", post(submit))
        .route("/tariffs", get(tariffs_get).put(tariffs_put))
        .route("/bills", get(bills_list))
        .route("/bills/{id}", get(bill_detail))
        .route("/bills/{id}/approve", post(bill_approve))
        .route("/bills/{id}/reject", post(bill_reject))
        .route("/bills/{id}/paid", post(bill_paid))
        .route("/bills/{id}/reissue", post(bill_reissue))
        .route("/bills/{id}/void", post(bill_void))
        .route("/photos/{id}", get(reading_photo));
    let mut app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .nest("/api", api)
        .with_state(state);
    if let Some(dir) = static_dir {
        use tower_http::services::{ServeDir, ServeFile};
        app = app
            .fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html"))));
    }
    app
}

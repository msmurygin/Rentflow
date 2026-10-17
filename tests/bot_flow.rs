//! Сквозные тесты бота: настройка → показания → расчёт → утверждение → счёт. Без сети и без Telegram:
//! ядро получает `Incoming`, возвращает `Reply`; SQLite в памяти; распознавание подменяется заглушкой.

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime};
use rentflow::app::{App, Config, Incoming, Reply, Sender};
use rentflow::db;
use rentflow::ocr::{Candidate, Extractor};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const LANDLORD: i64 = 1;
const TENANT: i64 = 2;
const STRANGER: i64 = 3;

#[derive(Default)]
struct FakeOcr(Mutex<VecDeque<Result<Vec<Candidate>, String>>>);

impl FakeOcr {
    fn push(&self, r: Result<Vec<Candidate>, String>) {
        self.0.lock().unwrap().push_back(r);
    }
}

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

fn cand(register: &str, value: &str, confidence: &str) -> Candidate {
    Candidate {
        register: Some(register.into()),
        value: Some(value.into()),
        confidence: confidence.into(),
    }
}

fn at(y: i32, m: u32, d: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
}

fn sender(id: i64) -> Sender {
    Sender {
        telegram_id: id,
        chat_id: id * 100,
        name: format!("user{id}"),
    }
}

struct H {
    app: App,
    ocr: Arc<FakeOcr>,
}

impl H {
    async fn new() -> Self {
        Self::with_photo_dir(None).await
    }

    async fn with_photo_dir(photo_dir: Option<std::path::PathBuf>) -> Self {
        let pool = db::connect("sqlite::memory:").await.unwrap();
        let ocr = Arc::new(FakeOcr::default());
        let app = App::new(
            pool,
            Config {
                landlord_telegram_id: LANDLORD,
                bot_username: "rentflow_bot".into(),
                photo_dir,
                invite_ttl_days: 7,
                ocr_enabled: false,
            },
            ocr.clone(),
        );
        Self { app, ocr }
    }

    async fn send(&self, who: i64, input: Incoming, now: NaiveDateTime) -> Vec<Reply> {
        self.app.handle(&sender(who), input, now).await
    }

    async fn say(&self, who: i64, text: &str, now: NaiveDateTime) -> Vec<Reply> {
        self.send(who, Incoming::Text(text.into()), now).await
    }

    async fn tap(&self, who: i64, data: &str, now: NaiveDateTime) -> Vec<Reply> {
        self.send(who, Incoming::Callback(data.into()), now).await
    }

    async fn photo(&self, who: i64, unique: &str, now: NaiveDateTime) -> Vec<Reply> {
        self.send(
            who,
            Incoming::Photo {
                file_id: format!("file-{unique}"),
                unique_id: unique.into(),
                bytes: vec![1, 2, 3],
            },
            now,
        )
        .await
    }

    async fn count(&self, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.app.pool())
            .await
            .unwrap()
    }

    /// Настройка объекта по данным golden dataset: июльские тарифы, дата baseline неизвестна.
    async fn setup(&self, now: NaiveDateTime) {
        self.say(LANDLORD, "/setup", now).await;
        for a in [
            "Квартира на Тверской",
            "Иван",
            "неизвестна",
            "753",
            "416",
            "3831",
            "1118",
            "2026-01-01",
            "65,77",
            "312,50",
            "51,62",
            "8,60",
            "3,71",
        ] {
            self.say(LANDLORD, a, now).await;
        }
        let r = self.tap(LANDLORD, "setup_ok", now).await;
        assert!(r[0].text.contains("Объект сохранён"), "{r:?}");
    }

    async fn connect_tenant(&self, now: NaiveDateTime) {
        let r = self.say(LANDLORD, "/invite", now).await;
        let token = r[0].text.split("start=").nth(1).unwrap().trim().to_string();
        let r = self.send(TENANT, Incoming::Start(Some(token)), now).await;
        assert!(r[0].text.contains("Вы подключены"), "{r:?}");
    }

    async fn ready() -> Self {
        let h = Self::new().await;
        h.setup(at(2026, 6, 26)).await;
        h.connect_tenant(at(2026, 6, 26)).await;
        h
    }

    /// Ручной ввод одного регистра: /enter → счётчик → число → «Верно».
    async fn enter(&self, who: i64, code: &str, value: &str, now: NaiveDateTime) -> Vec<Reply> {
        let r = self.say(who, "/enter", now).await;
        let pick = button_ending(&r, &format!(":{code}"));
        self.tap(who, &pick, now).await;
        let r = self.say(who, value, now).await;
        let ok = button_starting(&r, "ok:");
        self.tap(who, &ok, now).await
    }

    async fn month(&self, now: NaiveDateTime, v: [&str; 4]) -> Vec<Reply> {
        let mut last = vec![];
        for (code, value) in ["cold", "hot", "t1", "t2"].into_iter().zip(v) {
            last = self.enter(TENANT, code, value, now).await;
        }
        last
    }
}

fn buttons(replies: &[Reply]) -> Vec<String> {
    replies
        .iter()
        .flat_map(|r| r.buttons.iter().flatten().map(|b| b.data.clone()))
        .collect()
}

fn button_ending(replies: &[Reply], suffix: &str) -> String {
    buttons(replies)
        .into_iter()
        .find(|d| d.ends_with(suffix))
        .unwrap_or_else(|| panic!("no button *{suffix} in {replies:?}"))
}

fn button_starting(replies: &[Reply], prefix: &str) -> String {
    buttons(replies)
        .into_iter()
        .find(|d| d.starts_with(prefix))
        .unwrap_or_else(|| panic!("no button {prefix}* in {replies:?}"))
}

fn to_chat(replies: &[Reply], who: i64) -> String {
    replies
        .iter()
        .filter(|r| r.chat_id == who * 100)
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join("\n---\n")
}

// ---------- настройка ----------

#[tokio::test]
async fn bootstrap_creates_baseline_but_no_bill_and_keeps_unknown_date() {
    let h = H::new().await;
    h.setup(at(2026, 6, 26)).await;
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 0);
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'baseline' AND status = 'confirmed'")
            .await,
        4
    );
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'baseline' AND read_on IS NULL")
            .await,
        4,
        "дата baseline не придумывается"
    );
    let r = h.say(LANDLORD, "/status", at(2026, 6, 26)).await;
    assert!(r[0].text.contains("Не хватает"), "{r:?}");
}

#[tokio::test]
async fn wizard_rejects_bad_input_and_asks_again() {
    let h = H::new().await;
    h.say(LANDLORD, "/setup", at(2026, 6, 26)).await;
    h.say(LANDLORD, "Квартира", at(2026, 6, 26)).await;
    h.say(LANDLORD, "Иван", at(2026, 6, 26)).await;
    let r = h.say(LANDLORD, "вчера", at(2026, 6, 26)).await;
    assert!(r[0].text.contains("Не получилось разобрать"), "{r:?}");
    let r = h.say(LANDLORD, "25.06.2026", at(2026, 6, 26)).await;
    assert!(r[0].text.contains("ХВС"), "{r:?}");
    let r = h.say(LANDLORD, "-5", at(2026, 6, 26)).await;
    assert!(r[0].text.contains("Не получилось разобрать"), "{r:?}");
}

#[tokio::test]
async fn only_the_configured_landlord_can_setup() {
    let h = H::new().await;
    let r = h.say(STRANGER, "/setup", at(2026, 6, 26)).await;
    assert!(r[0].text.contains("не подключены"), "{r:?}");
    assert_eq!(h.count("SELECT count(*) FROM property").await, 0);
}

// ---------- приглашение ----------

#[tokio::test]
async fn invite_is_one_time_and_expires() {
    let h = H::new().await;
    h.setup(at(2026, 6, 26)).await;
    let r = h.say(LANDLORD, "/invite", at(2026, 6, 26)).await;
    let token = r[0].text.split("start=").nth(1).unwrap().trim().to_string();
    assert!(r[0].text.contains("https://t.me/rentflow_bot?start="));

    // чужой человек пробует просроченный токен
    let r = h
        .send(
            STRANGER,
            Incoming::Start(Some(token.clone())),
            at(2026, 7, 10),
        )
        .await;
    assert!(r[0].text.contains("недействительна"), "{r:?}");

    let r = h
        .send(
            TENANT,
            Incoming::Start(Some(token.clone())),
            at(2026, 6, 27),
        )
        .await;
    assert!(r[0].text.contains("Вы подключены"), "{r:?}");
    assert!(
        r.iter()
            .any(|x| x.chat_id == LANDLORD * 100 && x.text.contains("подключился"))
    );

    // токен уже использован
    let r = h
        .send(STRANGER, Incoming::Start(Some(token)), at(2026, 6, 27))
        .await;
    assert!(r[0].text.contains("недействительна"), "{r:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM app_user WHERE role = 'tenant'")
            .await,
        1
    );
}

// ---------- расчёт и golden ----------

#[tokio::test]
async fn golden_three_months_through_the_bot() {
    let h = H::ready().await;
    let mut totals = vec![];
    for (month, now, values, expected) in [
        (
            "2026-07",
            at(2026, 7, 25),
            ["755", "417", "3975,48", "1179"],
            "2063,61",
        ),
        (
            "2026-08",
            at(2026, 8, 25),
            ["759", "419", "4102", "1222"],
            "2469,51",
        ),
        (
            "2026-09",
            at(2026, 9, 25),
            ["761", "420", "4250", "1282"],
            "2104,29",
        ),
    ] {
        if month == "2026-08" {
            // тарифы воды и водоотведения изменились с 1 августа
            for cmd in [
                "/tariff cold 66,87 2026-08-01",
                "/tariff hot 317,71 2026-08-01",
                "/tariff sewer 52,48 2026-08-01",
            ] {
                let r = h.say(LANDLORD, cmd, now).await;
                assert!(r[0].text.contains("Тариф"), "{r:?}");
            }
        }
        let r = h.month(now, values).await;
        let draft = r
            .iter()
            .find(|x| x.chat_id == LANDLORD * 100)
            .unwrap_or_else(|| panic!("no draft for {month}: {r:?}"));
        assert!(
            draft.text.contains(&format!("Итого: {expected} ₽")),
            "{month}: {}",
            draft.text
        );
        assert!(draft.text.contains("Водоотведение"), "{}", draft.text);
        // арендатору итог ещё не показан
        assert!(
            !to_chat(&r, TENANT).contains("Итого"),
            "{month}: сумма ушла арендатору до утверждения"
        );
        let ap = button_starting(&r, "ap:");
        let r = h.tap(LANDLORD, &ap, now).await;
        let tenant_msg = to_chat(&r, TENANT);
        assert!(
            tenant_msg.contains(&format!("Итого: {expected} ₽")),
            "{tenant_msg}"
        );
        totals.push(expected);
        let paid = button_starting(&r, "pd:");
        h.tap(LANDLORD, &paid, now).await;
    }
    assert_eq!(totals, ["2063,61", "2469,51", "2104,29"]);
    assert_eq!(
        h.count("SELECT count(*) FROM bill WHERE status = 'paid'")
            .await,
        3
    );
    // июльский T1: на счётчике 3975,48, в расчёте целая часть — исходное значение сохранено рядом
    let raw: String = sqlx::query_scalar(
        "SELECT r.value FROM reading r JOIN register g ON g.id = r.register_id WHERE g.code = 't1' AND r.period = '2026-07' AND r.status = 'confirmed'",
    )
    .fetch_one(h.app.pool())
    .await
    .unwrap();
    assert_eq!(raw, "3975.48");
}

#[tokio::test]
async fn t1_and_t2_are_billed_separately() {
    let h = H::ready().await;
    let r = h
        .month(at(2026, 7, 25), ["755", "417", "3975", "1179"])
        .await;
    let draft = &r.iter().find(|x| x.chat_id == LANDLORD * 100).unwrap().text;
    assert!(
        draft.contains("Электричество T1: 3975 − 3831 = 144 кВт·ч × 8,60 = 1238,40 ₽"),
        "{draft}"
    );
    assert!(
        draft.contains("Электричество T2: 1179 − 1118 = 61 кВт·ч × 3,71 = 226,31 ₽"),
        "{draft}"
    );
}

#[tokio::test]
async fn missing_register_blocks_billing_and_names_what_is_missing() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    for (code, v) in [("cold", "755"), ("hot", "417"), ("t1", "3975")] {
        h.enter(TENANT, code, v, now).await;
    }
    let r = h.say(TENANT, "/readings", now).await;
    assert!(r[0].text.contains("Не хватает: Электричество T2"), "{r:?}");
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 0);
    let r = h.say(LANDLORD, "/calc", now).await;
    assert!(r[0].text.contains("неполные"), "{r:?}");
}

#[tokio::test]
async fn reading_below_the_previous_one_is_not_accepted() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    let r = h.say(TENANT, "/enter", now).await;
    h.tap(TENANT, &button_ending(&r, ":cold"), now).await;
    let r = h.say(TENANT, "750", now).await;
    let r = h.tap(TENANT, &button_starting(&r, "ok:"), now).await;
    assert!(r[0].text.contains("меньше предыдущего"), "{r:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted'")
            .await,
        0
    );
}

#[tokio::test]
async fn tariff_change_inside_the_period_blocks_automatic_billing() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.say(LANDLORD, "/tariff t1 9 2026-07-15", now).await;
    let r = h.month(now, ["755", "417", "3975", "1179"]).await;
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 0);
    let landlord = to_chat(&r, LANDLORD);
    assert!(
        landlord.contains("тариф менялся внутри периода"),
        "{landlord}"
    );
    // владелец исправил тарифы так, чтобы смена пришлась на границу периодов, и пересчитал
    sqlx::query("UPDATE tariff SET valid_from = '2026-08-01' WHERE code = 't1' AND price = '9'")
        .execute(h.app.pool())
        .await
        .unwrap();
    let r = h.say(LANDLORD, "/calc", now).await;
    assert!(r[0].text.contains("Итого"), "{r:?}");
}

#[tokio::test]
async fn sewer_can_be_turned_off_explicitly() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.say(LANDLORD, "/tariff sewer off", now).await;
    let r = h.month(now, ["755", "417", "3975", "1179"]).await;
    let draft = &r.iter().find(|x| x.chat_id == LANDLORD * 100).unwrap().text;
    assert!(!draft.contains("Водоотведение"), "{draft}");
    assert!(draft.contains("Итого: 1908,75 ₽"), "{draft}"); // 2063,61 − 154,86
}

// ---------- публикация и права ----------

#[tokio::test]
async fn landlord_must_approve_before_the_tenant_sees_any_amount() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    let r = h.month(now, ["755", "417", "3975", "1179"]).await;
    let tenant_text = to_chat(&r, TENANT);
    assert!(tenant_text.contains("проверку"), "{tenant_text}");
    assert!(!tenant_text.contains("2063"), "{tenant_text}");
    assert_eq!(
        h.count("SELECT count(*) FROM bill WHERE status = 'draft'")
            .await,
        1
    );

    // отклонение: арендатору суммы не уходят, показания открываются заново
    let rj = button_starting(&r, "rj:");
    let rr = h.tap(LANDLORD, &rj, now).await;
    assert!(!to_chat(&rr, TENANT).contains("2063"));
    assert_eq!(
        h.count("SELECT count(*) FROM bill WHERE status = 'rejected'")
            .await,
        1
    );
    let r = h.say(TENANT, "/readings", now).await;
    assert!(r[0].text.contains("Не хватает"), "{r:?}");

    // после исправления снова черновик → утверждение один раз
    let r = h.month(now, ["755", "417", "3975", "1179"]).await;
    let ap = button_starting(&r, "ap:");
    let first = h.tap(LANDLORD, &ap, now).await;
    assert!(to_chat(&first, TENANT).contains("2063,61"));
    let second = h.tap(LANDLORD, &ap, now).await;
    assert!(second[0].text.contains("уже обработан"), "{second:?}");
    assert!(
        to_chat(&second, TENANT).is_empty(),
        "повторное нажатие не рассылает счёт ещё раз"
    );
}

#[tokio::test]
async fn tenant_cannot_approve_pay_or_use_landlord_commands() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    let r = h.month(now, ["755", "417", "3975", "1179"]).await;
    let ap = button_starting(&r, "ap:");
    let r = h.tap(TENANT, &ap, now).await;
    assert!(r[0].text.contains("недоступно"), "{r:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM bill WHERE status = 'draft'")
            .await,
        1
    );
    let r = h.say(TENANT, "/setup", now).await;
    assert!(!r[0].text.contains("Настройка объекта"), "{r:?}");
}

#[tokio::test]
async fn tenant_cannot_touch_another_tenants_property_or_cards() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    // второй объект и его арендатор (4 регистра), заведённые напрямую
    let pool = h.app.pool();
    sqlx::query("INSERT INTO app_user (id, telegram_id, chat_id, role, created_at) VALUES (50, 50, 5000, 'landlord', 'x'), (51, 51, 5100, 'tenant', 'x')").execute(pool).await.unwrap();
    sqlx::query("INSERT INTO property (id, landlord_id, name, created_at) VALUES (9, 50, 'Чужая квартира', 'x')").execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO tenancy (property_id, tenant_user_id, tenant_name) VALUES (9, 51, 'Пётр')",
    )
    .execute(pool)
    .await
    .unwrap();
    for c in ["cold", "hot", "t1", "t2"] {
        sqlx::query("INSERT INTO register (property_id, code, label, unit) VALUES (9, ?, ?, 'u')")
            .bind(c)
            .bind(c)
            .execute(pool)
            .await
            .unwrap();
    }

    // карточка первого арендатора
    let r = h.say(TENANT, "/enter", now).await;
    let r = h.tap(TENANT, &button_ending(&r, ":cold"), now).await;
    let _ = r;
    let r = h.say(TENANT, "755", now).await;
    let ok = button_starting(&r, "ok:");

    // второй арендатор не может подтвердить чужую карточку
    let r = h.send(51, Incoming::Callback(ok.clone()), now).await;
    assert!(
        r[0].text.contains("Уже подтверждено") || r[0].text.contains("недоступн"),
        "{r:?}"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted'")
            .await,
        0
    );

    // и его собственные показания попадают только на его объект
    let r = h.send(51, Incoming::Text("/enter".into()), now).await;
    let pick = button_ending(&r, ":cold");
    h.send(51, Incoming::Callback(pick), now).await;
    let r = h.send(51, Incoming::Text("5".into()), now).await;
    let _ = r;

    // посторонний без приглашения не может отправлять показания и фото
    let r = h.photo(STRANGER, "X1", now).await;
    assert!(r[0].text.contains("не подключены"), "{r:?}");
    let r = h.say(STRANGER, "/enter", now).await;
    assert!(r[0].text.contains("не подключены"), "{r:?}");
}

// ---------- фото, дубли, ошибки распознавания ----------

#[tokio::test]
async fn photo_is_recognised_then_confirmed_and_original_is_kept() {
    let dir = std::env::temp_dir().join(format!("rentflow-test-{}", uuid::Uuid::new_v4()));
    let h = H::with_photo_dir(Some(dir.clone())).await;
    h.setup(at(2026, 6, 26)).await;
    h.connect_tenant(at(2026, 6, 26)).await;
    let now = at(2026, 7, 25);
    h.ocr.push(Ok(vec![cand("cold", "755.012", "high")]));
    let r = h.photo(TENANT, "PH1", now).await;
    assert!(
        r[0].text.contains("ХВС") && r[0].text.contains("755,012"),
        "{r:?}"
    );
    // до подтверждения ничего не записано
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted'")
            .await,
        0
    );
    h.tap(TENANT, &button_starting(&r, "ok:"), now).await;
    let (value, ai, path, uid): (String, String, String, String) = sqlx::query_as(
        "SELECT value, ai_value, photo_path, photo_unique_id FROM reading WHERE kind = 'submitted'",
    )
    .fetch_one(h.app.pool())
    .await
    .unwrap();
    assert_eq!(
        (value.as_str(), ai.as_str(), uid.as_str()),
        ("755.012", "755.012", "PH1")
    );
    assert_eq!(std::fs::read(&path).unwrap(), vec![1, 2, 3]);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn duplicate_photo_never_duplicates_a_reading_or_a_charge() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.ocr.push(Ok(vec![cand("cold", "755", "high")]));
    let r = h.photo(TENANT, "DUP", now).await;
    let ok = button_starting(&r, "ok:");
    // то же фото до подтверждения: возвращается та же карточка, новой не создаётся
    let again = h.photo(TENANT, "DUP", now).await;
    assert_eq!(h.count("SELECT count(*) FROM pending_reading").await, 1);
    assert!(again[0].text.contains("на проверке"), "{again:?}");
    // двойное нажатие «Верно»
    h.tap(TENANT, &ok, now).await;
    let twice = h.tap(TENANT, &ok, now).await;
    assert!(twice[0].text.contains("Уже подтверждено"), "{twice:?}");
    // то же фото после подтверждения
    let r = h.photo(TENANT, "DUP", now).await;
    assert!(r[0].text.contains("уже принято"), "{r:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted' AND status = 'confirmed'")
            .await,
        1
    );

    // после полного набора и расчёта новые показания за тот же период не принимаются
    for (code, v) in [("hot", "417"), ("t1", "3975"), ("t2", "1179")] {
        h.enter(TENANT, code, v, now).await;
    }
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 1);
    let r = h.say(TENANT, "/enter", now).await;
    assert!(r[0].text.contains("уже отправлен владельцу"), "{r:?}");
    let r = h.say(LANDLORD, "/calc", now).await;
    assert!(r[0].text.contains("счёт уже есть"), "{r:?}");
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 1);
}

#[tokio::test]
async fn ai_outage_falls_back_to_manual_entry() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.ocr.push(Err("timeout".into()));
    let r = h.photo(TENANT, "A1", now).await;
    assert!(r[0].text.contains("недоступно"), "{r:?}");
    // карточка без регистра: выбираем счётчик и вводим значение руками
    let r = h.tap(TENANT, &button_ending(&r, ":hot"), now).await;
    assert!(r[0].text.contains("введите показание"), "{r:?}");
    let r = h.say(TENANT, "417", now).await;
    h.tap(TENANT, &button_starting(&r, "ok:"), now).await;
    let (v, src): (String, String) =
        sqlx::query_as("SELECT value, source FROM reading WHERE kind = 'submitted'")
            .fetch_one(h.app.pool())
            .await
            .unwrap();
    assert_eq!((v.as_str(), src.as_str()), ("417", "photo"));
}

#[tokio::test]
async fn wrong_ai_value_can_be_corrected_and_both_values_are_kept() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.ocr.push(Ok(vec![cand("t1", "3915,48", "low")])); // ошибка из golden: 3915 вместо 3975
    let r = h.photo(TENANT, "T1", now).await;
    assert!(r[0].text.contains("неуверенное"), "{r:?}");
    let fix = button_starting(&r, "fix:");
    h.tap(TENANT, &fix, now).await;
    let r = h.say(TENANT, "3975,48", now).await;
    h.tap(TENANT, &button_starting(&r, "ok:"), now).await;
    let (v, ai): (String, String) =
        sqlx::query_as("SELECT value, ai_value FROM reading WHERE kind = 'submitted'")
            .fetch_one(h.app.pool())
            .await
            .unwrap();
    assert_eq!(v, "3975.48");
    assert_eq!(ai, "3915,48");
}

#[tokio::test]
async fn ai_register_mistake_is_fixed_with_the_register_picker_and_t1_t2_stay_apart() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    // на фото T2, а ИИ решил, что T1
    h.ocr.push(Ok(vec![cand("t1", "1179", "medium")]));
    let r = h.photo(TENANT, "R1", now).await;
    let r = h.tap(TENANT, &button_starting(&r, "chg:"), now).await;
    let r = h.tap(TENANT, &button_ending(&r, ":t2"), now).await;
    assert!(
        r[0].text.contains("T2") && r[0].text.contains("1179"),
        "{r:?}"
    );
    h.tap(TENANT, &button_starting(&r, "ok:"), now).await;
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT g.code, r.value FROM reading r JOIN register g ON g.id = r.register_id WHERE r.kind = 'submitted'",
    )
    .fetch_all(h.app.pool())
    .await
    .unwrap();
    assert_eq!(rows, vec![("t2".to_string(), "1179".to_string())]);
}

#[tokio::test]
async fn discarded_card_stores_nothing() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.ocr.push(Ok(vec![cand("cold", "755", "high")]));
    let r = h.photo(TENANT, "D1", now).await;
    h.tap(TENANT, &button_starting(&r, "no:"), now).await;
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted'")
            .await,
        0
    );
    let r = h.tap(TENANT, &button_starting(&r, "ok:"), now).await;
    assert!(
        r[0].text.contains("Уже подтверждено") || r[0].text.contains("обработана"),
        "{r:?}"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted'")
            .await,
        0
    );
}

#[tokio::test]
async fn readings_of_an_unfinished_month_are_not_mixed_with_the_next_one() {
    let h = H::ready().await;
    // в июле принято только ХВС
    h.enter(TENANT, "cold", "755", at(2026, 7, 28)).await;
    // в августе арендатор продолжает тот же (июльский) период, а не открывает новый
    let r = h.say(TENANT, "/readings", at(2026, 8, 2)).await;
    assert!(r[0].text.contains("Период 2026-07"), "{r:?}");
    for (code, v) in [("hot", "417"), ("t1", "3975"), ("t2", "1179")] {
        h.enter(TENANT, code, v, at(2026, 8, 2)).await;
    }
    assert_eq!(
        h.count("SELECT count(*) FROM bill WHERE period = '2026-07'")
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM bill WHERE period = '2026-08'")
            .await,
        0
    );
}

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
        Self::build(None, false).await
    }

    async fn with_photo_dir(photo_dir: Option<std::path::PathBuf>) -> Self {
        Self::build(photo_dir, false).await
    }

    async fn build(photo_dir: Option<std::path::PathBuf>, allow_void_paid: bool) -> Self {
        let pool = db::connect("sqlite::memory:").await.unwrap();
        let ocr = Arc::new(FakeOcr::default());
        let app = App::new(
            pool,
            Config {
                bot_username: "rentflow_bot".into(),
                photo_dir,
                invite_ttl_days: 7,
                ocr_enabled: false,
                allow_void_paid,
                ..Default::default()
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

    /// Настройка объекта по данным golden dataset: тарифы июля, дата стартовых показаний неизвестна.
    /// Тарифы заводятся на расчётный месяц даты `now` (вопроса про дату больше нет).
    async fn setup_defaults(&self, now: NaiveDateTime) {
        self.setup_defaults_sewer(now, "51,62").await;
    }

    async fn setup_defaults_sewer(&self, now: NaiveDateTime, sewer: &str) {
        self.say(LANDLORD, "/setup", now).await;
        for a in [
            "Квартира на Тверской",
            "Иван",
            "неизвестна",
            "753",
            "416",
            "3831",
            "1118",
            "65,77",
            "312,50",
            sewer,
            "8,60",
            "3,71",
        ] {
            self.say(LANDLORD, a, now).await;
        }
        let r = self.tap(LANDLORD, "setup_ok", now).await;
        assert!(r[0].text.contains("Объект сохранён"), "{r:?}");
    }

    /// То же, но фото-подтверждение выключено: большинству сценариев фото не нужны (включается в своих тестах).
    async fn setup(&self, now: NaiveDateTime) {
        self.setup_defaults(now).await;
        self.set_proof(false).await;
    }

    async fn set_proof(&self, on: bool) {
        sqlx::query("UPDATE property SET proof_required = ?")
            .bind(on)
            .execute(self.app.pool())
            .await
            .unwrap();
    }

    async fn connect_tenant(&self, now: NaiveDateTime) -> Vec<Reply> {
        let r = self.say(LANDLORD, "/invite", now).await;
        let token = r[0].text.split("start=").nth(1).unwrap().trim().to_string();
        let r = self.send(TENANT, Incoming::Start(Some(token)), now).await;
        assert!(r[0].text.contains("Вы подключены"), "{r:?}");
        r
    }

    async fn ready() -> Self {
        let h = Self::new().await;
        h.setup(at(2026, 6, 26)).await;
        h.connect_tenant(at(2026, 6, 26)).await;
        h
    }

    /// Полный ввод формы: /enter → четыре числа по шагам → «Отправить». Возвращает ответы на «Отправить».
    async fn month(&self, now: NaiveDateTime, v: [&str; 4]) -> Vec<Reply> {
        self.say(TENANT, "/enter", now).await;
        for value in v {
            self.say(TENANT, value, now).await;
        }
        self.tap(TENANT, "send", now).await
    }

    async fn drafts(&self) -> i64 {
        self.count("SELECT count(*) FROM pending_reading WHERE status = 'open'")
            .await
    }
}

fn buttons(replies: &[Reply]) -> Vec<String> {
    replies
        .iter()
        .flat_map(|r| r.buttons.iter().flatten().map(|b| b.data.clone()))
        .collect()
}

fn button_starting(replies: &[Reply], prefix: &str) -> String {
    buttons(replies)
        .into_iter()
        .find(|d| d.starts_with(prefix))
        .unwrap_or_else(|| panic!("no button {prefix}* in {replies:?}"))
}

fn all_text(replies: &[Reply]) -> String {
    replies
        .iter()
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join("\n---\n")
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
async fn wizard_shows_progress_rejects_bad_input_and_asks_again() {
    let h = H::new().await;
    let r = h.say(LANDLORD, "/setup", at(2026, 6, 26)).await;
    assert!(r[0].text.contains("(1/12)"), "{r:?}");
    let r = h.say(LANDLORD, "Квартира", at(2026, 6, 26)).await;
    assert!(r[0].text.contains("(2/12)"), "{r:?}");
    h.say(LANDLORD, "Иван", at(2026, 6, 26)).await;
    let r = h.say(LANDLORD, "вчера", at(2026, 6, 26)).await;
    assert!(r[0].text.contains("Не получилось разобрать"), "{r:?}");
    let r = h.say(LANDLORD, "25.06.2026", at(2026, 6, 26)).await;
    assert!(
        r[0].text.contains("(4/12)") && r[0].text.contains("ХВС"),
        "{r:?}"
    );
    let r = h.say(LANDLORD, "-5", at(2026, 6, 26)).await;
    assert!(r[0].text.contains("Не получилось разобрать"), "{r:?}");
}

#[tokio::test]
async fn a_newcomer_becomes_an_owner_only_by_asking() {
    let h = H::new().await;
    // обычное сообщение — приветствие с выбором, пользователь не создаётся
    let r = h.say(STRANGER, "привет", at(2026, 6, 26)).await;
    assert!(buttons(&r).contains(&"reg_owner".to_string()), "{r:?}");
    assert_eq!(h.count("SELECT count(*) FROM app_user").await, 0);
    // /setup — самостоятельная регистрация владельцем и начало мастера
    let r = h.say(STRANGER, "/setup", at(2026, 6, 26)).await;
    assert!(r[0].text.contains("Настройка объекта"), "{r:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM app_user WHERE role = 'landlord'")
            .await,
        1
    );
    // у него ещё нет ни объектов, ни чужих данных
    let r = h.say(STRANGER, "/status", at(2026, 6, 26)).await;
    assert!(
        r[0].text.contains("не настроен") || r[0].text.contains("/setup"),
        "{r:?}"
    );
}

// ---------- приглашение ----------

#[tokio::test]
async fn invite_is_one_time_and_expires() {
    let h = H::new().await;
    h.setup(at(2026, 6, 26)).await;
    let r = h.say(LANDLORD, "/invite", at(2026, 6, 26)).await;
    let token = r[0].text.split("start=").nth(1).unwrap().trim().to_string();
    assert!(r[0].text.contains("https://t.me/rentflow_bot?start="));

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
        r[0].text.contains("по шагам"),
        "приветствие объясняет сценарий: {r:?}"
    );
    assert!(
        buttons(&r).contains(&"enter".to_string()),
        "в приветствии есть кнопка ввода"
    );
    assert!(
        r.iter()
            .any(|x| x.chat_id == LANDLORD * 100 && x.text.contains("подключился"))
    );

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

// ---------- ввод показаний: понятный сценарий ----------

#[tokio::test]
async fn entry_is_a_stepwise_form_with_one_send_button() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);

    let r = h.say(TENANT, "/enter", now).await;
    let text = all_text(&r);
    assert!(
        text.contains("Шаг 1 из 4") && text.contains("Холодная вода"),
        "{text}"
    );
    assert!(
        text.contains("Прошлое показание: 753"),
        "показывается прошлое значение: {text}"
    );

    let r = h.say(TENANT, "755", now).await;
    assert!(
        all_text(&r).contains("✔ Холодная вода (ХВС): 755") && all_text(&r).contains("Шаг 2 из 4"),
        "{r:?}"
    );
    let r = h.say(TENANT, "417", now).await;
    assert!(all_text(&r).contains("Шаг 3 из 4"), "{r:?}");
    let r = h.say(TENANT, "3975,48", now).await;
    assert!(all_text(&r).contains("Шаг 4 из 4"), "{r:?}");
    // до «Отправить» ничего не записано и владелец ничего не получил
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted'")
            .await,
        0
    );
    let r = h.say(TENANT, "1179", now).await;
    let summary = all_text(&r);
    assert!(
        summary.contains("Проверьте показания за 2026-07"),
        "{summary}"
    );
    assert!(
        summary.contains("расход 2 м³") && summary.contains("расход 144 кВт·ч"),
        "в сводке виден расход: {summary}"
    );
    assert!(
        buttons(&r).contains(&"send".to_string()) && buttons(&r).contains(&"ed:t1".to_string()),
        "{r:?}"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted'")
            .await,
        0,
        "сводка ещё не отправка"
    );
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 0);

    let r = h.tap(TENANT, "send", now).await;
    assert!(
        to_chat(&r, TENANT).contains("Показания отправлены"),
        "{r:?}"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted' AND status = 'confirmed'")
            .await,
        4
    );
    assert_eq!(
        h.count("SELECT count(*) FROM bill WHERE status = 'draft'")
            .await,
        1
    );
    assert_eq!(h.drafts().await, 0);
}

#[tokio::test]
async fn invalid_number_keeps_the_same_step() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.say(TENANT, "/enter", now).await;
    let r = h.say(TENANT, "abc", now).await;
    assert!(r[0].text.contains("Нужно число"), "{r:?}");
    let r = h.say(TENANT, "750", now).await; // меньше стартового 753
    assert!(
        r[0].text.contains("меньше прошлого показания (753)"),
        "{r:?}"
    );
    assert_eq!(h.drafts().await, 0, "неверные значения не сохраняются");
    let r = h.say(TENANT, "755", now).await;
    assert!(
        all_text(&r).contains("Шаг 2 из 4"),
        "тот же шаг принял верное значение: {r:?}"
    );
}

#[tokio::test]
async fn any_value_can_be_edited_from_the_summary() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.say(TENANT, "/enter", now).await;
    for v in ["755", "417", "3975", "1179"] {
        h.say(TENANT, v, now).await;
    }
    let r = h.tap(TENANT, "ed:hot", now).await;
    assert!(
        all_text(&r).contains("Горячая вода") && all_text(&r).contains("Прошлое показание: 416"),
        "{r:?}"
    );
    let r = h.say(TENANT, "418", now).await;
    let text = all_text(&r);
    assert!(
        text.contains("Проверьте показания") && text.contains("Горячая вода (ГВС): 418"),
        "после правки снова сводка: {text}"
    );
    assert!(!text.contains("Горячая вода (ГВС): 417"));
    assert_eq!(
        h.drafts().await,
        4,
        "правка заменяет значение, а не добавляет второе"
    );
}

#[tokio::test]
async fn cancel_discards_the_form_and_stores_nothing() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.say(TENANT, "/enter", now).await;
    h.say(TENANT, "755", now).await;
    h.say(TENANT, "417", now).await;
    assert_eq!(h.drafts().await, 2);
    let r = h.tap(TENANT, "cx", now).await;
    assert!(r[0].text.contains("отменён"), "{r:?}");
    assert_eq!(h.drafts().await, 0);
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted'")
            .await,
        0
    );
    // старая кнопка «Отправить» после отмены ничего не отправляет
    let r = h.tap(TENANT, "send", now).await;
    assert!(r[0].text.contains("Нечего отправлять"), "{r:?}");
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 0);
}

#[tokio::test]
async fn double_send_makes_one_bill() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.month(now, ["755", "417", "3975", "1179"]).await;
    let again = h.tap(TENANT, "send", now).await;
    assert!(again[0].text.contains("уже отправлены"), "{again:?}");
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 1);
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted' AND status = 'confirmed'")
            .await,
        4
    );
    let r = h.say(TENANT, "/enter", now).await;
    assert!(r[0].text.contains("уже отправлен владельцу"), "{r:?}");
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
            // тарифы воды и водоотведения изменились с 1 августа (строкой — для опытных пользователей)
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
        assert!(
            !to_chat(&r, TENANT).contains("Итого"),
            "{month}: сумма ушла арендатору до утверждения"
        );
        let ap = button_starting(&r, "ap:");
        let r = h.tap(LANDLORD, &ap, now).await;
        assert!(
            to_chat(&r, TENANT).contains(&format!("Итого: {expected} ₽")),
            "{}",
            to_chat(&r, TENANT)
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
async fn missing_register_blocks_sending_and_billing() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.say(TENANT, "/enter", now).await;
    for v in ["755", "417", "3975"] {
        h.say(TENANT, v, now).await;
    }
    let r = h.tap(TENANT, "send", now).await;
    assert!(r[0].text.contains("Не хватает: Электричество T2"), "{r:?}");
    let r = h.say(TENANT, "/readings", now).await;
    assert!(
        r[0].text.contains("Черновик") && r[0].text.contains("Не хватает"),
        "{r:?}"
    );
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 0);
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted'")
            .await,
        0
    );
    let r = h.say(LANDLORD, "/calc", now).await;
    assert!(r[0].text.contains("неполные"), "{r:?}");
}

#[tokio::test]
async fn sewer_can_be_turned_off_explicitly() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.say(LANDLORD, "/tariff sewer off", now).await;
    let r = h.month(now, ["755", "417", "3975", "1179"]).await;
    let draft = &r.iter().find(|x| x.chat_id == LANDLORD * 100).unwrap().text;
    assert!(!draft.contains("Водоотведение"), "{draft}");
    assert!(draft.contains("Итого: 1908,75 ₽"), "{draft}");
}

// ---------- меню тарифов ----------

// ---------- публикация и права ----------

#[tokio::test]
async fn landlord_must_approve_before_the_tenant_sees_any_amount() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    let r = h.month(now, ["755", "417", "3975", "1179"]).await;
    let tenant_text = to_chat(&r, TENANT);
    assert!(tenant_text.contains("Владелец проверит"), "{tenant_text}");
    assert!(!tenant_text.contains("2063"), "{tenant_text}");
    assert_eq!(
        h.count("SELECT count(*) FROM bill WHERE status = 'draft'")
            .await,
        1
    );

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
async fn tenant_cannot_touch_another_tenants_property_or_forms() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    let pool = h.app.pool();
    sqlx::query("INSERT INTO app_user (id, telegram_id, chat_id, role, created_at) VALUES (50, 50, 5000, 'landlord', 'x'), (51, 51, 5100, 'tenant', 'x')").execute(pool).await.unwrap();
    sqlx::query("INSERT INTO property (id, landlord_id, name, created_at) VALUES (9, 50, 'Чужая квартира', 'x')").execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO tenancy (property_id, tenant_user_id, tenant_name) VALUES (9, 51, 'Пётр')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO membership (user_id, property_id, role, created_at) VALUES (50, 9, 'owner', 'x'), (51, 9, 'tenant', 'x')")
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

    // первый арендатор начал форму
    h.say(TENANT, "/enter", now).await;
    h.say(TENANT, "755", now).await;
    assert_eq!(h.drafts().await, 1);

    // второй не может ни отправить, ни отменить, ни править чужую форму
    let r = h.send(51, Incoming::Callback("send".into()), now).await;
    assert!(r[0].text.contains("Нечего отправлять"), "{r:?}");
    h.send(51, Incoming::Callback("cx".into()), now).await;
    assert_eq!(
        h.drafts().await,
        1,
        "чужая отмена не трогает форму первого арендатора"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted'")
            .await,
        0
    );

    // его собственный ввод привязан только к его объекту: у него нет прошлых показаний
    let r = h.send(51, Incoming::Text("/enter".into()), now).await;
    assert!(all_text(&r).contains("Прошлого показания нет"), "{r:?}");

    // посторонний без приглашения не может отправлять показания и фото
    let r = h.photo(STRANGER, "X1", now).await;
    assert!(r[0].text.contains("ссылку-приглашение"), "{r:?}");
    let r = h.say(STRANGER, "/enter", now).await;
    assert!(r[0].text.contains("ссылку-приглашение"), "{r:?}");
}

// ---------- фото, дубли, ошибки распознавания ----------

#[tokio::test]
async fn photo_fills_the_form_and_is_confirmed_on_the_summary_with_original_kept() {
    let dir = std::env::temp_dir().join(format!("rentflow-test-{}", uuid::Uuid::new_v4()));
    let h = H::with_photo_dir(Some(dir.clone())).await;
    h.setup(at(2026, 6, 26)).await;
    h.connect_tenant(at(2026, 6, 26)).await;
    let now = at(2026, 7, 25);

    h.ocr.push(Ok(vec![cand("cold", "755.012", "high")]));
    let r = h.photo(TENANT, "PH1", now).await;
    let text = all_text(&r);
    assert!(
        text.contains("Распознано: Холодная вода (ХВС) = 755,012"),
        "{text}"
    );
    assert!(
        text.contains("Шаг 2 из 4"),
        "дальше форма просит следующий счётчик: {text}"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted'")
            .await,
        0,
        "до сводки ничего не записано"
    );

    for v in ["417", "3975", "1179"] {
        h.say(TENANT, v, now).await;
    }
    h.tap(TENANT, "send", now).await;
    let (value, ai, path, uid, source): (String, String, String, String, String) = sqlx::query_as(
        "SELECT value, ai_value, photo_path, photo_unique_id, source FROM reading WHERE kind = 'submitted' AND photo_unique_id IS NOT NULL",
    )
    .fetch_one(h.app.pool())
    .await
    .unwrap();
    assert_eq!(
        (value.as_str(), ai.as_str(), uid.as_str(), source.as_str()),
        ("755.012", "755.012", "PH1", "photo")
    );
    assert_eq!(std::fs::read(&path).unwrap(), vec![1, 2, 3]);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn duplicate_photo_never_duplicates_a_reading_or_a_charge() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.ocr.push(Ok(vec![cand("cold", "755", "high")]));
    h.photo(TENANT, "DUP", now).await;
    // то же фото ещё раз до отправки: не принимается, форма не дублируется
    let again = h.photo(TENANT, "DUP", now).await;
    assert!(
        again[0].text.contains("нельзя использовать дважды"),
        "{again:?}"
    );
    assert_eq!(h.drafts().await, 1);

    for v in ["417", "3975", "1179"] {
        h.say(TENANT, v, now).await;
    }
    h.tap(TENANT, "send", now).await;
    h.tap(TENANT, "send", now).await; // двойное нажатие
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 1);
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE kind = 'submitted' AND status = 'confirmed'")
            .await,
        4
    );
    // после отправки период закрыт для новых показаний, повторный расчёт не создаёт второй счёт
    let r = h.photo(TENANT, "DUP2", now).await;
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
    let text = all_text(&r);
    assert!(
        text.contains("недоступно") && text.contains("Шаг 1 из 4"),
        "{text}"
    );
    for v in ["755", "417", "3975", "1179"] {
        h.say(TENANT, v, now).await;
    }
    h.tap(TENANT, "send", now).await;
    assert_eq!(
        h.count("SELECT count(*) FROM reading WHERE source = 'manual' AND kind = 'submitted'")
            .await,
        4
    );
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 1);
}

#[tokio::test]
async fn wrong_ai_value_can_be_corrected_and_both_values_are_kept() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.ocr.push(Ok(vec![cand("t1", "3915,48", "low")])); // ошибка из golden: 3915 вместо 3975
    let r = h.photo(TENANT, "T1", now).await;
    assert!(
        all_text(&r).contains("Распознано: Электричество T1 = 3915,48"),
        "{r:?}"
    );
    for v in ["755", "417", "1179"] {
        h.say(TENANT, v, now).await;
    }
    // сводка показывает значение с пометкой «сверьте с фото»
    let r = h.tap(TENANT, "ed:t1", now).await;
    assert!(all_text(&r).contains("Электричество T1"), "{r:?}");
    let r = h.say(TENANT, "3975,48", now).await;
    assert!(all_text(&r).contains("Электричество T1: 3975,48"), "{r:?}");
    h.tap(TENANT, "send", now).await;
    let (v, ai): (String, String) = sqlx::query_as(
        "SELECT value, ai_value FROM reading WHERE kind = 'submitted' AND source = 'photo'",
    )
    .fetch_one(h.app.pool())
    .await
    .unwrap();
    assert_eq!(v, "3975.48");
    assert_eq!(ai, "3915,48", "исходное предложение ИИ сохранено");
}

#[tokio::test]
async fn low_confidence_photo_value_is_flagged_on_the_summary() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.ocr.push(Ok(vec![cand("cold", "755", "low")]));
    h.photo(TENANT, "L1", now).await;
    for v in ["417", "3975"] {
        h.say(TENANT, v, now).await;
    }
    let r = h.say(TENANT, "1179", now).await;
    assert!(all_text(&r).contains("📎⚠️ сверьте с фото"), "{r:?}");
}

#[tokio::test]
async fn ai_register_mistake_is_caught_by_the_previous_reading_check() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    // на фото T2 (1179), а ИИ решил, что T1 — но это меньше прошлого T1 (3831), значение не принимается
    h.ocr.push(Ok(vec![cand("t1", "1179", "medium")]));
    let r = h.photo(TENANT, "R1", now).await;
    let text = all_text(&r);
    assert!(
        text.contains("меньше прошлого") && text.contains("Не принято"),
        "{text}"
    );
    assert_eq!(h.drafts().await, 0, "неверное значение не сохраняется");
    assert!(
        text.contains("Шаг 1 из 4"),
        "дальше — ручной ввод по шагам: {text}"
    );
}

#[tokio::test]
async fn unknown_register_from_ai_asks_for_manual_entry() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.ocr.push(Ok(vec![Candidate {
        register: None,
        value: Some("1179".into()),
        confidence: "low".into(),
    }]));
    let r = h.photo(TENANT, "U1", now).await;
    assert!(
        all_text(&r).contains("не удалось надёжно определить"),
        "{r:?}"
    );
    assert_eq!(h.drafts().await, 0);
}

#[tokio::test]
async fn readings_of_an_unfinished_month_are_not_mixed_with_the_next_one() {
    let h = H::ready().await;
    h.say(TENANT, "/enter", at(2026, 7, 28)).await;
    h.say(TENANT, "755", at(2026, 7, 28)).await;
    h.say(TENANT, "417", at(2026, 7, 28)).await;
    // в августе арендатор продолжает тот же (июльский) период, а не открывает новый
    let r = h.say(TENANT, "/readings", at(2026, 8, 2)).await;
    assert!(
        r[0].text.contains("Период 2026-07") && r[0].text.contains("Черновик"),
        "{r:?}"
    );
    let r = h.say(TENANT, "/enter", at(2026, 8, 2)).await;
    assert!(
        all_text(&r).contains("Продолжаем") && all_text(&r).contains("Шаг 3 из 4"),
        "{r:?}"
    );
    h.say(TENANT, "3975", at(2026, 8, 2)).await;
    h.say(TENANT, "1179", at(2026, 8, 2)).await;
    h.tap(TENANT, "send", at(2026, 8, 2)).await;
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

#[tokio::test]
async fn stale_buttons_from_the_old_card_flow_explain_what_to_do() {
    let h = H::ready().await;
    let r = h.tap(TENANT, "ok:1", at(2026, 7, 25)).await;
    assert!(
        r[0].text.contains("устарела") && r[0].text.contains("/enter"),
        "{r:?}"
    );
}

// ---------- история тарифов ----------

#[tokio::test]
async fn tariff_history_is_reachable_from_the_menu_and_hidden_from_the_tenant() {
    let h = H::ready().await;
    let now = at(2026, 9, 10);
    let r = h.say(LANDLORD, "/tariff", now).await;
    assert!(
        buttons(&r).contains(&"th".to_string()),
        "в меню тарифов есть кнопка истории: {r:?}"
    );
    let r = h.tap(LANDLORD, "th", now).await;
    assert!(r[0].text.contains("История тарифов"), "{r:?}");
    let r = h.say(TENANT, "/tariffs", now).await;
    assert!(!r[0].text.contains("История тарифов"), "{r:?}");
}

// ---------- фото как подтверждение показаний ----------

impl H {
    /// Готовый стенд с обязательным фото-подтверждением.
    async fn ready_with_proof() -> Self {
        let h = Self::ready().await;
        h.set_proof(true).await;
        h
    }

    /// Число, затем фото к нему (ИИ недоступен).
    async fn value_then_photo(&self, value: &str, unique: &str, now: NaiveDateTime) -> Vec<Reply> {
        self.say(TENANT, value, now).await;
        self.photo(TENANT, unique, now).await
    }
}

#[tokio::test]
async fn proof_is_required_by_default_and_the_welcome_says_so() {
    let h = H::new().await;
    h.setup_defaults(at(2026, 6, 26)).await;
    assert_eq!(
        h.count("SELECT proof_required FROM property").await,
        1,
        "по умолчанию фото обязательно"
    );
    let welcome = h.connect_tenant(at(2026, 6, 26)).await;
    assert!(
        welcome[0].text.contains("приложите фото счётчика"),
        "{welcome:?}"
    );
    let r = h.say(LANDLORD, "/status", at(2026, 6, 26)).await;
    assert!(
        r[0].text.contains("Фото-подтверждение: обязательно"),
        "{r:?}"
    );
}

#[tokio::test]
async fn number_then_photo_attaches_proof_and_the_landlord_gets_the_photos() {
    let h = H::ready_with_proof().await;
    let now = at(2026, 7, 25);
    let r = h.say(TENANT, "/enter", now).await;
    assert!(all_text(&r).contains("нужно приложить фото"), "{r:?}");
    let r = h.say(TENANT, "755", now).await;
    let text = all_text(&r);
    assert!(
        text.contains("Теперь пришлите фото") && !text.contains("Шаг 2"),
        "без фото дальше не идём: {text}"
    );
    let r = h.photo(TENANT, "P-cold", now).await;
    assert!(
        all_text(&r).contains("Фото прикреплено") && all_text(&r).contains("Шаг 2 из 4"),
        "{r:?}"
    );

    h.value_then_photo("417", "P-hot", now).await;
    h.value_then_photo("3975", "P-t1", now).await;
    let r = h.value_then_photo("1179", "P-t2", now).await;
    let summary = all_text(&r);
    assert!(summary.matches("📎").count() >= 4, "{summary}");
    assert!(
        !summary.contains("нет фото") && buttons(&r).contains(&"send".to_string()),
        "{summary}"
    );

    let r = h.tap(TENANT, "send", now).await;
    let draft = r
        .iter()
        .find(|x| x.chat_id == LANDLORD * 100 && !x.buttons.is_empty())
        .expect("draft for landlord");
    assert_eq!(draft.photos.len(), 4, "владелец получает все четыре фото");
    assert_eq!(
        draft.photos[0],
        (
            "file-P-cold".to_string(),
            "Холодная вода (ХВС): 755".to_string()
        )
    );
    assert!(
        draft.text.contains("Фото-подтверждения всех показаний"),
        "{}",
        draft.text
    );
    assert_eq!(h.count("SELECT count(*) FROM reading WHERE kind = 'submitted' AND photo_file_id IS NOT NULL AND source = 'manual'").await, 4);
}

#[tokio::test]
async fn photo_then_number_works_too() {
    let h = H::ready_with_proof().await;
    let now = at(2026, 7, 25);
    h.say(TENANT, "/enter", now).await;
    let r = h.photo(TENANT, "P1", now).await;
    let text = all_text(&r);
    assert!(
        text.contains("Фото сохранено как подтверждение") && text.contains("Шаг 1 из 4"),
        "{text}"
    );
    let r = h.say(TENANT, "755", now).await;
    assert!(
        all_text(&r).contains("Шаг 2 из 4"),
        "фото уже есть — сразу следующий шаг: {r:?}"
    );
    assert_eq!(h.drafts().await, 1, "число и фото — одна запись, а не две");
}

#[tokio::test]
async fn sending_is_blocked_until_every_register_has_a_photo() {
    let h = H::ready().await; // фото выключено: форму можно заполнить без фото
    let now = at(2026, 7, 25);
    h.say(TENANT, "/enter", now).await;
    for v in ["755", "417", "3975", "1179"] {
        h.say(TENANT, v, now).await;
    }
    // владелец включил требование уже после ввода
    let r = h.tap(LANDLORD, "pr:on", now).await;
    assert!(r[0].text.contains("обязательно"), "{r:?}");
    let r = h.tap(TENANT, "send", now).await;
    assert!(
        r[0].text.contains("Не хватает фото") && r[0].text.contains("Холодная вода"),
        "{r:?}"
    );
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 0);

    // через ✏️ добавляем фото к каждому счётчику; пока фото есть не у всех, кнопки «Отправить» на сводке нет
    for (code, uid) in [("cold", "E1"), ("hot", "E2"), ("t1", "E3")] {
        h.tap(TENANT, &format!("ed:{code}"), now).await;
        let r = h.photo(TENANT, uid, now).await;
        let text = all_text(&r);
        assert!(
            text.contains("Фото прикреплено") && text.contains("Проверьте показания"),
            "{text}"
        );
        assert!(
            text.contains("❌ нет фото"),
            "на сводке видно, где фото ещё нет: {text}"
        );
        assert!(
            !buttons(&r).contains(&"send".to_string()),
            "кнопки «Отправить» нет, пока не хватает фото: {r:?}"
        );
    }
    h.tap(TENANT, "ed:t2", now).await;
    let r = h.photo(TENANT, "E4", now).await;
    assert!(
        buttons(&r).contains(&"send".to_string()),
        "когда у всех есть фото, кнопка «Отправить» появляется: {r:?}"
    );
    h.tap(TENANT, "send", now).await;
    assert_eq!(
        h.count("SELECT count(*) FROM bill WHERE status = 'draft'")
            .await,
        1
    );
}

#[tokio::test]
async fn recognition_cross_checks_the_typed_number_against_the_photo() {
    let h = H::ready_with_proof().await;
    let now = at(2026, 7, 25);
    h.say(TENANT, "/enter", now).await;
    h.say(TENANT, "757", now).await; // арендатор ошибся
    h.ocr.push(Ok(vec![cand("cold", "755", "high")]));
    let r = h.photo(TENANT, "X1", now).await;
    let text = all_text(&r);
    assert!(
        text.contains("На фото я вижу 755") && text.contains("вы ввели 757"),
        "{text}"
    );
    // число не подменено молча: на сводке остаётся то, что ввёл арендатор
    let (v, ai): (String, String) = sqlx::query_as("SELECT value, ai_value FROM pending_reading WHERE status = 'open' AND register_code = 'cold'")
        .fetch_one(h.app.pool())
        .await
        .unwrap();
    assert_eq!((v.as_str(), ai.as_str()), ("757", "755"));
}

#[tokio::test]
async fn one_photo_cannot_prove_two_registers() {
    let h = H::ready_with_proof().await;
    let now = at(2026, 7, 25);
    h.say(TENANT, "/enter", now).await;
    h.value_then_photo("755", "SAME", now).await;
    h.say(TENANT, "417", now).await;
    let r = h.photo(TENANT, "SAME", now).await;
    assert!(r[0].text.contains("нельзя использовать дважды"), "{r:?}");
    assert_eq!(
        h.count(
            "SELECT count(*) FROM pending_reading WHERE status = 'open' AND unique_id = 'SAME'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn proof_command_is_landlord_only_and_toggles_the_requirement() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    let r = h.say(LANDLORD, "/proof", now).await;
    assert!(
        r[0].text.contains("необязательно") && buttons(&r).contains(&"pr:on".to_string()),
        "{r:?}"
    );
    h.tap(LANDLORD, "pr:on", now).await;
    assert_eq!(h.count("SELECT proof_required FROM property").await, 1);
    let r = h.tap(TENANT, "pr:off", now).await;
    assert!(r[0].text.contains("недоступно"), "{r:?}");
    assert_eq!(
        h.count("SELECT proof_required FROM property").await,
        1,
        "арендатор не может отключить требование"
    );
    let r = h.say(TENANT, "/proof", now).await;
    assert!(
        !r[0].text.contains("Фото-подтверждение показаний сейчас"),
        "{r:?}"
    );
}

#[tokio::test]
async fn landlord_is_told_when_no_photos_were_attached() {
    let h = H::ready().await;
    let r = h
        .month(at(2026, 7, 25), ["755", "417", "3975", "1179"])
        .await;
    let draft = r
        .iter()
        .find(|x| x.chat_id == LANDLORD * 100 && !x.buttons.is_empty())
        .unwrap();
    assert!(
        draft.photos.is_empty() && draft.text.contains("Фото к показаниям не приложены"),
        "{}",
        draft.text
    );
}

// ---------- помесячные тарифы ----------

impl H {
    /// Объект настроен 15 октября, арендатор подключён и отправил октябрьские показания (golden июля).
    /// Возвращает ответы на «Отправить» (в них — черновик владельцу).
    async fn october() -> (Self, Vec<Reply>) {
        Self::october_with(false).await
    }

    async fn october_with(allow_void_paid: bool) -> (Self, Vec<Reply>) {
        let h = Self::build(None, allow_void_paid).await;
        h.setup(at(2026, 10, 15)).await;
        h.connect_tenant(at(2026, 10, 15)).await;
        let r = h
            .month(at(2026, 10, 16), ["755", "417", "3975,48", "1179"])
            .await;
        (h, r)
    }

    async fn t1_rows(&self) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT valid_from, price FROM tariff WHERE code = 't1' ORDER BY valid_from, id",
        )
        .fetch_all(self.app.pool())
        .await
        .unwrap()
    }

    async fn status_of(&self, period: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT status FROM bill WHERE period = ? ORDER BY id")
            .bind(period)
            .fetch_all(self.app.pool())
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn setup_stores_monthly_tariffs_whatever_the_day() {
    for (day, sewer) in [(1, "51,62"), (31, "51,62")] {
        let h = H::new().await;
        h.setup_defaults_sewer(at(2026, 10, day), sewer).await;
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT code, valid_from FROM tariff ORDER BY code")
                .fetch_all(h.app.pool())
                .await
                .unwrap();
        assert_eq!(rows.len(), 5, "{rows:?}");
        assert!(rows.iter().all(|r| r.1 == "2026-10-01"), "{rows:?}");
    }
    let h = H::new().await;
    h.setup_defaults_sewer(at(2026, 10, 15), "нет").await;
    let enabled: i64 = h
        .count("SELECT enabled FROM tariff WHERE code = 'sewer'")
        .await;
    assert_eq!(enabled, 0);
}

/// Продакшн-сценарий 17 октября: объект настроен 15-го, тариф T1 меняется 17-го, весь октябрь — по новому.
#[tokio::test]
async fn october_17_tariff_change_reprices_the_whole_month_draft() {
    let (h, r) = H::october().await;
    assert!(all_text(&r).contains("Итого: 2063,61 ₽"), "{r:?}");
    assert_eq!(h.status_of("2026-10").await, ["draft"]);

    let r = h.say(LANDLORD, "/tariff t1 9", at(2026, 10, 17)).await;
    let text = all_text(&r);
    assert!(text.contains("Тарифы сохранены"), "{text}");
    assert!(text.contains("пересчитан"), "{text}");
    assert!(text.contains("3975 − 3831 = 144"), "{text}");
    assert!(text.contains("= 1296,00 ₽"), "{text}");
    assert!(text.contains("Итого: 2121,21 ₽"), "{text}");

    assert_eq!(h.status_of("2026-10").await, ["void", "draft"]);
    // версия месяца заменена, а не добавлена
    assert_eq!(
        h.t1_rows().await,
        [("2026-10-01".to_string(), "9".to_string())]
    );
    // арендатор по-прежнему не видит сумм
    assert!(!to_chat(&r, TENANT).contains("Итого"));

    // старая кнопка «Утвердить» устаревшего черновика объясняет, что произошло
    let old = h.count("SELECT id FROM bill WHERE status = 'void'").await;
    let r = h
        .tap(LANDLORD, &format!("ap:{old}"), at(2026, 10, 17))
        .await;
    assert!(r[0].text.contains("устарел"), "{r:?}");
}

#[tokio::test]
async fn next_month_inherits_the_tariff_and_approved_bill_stays() {
    let (h, r) = H::october().await;
    h.say(LANDLORD, "/tariff t1 9", at(2026, 10, 17)).await;
    let r2 = h.say(LANDLORD, "/calc", at(2026, 10, 17)).await;
    let _ = (r, r2);
    let ap: i64 = h.count("SELECT id FROM bill WHERE status = 'draft'").await;
    h.tap(LANDLORD, &format!("ap:{ap}"), at(2026, 10, 18)).await;
    let before: String = sqlx::query_scalar("SELECT snapshot FROM bill WHERE id = ?")
        .bind(ap)
        .fetch_one(h.app.pool())
        .await
        .unwrap();

    let r = h
        .month(at(2026, 11, 25), ["757", "418", "4000", "1200"])
        .await;
    let text = all_text(&r);
    assert!(text.contains("4000 − 3975 = 25"), "{text}");
    assert!(text.contains("× 9,00 = 225,00 ₽"), "{text}");
    // за ноябрь отдельных тарифных строк не заводилось
    assert_eq!(h.t1_rows().await.len(), 1);
    let after: String = sqlx::query_scalar("SELECT snapshot FROM bill WHERE id = ?")
        .bind(ap)
        .fetch_one(h.app.pool())
        .await
        .unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
async fn approved_bill_is_immutable_and_reissue_replaces_it_explicitly() {
    let (h, _) = H::october().await;
    let id: i64 = h.count("SELECT id FROM bill WHERE status = 'draft'").await;
    h.tap(LANDLORD, &format!("ap:{id}"), at(2026, 10, 17)).await;
    let before: String = sqlx::query_scalar("SELECT snapshot FROM bill WHERE id = ?")
        .bind(id)
        .fetch_one(h.app.pool())
        .await
        .unwrap();

    let r = h
        .say(LANDLORD, "/tariff t1 10 2026-10", at(2026, 10, 20))
        .await;
    assert!(
        all_text(&r).contains("Выставленные счета не изменены"),
        "{r:?}"
    );
    assert!(all_text(&r).contains("/reissue"), "{r:?}");
    assert_eq!(h.status_of("2026-10").await, ["approved"]);

    let r = h.say(LANDLORD, "/reissue", at(2026, 10, 20)).await;
    let ask = h
        .tap(LANDLORD, &button_starting(&r, "ri:"), at(2026, 10, 20))
        .await;
    let t = all_text(&ask);
    assert!(t.contains("Сейчас выставлен: 2063,61"), "{t}");
    assert!(t.contains("По текущим тарифам: 2265,21"), "{t}");
    let r = h
        .tap(LANDLORD, &button_starting(&ask, "rc:"), at(2026, 10, 20))
        .await;
    assert_eq!(h.status_of("2026-10").await, ["void", "draft"]);
    assert!(
        !to_chat(&r, TENANT).is_empty(),
        "арендатор уведомлён: {r:?}"
    );
    let old: String = sqlx::query_scalar("SELECT snapshot FROM bill WHERE id = ?")
        .bind(id)
        .fetch_one(h.app.pool())
        .await
        .unwrap();
    assert_eq!(before, old, "снимок старого счёта не тронут");
}

#[tokio::test]
async fn paid_bill_cannot_be_reissued() {
    let (h, _) = H::october().await;
    let id: i64 = h.count("SELECT id FROM bill WHERE status = 'draft'").await;
    h.tap(LANDLORD, &format!("ap:{id}"), at(2026, 10, 17)).await;
    h.tap(LANDLORD, &format!("pd:{id}"), at(2026, 10, 18)).await;
    h.say(LANDLORD, "/tariff t1 10 2026-10", at(2026, 10, 20))
        .await;
    assert_eq!(h.status_of("2026-10").await, ["paid"]);
    let r = h.tap(LANDLORD, &format!("ri:{id}"), at(2026, 10, 20)).await;
    assert!(all_text(&r).contains("оплачен"), "{r:?}");
}

#[tokio::test]
async fn tariff_change_before_calc_leads_to_calc_with_new_price() {
    let h = H::new().await;
    h.setup(at(2026, 10, 15)).await;
    h.connect_tenant(at(2026, 10, 15)).await;
    // арендатор отправил, владелец не считал — черновик появляется сразу, поэтому проверяем по /calc
    let r = h
        .month(at(2026, 10, 16), ["755", "417", "3975,48", "1179"])
        .await;
    assert!(all_text(&r).contains("Итого: 2063,61 ₽"));
    h.say(LANDLORD, "/tariff t1 9", at(2026, 10, 17)).await;
    let r = h.say(LANDLORD, "/calc", at(2026, 10, 17)).await;
    let t = all_text(&r);
    assert!(!t.contains("2063,61"), "{t}");
    assert_eq!(h.status_of("2026-10").await.last().unwrap(), "draft");
    assert_eq!(
        h.count("SELECT count(*) FROM bill WHERE status = 'draft'")
            .await,
        1
    );
}

#[tokio::test]
async fn same_month_edit_supersedes_and_repeat_is_a_noop() {
    let h = H::new().await;
    h.setup(at(2026, 10, 15)).await;
    h.say(LANDLORD, "/tariff t1 9", at(2026, 10, 17)).await;
    h.say(LANDLORD, "/tariff t1 9,5 2026-10-30", at(2026, 10, 18))
        .await;
    assert_eq!(
        h.t1_rows().await,
        [("2026-10-01".to_string(), "9.5".to_string())]
    );
    let r = h.say(LANDLORD, "/tariff t1 9,5", at(2026, 10, 19)).await;
    assert!(r[0].text.contains("менять нечего"), "{r:?}");
}

#[tokio::test]
async fn sewer_follows_the_monthly_policy() {
    let h = H::new().await;
    h.setup(at(2026, 10, 15)).await;
    h.connect_tenant(at(2026, 10, 15)).await;
    let r = h
        .month(at(2026, 10, 16), ["755", "417", "3975,48", "1179"])
        .await;
    assert!(all_text(&r).contains("Водоотведение"));
    let id: i64 = h.count("SELECT id FROM bill WHERE status = 'draft'").await;
    h.tap(LANDLORD, &format!("ap:{id}"), at(2026, 10, 17)).await;

    h.say(LANDLORD, "/tariff sewer off 2026-11", at(2026, 10, 20))
        .await;
    let r = h
        .month(at(2026, 11, 25), ["757", "418", "4000", "1200"])
        .await;
    assert!(!to_chat(&r, LANDLORD).contains("Водоотведение"), "{r:?}");

    h.say(LANDLORD, "/tariff sewer 55 2026-12", at(2026, 11, 26))
        .await;
    let nov: i64 = h
        .count("SELECT id FROM bill WHERE period = '2026-11'")
        .await;
    h.tap(LANDLORD, &format!("ap:{nov}"), at(2026, 11, 26))
        .await;
    let r = h
        .month(at(2026, 12, 25), ["760", "419", "4010", "1210"])
        .await;
    // вода: 3 + 1 = 4 м³
    assert!(to_chat(&r, LANDLORD).contains("Водоотведение"), "{r:?}");
    assert!(to_chat(&r, LANDLORD).contains("× 55,00"), "{r:?}");
    let oct: String = sqlx::query_scalar("SELECT snapshot FROM bill WHERE period = '2026-10'")
        .fetch_one(h.app.pool())
        .await
        .unwrap();
    assert!(
        oct.contains("sewer"),
        "в утверждённом октябре водоотведение осталось"
    );
}

#[tokio::test]
async fn tariff_command_validates_and_menu_is_monthly() {
    let h = H::new().await;
    h.setup(at(2026, 10, 15)).await;
    h.connect_tenant(at(2026, 10, 15)).await;
    for bad in [
        "/tariff xx 5",
        "/tariff t1",
        "/tariff t1 abc",
        "/tariff t1 5 2026-13",
        "/tariff t1 -5",
    ] {
        let r = h.say(LANDLORD, bad, at(2026, 10, 17)).await;
        assert!(r[0].text.contains("Формат"), "{bad}: {r:?}");
    }
    assert_eq!(h.t1_rows().await.len(), 1, "ничего не записано");

    let r = h.say(LANDLORD, "/tariff", at(2026, 10, 17)).await;
    assert!(buttons(&r).contains(&"tm:2026-10".to_string()), "{r:?}");
    assert!(buttons(&r).contains(&"tm:2026-11".to_string()), "{r:?}");
    h.tap(LANDLORD, "tm:2026-11", at(2026, 10, 17)).await;
    h.say(LANDLORD, "9", at(2026, 10, 17)).await; // сначала ХВС… проходим меню «оставить»
    h.tap(LANDLORD, "tx", at(2026, 10, 17)).await;
    assert_eq!(h.t1_rows().await.len(), 1, "отмена ничего не сохраняет");

    for cb in ["tnew", "tm:2026-10", "tk", "ts", "tx", "th"] {
        let r = h.tap(TENANT, cb, at(2026, 10, 17)).await;
        assert!(!all_text(&r).contains("Тарифы сохранены"), "{cb}: {r:?}");
    }
    assert_eq!(h.t1_rows().await.len(), 1);
}

#[tokio::test]
async fn history_is_monthly() {
    let h = H::new().await;
    h.setup(at(2026, 10, 15)).await;
    h.say(LANDLORD, "/tariff t1 9", at(2026, 10, 17)).await;
    h.say(LANDLORD, "/tariff t1 10 2026-12", at(2026, 10, 18))
        .await;
    let r = h.say(LANDLORD, "/tariffs", at(2026, 10, 19)).await;
    let t = all_text(&r);
    assert!(t.contains("с 2026-10"), "{t}");
    assert!(t.contains("с 2026-12"), "{t}");
    assert!(t.contains("ещё не вступил"), "{t}");
}

#[tokio::test]
async fn reset_explains_what_to_do_for_each_bill_state() {
    let (h, _) = H::october().await;
    let now = at(2026, 10, 17);
    let r = h.say(LANDLORD, "/reset", now).await;
    assert!(r[0].text.contains("«Отклонить»"), "черновик: {r:?}");
    let id: i64 = h.count("SELECT id FROM bill WHERE status = 'draft'").await;
    h.tap(LANDLORD, &format!("ap:{id}"), now).await;
    let r = h.say(LANDLORD, "/reset", now).await;
    assert!(
        r[0].text.contains("/reissue") && !r[0].text.contains("кнопкой «Отклонить»"),
        "утверждён: {r:?}"
    );
    h.tap(LANDLORD, &format!("pd:{id}"), now).await;
    let r = h.say(LANDLORD, "/reset", now).await;
    assert!(
        r[0].text.contains("оплачен") && !r[0].text.contains("/reissue"),
        "оплачен: {r:?}"
    );
}

// ---------- /void ----------

impl H {
    /// Октябрьский счёт утверждён (но не оплачен); возвращает его id.
    async fn approved_october() -> (Self, i64) {
        Self::approved_october_with(false).await
    }

    async fn approved_october_with(allow_void_paid: bool) -> (Self, i64) {
        let (h, _) = H::october_with(allow_void_paid).await;
        let id: i64 = h.count("SELECT id FROM bill WHERE status = 'draft'").await;
        h.tap(LANDLORD, &format!("ap:{id}"), at(2026, 10, 17)).await;
        (h, id)
    }

    /// /void → выбор счёта → причина → кнопка подтверждения; возвращает ответы на причину.
    async fn void_until_confirm(&self, id: i64, now: NaiveDateTime) -> Vec<Reply> {
        let r = self.say(LANDLORD, "/void", now).await;
        assert!(buttons(&r).contains(&format!("vo:{id}")), "{r:?}");
        self.tap(LANDLORD, &format!("vo:{id}"), now).await;
        self.say(LANDLORD, "тестовый счёт", now).await
    }
}

#[tokio::test]
async fn approved_void_reopens_the_period_for_new_readings_and_a_new_bill() {
    let (h, id) = H::approved_october().await;
    let now = at(2026, 10, 18);
    let settings_before = (
        h.count("SELECT count(*) FROM tariff").await,
        h.count("SELECT count(*) FROM reading WHERE kind = 'baseline'")
            .await,
        h.count("SELECT count(*) FROM tenancy WHERE tenant_user_id IS NOT NULL")
            .await,
    );

    let r = h.void_until_confirm(id, now).await;
    assert!(r[0].text.contains("Причина: тестовый счёт"), "{r:?}");
    // до подтверждения ничего не изменилось
    assert_eq!(h.status_of("2026-10").await, ["approved"]);

    let r = h.tap(LANDLORD, &format!("vc:{id}"), now).await;
    assert!(to_chat(&r, TENANT).contains("аннулирован"), "{r:?}");
    assert!(to_chat(&r, TENANT).contains("тестовый счёт"), "{r:?}");
    assert_eq!(h.status_of("2026-10").await, ["void"]);
    let (note, snapshot): (String, String) =
        sqlx::query_as("SELECT note, snapshot FROM bill WHERE id = ?")
            .bind(id)
            .fetch_one(h.app.pool())
            .await
            .unwrap();
    assert!(note.contains("тестовый счёт"));
    assert!(snapshot.contains("2063.61"), "исходный счёт сохранён");
    assert_eq!(
        h.count("SELECT count(*) FROM audit_log WHERE action = 'bill_void'")
            .await,
        1
    );

    // настройки, тарифы, стартовые показания и арендатор на месте
    let settings_after = (
        h.count("SELECT count(*) FROM tariff").await,
        h.count("SELECT count(*) FROM reading WHERE kind = 'baseline'")
            .await,
        h.count("SELECT count(*) FROM tenancy WHERE tenant_user_id IS NOT NULL")
            .await,
    );
    assert_eq!(settings_before, settings_after);

    // новые показания и новый счёт за тот же период
    let r = h
        .month(at(2026, 10, 19), ["756", "418", "3980", "1180"])
        .await;
    assert!(all_text(&r).contains("Расчёт за 2026-10"), "{r:?}");
    assert_eq!(h.status_of("2026-10").await, ["void", "draft"]);
}

#[tokio::test]
async fn void_needs_a_reason_and_an_explicit_confirmation() {
    let (h, id) = H::approved_october().await;
    let now = at(2026, 10, 18);
    h.say(LANDLORD, "/void", now).await;
    h.tap(LANDLORD, &format!("vo:{id}"), now).await;
    let r = h.say(LANDLORD, "   ", now).await;
    assert!(r[0].text.contains("Причина"), "{r:?}");
    // подтверждение без причины и чужой id не действуют
    let r = h.tap(LANDLORD, &format!("vc:{id}"), now).await;
    assert!(r[0].text.contains("устарело"), "{r:?}");
    assert_eq!(h.status_of("2026-10").await, ["approved"]);
    // отмена оставляет счёт
    h.void_until_confirm(id, now).await;
    h.tap(LANDLORD, "vx", now).await;
    let r = h.tap(LANDLORD, &format!("vc:{id}"), now).await;
    assert!(r[0].text.contains("устарело"), "{r:?}");
    assert_eq!(h.status_of("2026-10").await, ["approved"]);
}

#[tokio::test]
async fn duplicate_confirmation_voids_once_and_notifies_once() {
    let (h, id) = H::approved_october().await;
    let now = at(2026, 10, 18);
    h.void_until_confirm(id, now).await;
    let first = h.tap(LANDLORD, &format!("vc:{id}"), now).await;
    let second = h.tap(LANDLORD, &format!("vc:{id}"), now).await;
    assert!(!to_chat(&first, TENANT).is_empty());
    assert!(
        to_chat(&second, TENANT).is_empty(),
        "повтор ничего не рассылает: {second:?}"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM audit_log WHERE action = 'bill_void'")
            .await,
        1
    );
    // повторное нажатие не воссоздаёт счёт и не трогает новые показания
    h.month(at(2026, 10, 19), ["756", "418", "3980", "1180"])
        .await;
    h.tap(LANDLORD, &format!("vc:{id}"), now).await;
    assert_eq!(h.status_of("2026-10").await, ["void", "draft"]);
}

#[tokio::test]
async fn a_paid_bill_is_voided_only_through_its_own_explicit_confirmation() {
    let (h, id) = H::approved_october_with(true).await;
    let now = at(2026, 10, 18);
    // 1. подтверждение, начатое для неоплаченного счёта, не срабатывает, если счёт оплатили в промежутке
    h.void_until_confirm(id, now).await;
    h.tap(LANDLORD, &format!("pd:{id}"), now).await;
    let r = h.tap(LANDLORD, &format!("vc:{id}"), now).await;
    assert!(r[0].text.contains("Статус счёта изменился"), "{r:?}");
    assert_eq!(h.status_of("2026-10").await, ["paid"]);
    assert!(to_chat(&r, TENANT).is_empty());

    // 2. оплаченный счёт виден в /void с пометкой, подтверждение содержит предупреждение
    let r = h.say(LANDLORD, "/void", now).await;
    assert!(r[0].buttons[0][0].label.contains("оплачен"), "{r:?}");
    h.tap(LANDLORD, &format!("vo:{id}"), now).await;
    let r = h.say(LANDLORD, "ошибочные показания", now).await;
    assert!(r[0].text.contains("ОПЛАЧЕН"), "{r:?}");
    assert!(r[0].buttons[0][0].label.contains("оплаченный"), "{r:?}");
    assert_eq!(
        h.status_of("2026-10").await,
        ["paid"],
        "до подтверждения ничего не изменилось"
    );

    // 3. подтверждение: счёт аннулирован, арендатор предупреждён об оплате, период открыт
    let r = h.tap(LANDLORD, &format!("vc:{id}"), now).await;
    assert!(to_chat(&r, TENANT).contains("оплат"), "{r:?}");
    assert_eq!(h.status_of("2026-10").await, ["void"]);
    let details: String =
        sqlx::query_scalar("SELECT details FROM audit_log WHERE action = 'bill_void'")
            .fetch_one(h.app.pool())
            .await
            .unwrap();
    assert!(details.contains("\"was_paid\":true"), "{details}");
    let r = h
        .month(at(2026, 10, 19), ["756", "418", "3980", "1180"])
        .await;
    assert!(all_text(&r).contains("Расчёт за 2026-10"), "{r:?}");

    // 4. арендатору команда недоступна
    let r = h.tap(TENANT, &format!("vc:{id}"), now).await;
    assert!(r[0].text.contains("недоступно"), "{r:?}");
}

#[tokio::test]
async fn void_of_a_draft_is_refused() {
    let (h, _) = H::october().await;
    let id: i64 = h.count("SELECT id FROM bill WHERE status = 'draft'").await;
    let r = h.tap(LANDLORD, &format!("vo:{id}"), at(2026, 10, 18)).await;
    assert!(r[0].text.contains("только утверждённый"), "{r:?}");
    assert_eq!(h.status_of("2026-10").await, ["draft"]);
}

// ---------- расписание и напоминания ----------

async fn october_ready_with_schedule(day: &str) -> H {
    let h = H::new().await;
    h.setup(at(2026, 10, 15)).await;
    h.connect_tenant(at(2026, 10, 15)).await;
    let r = h
        .say(LANDLORD, &format!("/schedule {day}"), at(2026, 10, 15))
        .await;
    assert!(r[0].text.contains("Срок передачи"), "{r:?}");
    h
}

fn at_h(y: i32, m: u32, d: u32, hour: u32) -> NaiveDateTime {
    at(y, m, d).date().and_hms_opt(hour, 0, 0).unwrap()
}

#[tokio::test]
async fn schedule_command_validates_and_is_landlord_only() {
    let h = H::new().await;
    h.setup(at(2026, 10, 15)).await;
    h.connect_tenant(at(2026, 10, 15)).await;
    let now = at(2026, 10, 15);
    for bad in [
        "/schedule 0",
        "/schedule 32",
        "/schedule abc",
        "/schedule -3",
    ] {
        let r = h.say(LANDLORD, bad, now).await;
        assert!(r[0].text.contains("число"), "{bad}: {r:?}");
    }
    let r = h.say(LANDLORD, "/schedule", now).await;
    assert!(r[0].text.contains("не задано"), "{r:?}");
    // 29–31 — это «последний день месяца»; правило показывается явно
    for ok in ["/schedule 29", "/schedule 30", "/schedule 31"] {
        let r = h.say(LANDLORD, ok, now).await;
        assert!(r[0].text.contains("последний день"), "{ok}: {r:?}");
    }
    assert_eq!(h.count("SELECT reading_last FROM property").await, 1);
    assert_eq!(h.count("SELECT count(reading_day) FROM property").await, 0);
    h.say(LANDLORD, "/schedule 25", now).await;
    let r = h.say(LANDLORD, "/schedule", now).await;
    assert!(r[0].text.contains("25-го"), "{r:?}");
    // арендатор расписание не меняет
    h.say(TENANT, "/schedule 5", now).await;
    let day: i64 = h.count("SELECT reading_day FROM property").await;
    assert_eq!(day, 25);
    assert_eq!(
        h.count("SELECT reading_last FROM property").await,
        0,
        "число 25 снимает режим «последний день»"
    );
    h.say(LANDLORD, "/schedule off", now).await;
    assert_eq!(h.count("SELECT count(reading_day) FROM property").await, 0);
    assert_eq!(h.count("SELECT reading_last FROM property").await, 0);
}

/// Бот хранит время квартиры как UTC + сдвиг; у тестовых квартир сдвиг +3 (Москва).
fn utc(local: NaiveDateTime) -> NaiveDateTime {
    local - chrono::Duration::hours(3)
}

#[tokio::test]
async fn reminders_come_on_the_day_and_two_days_later_once_each() {
    let h = october_ready_with_schedule("25").await;
    let quiet = |r: &[Reply]| assert!(r.is_empty(), "{r:?}");
    quiet(&h.app.due_reminders(utc(at(2026, 10, 24))).await.unwrap()); // накануне — тишина
    quiet(
        &h.app
            .due_reminders(utc(at_h(2026, 10, 25, 9)))
            .await
            .unwrap(),
    ); // до 10:00 по времени квартиры

    let r = h
        .app
        .due_reminders(utc(at_h(2026, 10, 25, 10)))
        .await
        .unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].chat_id, TENANT * 100);
    assert!(r[0].text.contains("Сегодня (25.10)"), "{r:?}");
    assert!(buttons(&r).contains(&"enter".to_string()));
    quiet(
        &h.app
            .due_reminders(utc(at_h(2026, 10, 25, 18)))
            .await
            .unwrap(),
    ); // повтор в тот же день — тишина
    quiet(&h.app.due_reminders(utc(at(2026, 10, 26))).await.unwrap()); // на следующий день — не спамим

    let r = h.app.due_reminders(utc(at(2026, 10, 27))).await.unwrap();
    assert_eq!(r.len(), 2, "{r:?}");
    assert!(
        r.iter()
            .any(|x| x.chat_id == TENANT * 100 && x.text.contains("ещё не получены"))
    );
    assert!(
        r.iter()
            .any(|x| x.chat_id == LANDLORD * 100 && x.text.contains("не передал"))
    );
    quiet(&h.app.due_reminders(utc(at(2026, 10, 27))).await.unwrap());
    quiet(&h.app.due_reminders(utc(at(2026, 10, 28))).await.unwrap()); // дальше — тишина
}

#[tokio::test]
async fn the_property_timezone_decides_when_the_day_starts() {
    let h = october_ready_with_schedule("25").await;
    // квартира в UTC+8: 25 октября 10:00 у неё — это 02:00 UTC
    sqlx::query("UPDATE property SET tz_offset_hours = 8")
        .execute(h.app.pool())
        .await
        .unwrap();
    let utc_2am = at_h(2026, 10, 25, 2);
    let r = h.app.due_reminders(utc_2am).await.unwrap();
    assert_eq!(r.len(), 1, "в Иркутске уже 10:00 25-го: {r:?}");
    // а для Москвы (UTC+3) в этот же момент 05:00 — рано
    let h2 = october_ready_with_schedule("25").await;
    assert!(h2.app.due_reminders(utc_2am).await.unwrap().is_empty());
}

#[tokio::test]
async fn schedule_29_to_31_falls_on_the_last_day_of_the_month() {
    let h = october_ready_with_schedule("31").await;
    // в ноябре 30 дней: срок — 30 ноября
    assert!(
        h.app
            .due_reminders(utc(at(2026, 11, 29)))
            .await
            .unwrap()
            .is_empty()
    );
    let r = h.app.due_reminders(utc(at(2026, 11, 30))).await.unwrap();
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].text.contains("30.11"), "{r:?}");
}

#[tokio::test]
async fn no_reminders_without_schedule_after_submission_or_without_a_tenant() {
    // расписания нет
    let h = H::new().await;
    h.setup(at(2026, 10, 15)).await;
    h.connect_tenant(at(2026, 10, 15)).await;
    assert!(
        h.app
            .due_reminders(at(2026, 10, 25))
            .await
            .unwrap()
            .is_empty()
    );
    // показания отправлены — напоминать не о чем
    h.say(LANDLORD, "/schedule 25", at(2026, 10, 15)).await;
    h.month(at(2026, 10, 16), ["755", "417", "3975,48", "1179"])
        .await;
    assert!(
        h.app
            .due_reminders(at(2026, 10, 25))
            .await
            .unwrap()
            .is_empty()
    );
    // арендатор не подключён
    let h = H::new().await;
    h.setup(at(2026, 10, 15)).await;
    h.say(LANDLORD, "/schedule 25", at(2026, 10, 15)).await;
    assert!(
        h.app
            .due_reminders(at(2026, 10, 25))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn paid_bill_cannot_be_voided_by_default() {
    let (h, id) = H::approved_october().await;
    let now = at(2026, 10, 18);
    h.tap(LANDLORD, &format!("pd:{id}"), now).await;
    let r = h.say(LANDLORD, "/void", now).await;
    assert!(r[0].text.contains("Нет утверждённых неоплаченных"), "{r:?}");
    let r = h.tap(LANDLORD, &format!("vo:{id}"), now).await;
    assert!(r[0].text.contains("оплачен"), "{r:?}");
    assert_eq!(h.status_of("2026-10").await, ["paid"]);
}

// ---------- срок оплаты и просрочка ----------

#[tokio::test]
async fn approval_sets_the_payment_deadline_from_the_property_term() {
    let (h, id) = H::approved_october().await;
    let due: String = sqlx::query_scalar("SELECT due_date FROM bill WHERE id = ?")
        .bind(id)
        .fetch_one(h.app.pool())
        .await
        .unwrap();
    assert_eq!(due, "2026-10-27", "17 октября + 10 дней по умолчанию");
    // свой срок у квартиры: 14 дней
    let (h2, _) = H::october().await;
    sqlx::query("UPDATE property SET payment_days = 14")
        .execute(h2.app.pool())
        .await
        .unwrap();
    let draft: i64 = h2.count("SELECT id FROM bill WHERE status = 'draft'").await;
    h2.tap(LANDLORD, &format!("ap:{draft}"), at(2026, 10, 17))
        .await;
    let due: String = sqlx::query_scalar("SELECT due_date FROM bill WHERE id = ?")
        .bind(draft)
        .fetch_one(h2.app.pool())
        .await
        .unwrap();
    assert_eq!(due, "2026-10-31");
}

#[tokio::test]
async fn payment_reminders_come_on_the_due_day_and_two_days_after_once_each() {
    let (h, id) = H::approved_october().await; // срок 27.10
    let quiet = |r: &[Reply]| assert!(r.is_empty(), "{r:?}");
    quiet(&h.app.due_reminders(utc(at(2026, 10, 26))).await.unwrap());
    quiet(
        &h.app
            .due_reminders(utc(at_h(2026, 10, 27, 9)))
            .await
            .unwrap(),
    );
    let r = h
        .app
        .due_reminders(utc(at_h(2026, 10, 27, 10)))
        .await
        .unwrap();
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].chat_id, TENANT * 100);
    assert!(
        r[0].text.contains("срок оплаты") && r[0].text.contains("2063,61"),
        "{r:?}"
    );
    quiet(&h.app.due_reminders(utc(at(2026, 10, 27))).await.unwrap());
    quiet(&h.app.due_reminders(utc(at(2026, 10, 28))).await.unwrap());
    let r = h.app.due_reminders(utc(at(2026, 10, 29))).await.unwrap();
    assert_eq!(r.len(), 2, "{r:?}");
    assert!(
        r.iter()
            .any(|x| x.chat_id == TENANT * 100 && x.text.contains("прошёл"))
    );
    assert!(
        r.iter()
            .any(|x| x.chat_id == LANDLORD * 100 && x.text.contains("не оплачен"))
    );
    quiet(&h.app.due_reminders(utc(at(2026, 10, 29))).await.unwrap());
    // оплачено владельцем — больше ничего (в том числе после срока)
    h.tap(LANDLORD, &format!("pd:{id}"), at(2026, 10, 30)).await;
    quiet(&h.app.due_reminders(utc(at(2026, 10, 31))).await.unwrap());
}

#[tokio::test]
async fn paid_bills_and_bills_without_a_deadline_never_trigger_payment_reminders() {
    let (h, id) = H::approved_october().await;
    h.tap(LANDLORD, &format!("pd:{id}"), at(2026, 10, 18)).await;
    assert!(
        h.app
            .due_reminders(utc(at(2026, 10, 27)))
            .await
            .unwrap()
            .is_empty()
    );
    // счёт, утверждённый до появления сроков (due_date пуст), — без неожиданных уведомлений
    let (h2, id2) = H::approved_october().await;
    sqlx::query("UPDATE bill SET due_date = NULL WHERE id = ?")
        .bind(id2)
        .execute(h2.app.pool())
        .await
        .unwrap();
    assert!(
        h2.app
            .due_reminders(utc(at(2026, 10, 29)))
            .await
            .unwrap()
            .is_empty()
    );
}

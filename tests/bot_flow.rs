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

    /// Настройка объекта по данным golden dataset: июльские тарифы, дата baseline неизвестна. Настройки по умолчанию.
    async fn setup_defaults(&self, now: NaiveDateTime) {
        self.setup_defaults_with_tariff_date(now, "2026-01-01")
            .await;
    }

    async fn setup_defaults_with_tariff_date(&self, now: NaiveDateTime, tariff_from: &str) {
        self.say(LANDLORD, "/setup", now).await;
        for a in [
            "Квартира на Тверской",
            "Иван",
            "неизвестна",
            "753",
            "416",
            "3831",
            "1118",
            tariff_from,
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
    assert!(r[0].text.contains("(1/13)"), "{r:?}");
    let r = h.say(LANDLORD, "Квартира", at(2026, 6, 26)).await;
    assert!(r[0].text.contains("(2/13)"), "{r:?}");
    h.say(LANDLORD, "Иван", at(2026, 6, 26)).await;
    let r = h.say(LANDLORD, "вчера", at(2026, 6, 26)).await;
    assert!(r[0].text.contains("Не получилось разобрать"), "{r:?}");
    let r = h.say(LANDLORD, "25.06.2026", at(2026, 6, 26)).await;
    assert!(
        r[0].text.contains("(4/13)") && r[0].text.contains("ХВС"),
        "{r:?}"
    );
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
async fn real_price_change_inside_the_period_blocks_billing_and_offers_a_choice() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.say(LANDLORD, "/tariff t1 9 2026-07-15", now).await;
    let r = h.month(now, ["755", "417", "3975", "1179"]).await;
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 0);
    let landlord = r
        .iter()
        .find(|x| x.chat_id == LANDLORD * 100)
        .expect("owner is told");
    assert!(
        landlord
            .text
            .contains("Электричество T1: 8,60 → 9,00 ₽ с 2026-07-15"),
        "{}",
        landlord.text
    );
    assert!(landlord.text.contains("ваше решение"), "{}", landlord.text);
    assert!(
        landlord.buttons.iter().flatten().any(|b| b.data == "cn")
            && landlord.buttons.iter().flatten().any(|b| b.data == "co")
    );
    // и без решения /calc тоже не считает, а снова предлагает выбор
    let r = h.say(LANDLORD, "/calc", now).await;
    assert!(
        r[0].buttons.iter().flatten().any(|b| b.data == "cn"),
        "{r:?}"
    );
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 0);
}

#[tokio::test]
async fn owner_can_apply_the_new_price_to_the_whole_period_and_it_is_recorded_in_the_bill() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.say(LANDLORD, "/tariff t1 9 2026-07-15", now).await;
    h.month(now, ["755", "417", "3975", "1179"]).await;
    let r = h.tap(LANDLORD, "cn", now).await;
    let text = all_text(&r);
    // T1: 144 кВт·ч × 9,00 = 1296,00 вместо 1238,40 → итог 2063,61 + 57,60 = 2121,21
    assert!(
        text.contains("Электричество T1: 3975 − 3831 = 144 кВт·ч × 9,00 = 1296,00 ₽"),
        "{text}"
    );
    assert!(text.contains("Итого: 2121,21 ₽"), "{text}");
    assert!(
        text.contains("по решению владельца для всего периода применён новый тариф"),
        "решение записано в счёте: {text}"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM bill WHERE status = 'draft'")
            .await,
        1
    );
    let snapshot: String = sqlx::query_scalar("SELECT snapshot FROM bill")
        .fetch_one(h.app.pool())
        .await
        .unwrap();
    assert!(snapshot.contains("по решению владельца"), "{snapshot}");
}

#[tokio::test]
async fn owner_can_apply_the_old_price_to_the_whole_period() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.say(LANDLORD, "/tariff t1 9 2026-07-15", now).await;
    h.month(now, ["755", "417", "3975", "1179"]).await;
    let r = h.tap(LANDLORD, "co", now).await;
    let text = all_text(&r);
    assert!(
        text.contains("Итого: 2063,61 ₽") && text.contains("применён прежний тариф"),
        "{text}"
    );
    // повторное решение не создаёт второй счёт
    let again = h.tap(LANDLORD, "cn", now).await;
    assert!(again[0].text.contains("счёт уже есть"), "{again:?}");
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 1);
}

#[tokio::test]
async fn a_first_tariff_that_starts_mid_period_is_not_a_change() {
    // ровно сценарий из жизни: тарифы заведены 15 октября, показания пришли 17-го — расчёт идёт без «ручной разбивки»
    let h = H::new().await;
    h.setup_defaults_with_tariff_date(at(2026, 10, 15), "2026-10-15")
        .await;
    h.set_proof(false).await;
    h.connect_tenant(at(2026, 10, 15)).await;
    let r = h
        .month(at(2026, 10, 17), ["755", "417", "3975,48", "1179"])
        .await;
    let draft = r
        .iter()
        .find(|x| x.chat_id == LANDLORD * 100)
        .expect("draft goes to the owner");
    assert!(draft.text.contains("Итого: 2063,61 ₽"), "{}", draft.text);
    assert!(
        !draft.text.contains("ℹ️"),
        "никаких решений владельца не понадобилось: {}",
        draft.text
    );
    assert_eq!(
        h.count("SELECT count(*) FROM bill WHERE status = 'draft'")
            .await,
        1
    );
}

#[tokio::test]
async fn re_entering_the_same_prices_with_an_earlier_date_changes_nothing_and_does_not_block() {
    let h = H::new().await;
    h.setup_defaults_with_tariff_date(at(2026, 10, 15), "2026-10-15")
        .await;
    h.set_proof(false).await;
    h.connect_tenant(at(2026, 10, 15)).await;
    h.month(at(2026, 10, 17), ["755", "417", "3975,48", "1179"])
        .await; // черновик ждёт решения владельца
    h.tap(LANDLORD, "rj:1", at(2026, 10, 17)).await; // отклонили, чтобы вернуться к тарифам
    // владелец открывает меню и вводит те же цены с 1 октября — как в жизни; ничего не должно записаться
    h.say(LANDLORD, "/tariff", at(2026, 10, 17)).await;
    h.tap(LANDLORD, "td:2026-10-01", at(2026, 10, 17)).await;
    for price in ["65,77", "312,50", "51,62", "8,60"] {
        h.say(LANDLORD, price, at(2026, 10, 17)).await;
    }
    let r = h.say(LANDLORD, "3,71", at(2026, 10, 17)).await;
    assert!(
        all_text(&r).contains("Ничего не меняется"),
        "одинаковые цены — это не изменение: {r:?}"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM tariff WHERE valid_from = '2026-10-01'")
            .await,
        0
    );
}

#[tokio::test]
async fn menu_warns_when_a_later_tariff_will_override_the_new_one() {
    // жизненный случай: цены уже заведены с 15 октября, а владелец «меняет» их с 1 октября
    let h = H::new().await;
    h.setup_defaults_with_tariff_date(at(2026, 10, 15), "2026-10-15")
        .await;
    let now = at(2026, 10, 17);
    h.say(LANDLORD, "/tariff", now).await;
    h.tap(LANDLORD, "td:2026-10-01", now).await;
    h.say(LANDLORD, "70", now).await;
    for _ in 0..3 {
        h.tap(LANDLORD, "tk", now).await;
    }
    let r = h.tap(LANDLORD, "tk", now).await;
    let text = all_text(&r);
    assert!(
        text.contains("Холодная вода (ХВС): 65,77 ₽ → 70,00 ₽"),
        "{text}"
    );
    assert!(
        text.contains(
            "⚠️ с 2026-10-15 уже задана цена 65,77 ₽ — после этой даты будет действовать она"
        ),
        "{text}"
    );
}

#[tokio::test]
async fn saving_tariffs_offers_a_recalc_button_when_the_readings_are_waiting() {
    let h = H::ready().await;
    let now = at(2026, 7, 25);
    h.say(LANDLORD, "/tariff t1 9 2026-07-15", now).await;
    h.month(now, ["755", "417", "3975", "1179"]).await; // расчёт ждёт решения о цене
    assert_eq!(h.count("SELECT count(*) FROM bill").await, 0);
    h.say(LANDLORD, "/tariff", now).await;
    h.tap(LANDLORD, "td:2026-08-01", now).await;
    h.say(LANDLORD, "70", now).await;
    for _ in 0..3 {
        h.tap(LANDLORD, "tk", now).await;
    }
    h.tap(LANDLORD, "tk", now).await;
    let r = h.tap(LANDLORD, "ts", now).await;
    assert!(
        buttons(&r).contains(&"cc".to_string()),
        "после сохранения тарифов предлагается пересчёт: {r:?}"
    );
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

#[tokio::test]
async fn tariff_menu_walks_through_every_item_and_saves_once() {
    let h = H::ready().await;
    let now = at(2026, 7, 30);
    let r = h.say(LANDLORD, "/tariff", now).await;
    assert!(r[0].text.contains("С какой даты"), "{r:?}");
    assert!(
        buttons(&r).contains(&"td:2026-08-01".to_string()),
        "кнопка «с 1-го числа следующего месяца»: {r:?}"
    );

    let r = h.tap(LANDLORD, "td:2026-08-01", now).await;
    assert!(
        r[0].text.contains("Шаг 1 из 5") && r[0].text.contains("Сейчас: 65,77 ₽"),
        "{r:?}"
    );
    let r = h.say(LANDLORD, "66,87", now).await;
    assert!(
        r[0].text.contains("Шаг 2 из 5") && r[0].text.contains("Сейчас: 312,50"),
        "{r:?}"
    );
    let r = h.say(LANDLORD, "317,71", now).await;
    assert!(
        r[0].text.contains("Шаг 3 из 5") && r[0].text.contains("Водоотведение"),
        "{r:?}"
    );
    assert!(
        buttons(&r).contains(&"toff".to_string()),
        "водоотведение можно выключить: {r:?}"
    );
    h.say(LANDLORD, "52,48", now).await;
    h.tap(LANDLORD, "tk", now).await; // T1 без изменений
    let r = h.tap(LANDLORD, "tk", now).await; // T2 без изменений
    let text = all_text(&r);
    assert!(
        text.contains("Холодная вода (ХВС): 65,77 ₽ → 66,87 ₽"),
        "{text}"
    );
    assert!(text.contains("Водоотведение: 51,62 ₽ → 52,48 ₽"), "{text}");
    assert!(
        !text.contains("T1"),
        "неизменённое в сводку не попадает: {text}"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM tariff WHERE valid_from = '2026-08-01'")
            .await,
        0,
        "до «Сохранить» ничего не записано"
    );

    let r = h.tap(LANDLORD, "ts", now).await;
    assert!(r[0].text.contains("Тарифы сохранены"), "{r:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM tariff WHERE valid_from = '2026-08-01'")
            .await,
        3
    );
    // повторное «Сохранить» ничего не дублирует
    let again = h.tap(LANDLORD, "ts", now).await;
    assert!(again[0].text.contains("уже сохранены"), "{again:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM tariff WHERE valid_from = '2026-08-01'")
            .await,
        3
    );
}

#[tokio::test]
async fn tariff_menu_with_no_changes_stores_nothing() {
    let h = H::ready().await;
    let now = at(2026, 7, 30);
    h.say(LANDLORD, "/tariff", now).await;
    h.tap(LANDLORD, "td:2026-08-01", now).await;
    let mut last = vec![];
    for _ in 0..5 {
        last = h.tap(LANDLORD, "tk", now).await;
    }
    assert!(last[0].text.contains("Ничего не меняется"), "{last:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM tariff WHERE valid_from = '2026-08-01'")
            .await,
        0
    );
}

#[tokio::test]
async fn tariff_menu_can_turn_sewer_off() {
    let h = H::ready().await;
    let now = at(2026, 7, 30);
    h.say(LANDLORD, "/tariff", now).await;
    h.say(LANDLORD, "2026-08-01", now).await; // дату можно ввести текстом
    h.tap(LANDLORD, "tk", now).await;
    h.tap(LANDLORD, "tk", now).await;
    let r = h.tap(LANDLORD, "toff", now).await;
    assert!(r[0].text.contains("Шаг 4 из 5"), "{r:?}");
    h.tap(LANDLORD, "tk", now).await;
    let r = h.tap(LANDLORD, "tk", now).await;
    assert!(all_text(&r).contains("выключить"), "{r:?}");
    h.tap(LANDLORD, "ts", now).await;
    assert_eq!(h.count("SELECT sewer_enabled FROM property").await, 0);
}

#[tokio::test]
async fn tariff_menu_validates_input_and_can_be_cancelled() {
    let h = H::ready().await;
    let now = at(2026, 7, 30);
    h.say(LANDLORD, "/tariff", now).await;
    let r = h.say(LANDLORD, "завтра", now).await;
    assert!(r[0].text.contains("Не получилось разобрать дату"), "{r:?}");
    h.say(LANDLORD, "2026-08-01", now).await;
    let r = h.say(LANDLORD, "много", now).await;
    assert!(r[0].text.contains("Нужно число"), "{r:?}");
    let r = h.say(LANDLORD, "-3", now).await;
    assert!(r[0].text.contains("Нужно число"), "{r:?}");
    // шаг не сдвинулся — принимается верная цена для ХВС
    let r = h.say(LANDLORD, "66,87", now).await;
    assert!(r[0].text.contains("Шаг 2 из 5"), "{r:?}");
    let r = h.tap(LANDLORD, "tx", now).await;
    assert!(r[0].text.contains("отменён"), "{r:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM tariff WHERE valid_from = '2026-08-01'")
            .await,
        0
    );
    // после отмены кнопка «Сохранить» из старого сообщения ничего не делает
    let r = h.tap(LANDLORD, "ts", now).await;
    assert!(
        r[0].text.contains("уже сохранены или меню закрыто"),
        "{r:?}"
    );
}

#[tokio::test]
async fn tariff_menu_refuses_a_duplicate_date_and_tenant_cannot_use_it() {
    let h = H::ready().await;
    let now = at(2026, 7, 30);
    // на 2026-01-01 тарифы уже заведены мастером
    h.say(LANDLORD, "/tariff", now).await;
    h.tap(LANDLORD, "td:2026-01-01", now).await;
    h.say(LANDLORD, "70", now).await;
    for _ in 0..4 {
        h.tap(LANDLORD, "tk", now).await;
    }
    let r = h.tap(LANDLORD, "ts", now).await;
    assert!(r[0].text.contains("уже есть"), "{r:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM tariff WHERE code = 'cold'")
            .await,
        1
    );

    for data in ["ts", "td:2026-08-01", "tk", "tx"] {
        let r = h.tap(TENANT, data, now).await;
        assert!(r[0].text.contains("недоступно"), "{data}: {r:?}");
    }
    let r = h.say(TENANT, "/tariff", now).await;
    assert!(!r[0].text.contains("С какой даты"), "{r:?}");
}

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
    assert!(r[0].text.contains("не подключены"), "{r:?}");
    let r = h.say(STRANGER, "/enter", now).await;
    assert!(r[0].text.contains("не подключены"), "{r:?}");
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
async fn tariff_history_lists_versions_newest_first_with_change_and_current_mark() {
    let h = H::ready().await;
    let now = at(2026, 9, 10);
    for cmd in [
        "/tariff cold 66,87 2026-08-01",
        "/tariff cold 68 2026-10-01",
        "/tariff sewer 52,48 2026-08-01",
    ] {
        h.say(LANDLORD, cmd, now).await;
    }
    let r = h.say(LANDLORD, "/tariffs", now).await;
    let text = all_text(&r);
    // ХВС: 68 (будущий), 66,87 (действует), 65,77 (исходный)
    let cold = text.split("Горячая вода").next().unwrap();
    let pos_future = cold
        .find("с 2026-10-01: 68,00 (+1,13) — ещё не вступил в силу")
        .expect(&text);
    let pos_now = cold
        .find("с 2026-08-01: 66,87 (+1,10) — действует")
        .expect(&text);
    let pos_old = cold.find("с 2026-01-01: 65,77").expect(&text);
    assert!(
        pos_future < pos_now && pos_now < pos_old,
        "от новых к старым: {text}"
    );
    assert!(
        text.contains("Водоотведение, ₽ за м³:")
            && text.contains("с 2026-08-01: 52,48 (+0,86) — действует"),
        "{text}"
    );
    assert!(buttons(&r).contains(&"tnew".to_string()));
}

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

#[tokio::test]
async fn tariff_history_shows_sewer_as_off_when_disabled() {
    let h = H::ready().await;
    h.say(LANDLORD, "/tariff sewer off", at(2026, 9, 10)).await;
    let r = h.say(LANDLORD, "/tariffs", at(2026, 9, 10)).await;
    assert!(
        r[0].text.contains("сейчас арендатор не оплачивает"),
        "{r:?}"
    );
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

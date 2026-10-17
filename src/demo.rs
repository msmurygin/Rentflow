//! Демо-квартира для знакомства: владелец с отрицательным Telegram-id, история счетов и один счёт на утверждении.
//! Это обычные данные в базе, но отдельные от реальных: у демо-пользователей `telegram_id < 0`,
//! API не пускает их на запись, бот не шлёт им сообщений, метрики их не считают.

use crate::app::{App, Incoming, Res, Sender, Who};
use crate::setup::NewProperty;
use chrono::{Duration, NaiveDateTime};

pub const DEMO_OWNER_ID: i64 = -1000;
pub const DEMO_TENANT_ID: i64 = -1001;

pub fn is_demo(telegram_id: i64) -> bool {
    telegram_id < 0
}

fn sender(id: i64, name: &str) -> Sender {
    Sender {
        telegram_id: id,
        chat_id: id,
        name: name.into(),
    }
}

impl App {
    /// Создаёт демо-квартиру один раз; возвращает её id. Повторный вызов ничего не меняет.
    pub async fn ensure_demo(&self, now: NaiveDateTime) -> Res<i64> {
        let owner = sender(DEMO_OWNER_ID, "Демо");
        let existing: Option<i64> = sqlx::query_scalar(
            "SELECT p.id FROM property p JOIN app_user u ON u.id = p.landlord_id WHERE u.telegram_id = ? ORDER BY p.id LIMIT 1",
        )
        .bind(DEMO_OWNER_ID)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(id) = existing {
            return Ok(id);
        }
        let owner_id = self.register_owner(&owner, now).await?;
        let two_ago = now - Duration::days(61);
        let one_ago = now - Duration::days(31);
        let pid = self
            .create_property(
                owner_id,
                &NewProperty {
                    name: "Демо: квартира на Ленина, 5".into(),
                    tenant_name: "Анна (демо)".into(),
                    base_date: Some(two_ago.date().to_string()),
                    baselines: ["cold", "hot", "t1", "t2"]
                        .iter()
                        .zip(["100", "50", "1000", "400"])
                        .map(|(c, v)| (c.to_string(), v.to_string()))
                        .collect(),
                    prices: ["cold", "hot", "t1", "t2"]
                        .iter()
                        .zip(["45", "220", "6.5", "3.2"])
                        .map(|(c, v)| (c.to_string(), v.to_string()))
                        .collect(),
                    sewer_price: Some("38".into()),
                    tz_offset_hours: Some(3),
                    ..Default::default()
                },
                two_ago,
            )
            .await?;
        // фото в демо не нужны: показания вводятся числами
        sqlx::query("UPDATE property SET proof_required = 0 WHERE id = ?")
            .bind(pid)
            .execute(&self.pool)
            .await?;
        let token = self.create_invite(pid, now).await?;
        let tenant = sender(DEMO_TENANT_ID, "Анна (демо)");
        self.handle(&tenant, Incoming::Start(Some(token)), now)
            .await;
        let Who::Tenant(tu) = self.who(&tenant, now).await? else {
            return Err(crate::app::AppError::User(
                "демо: арендатор не подключился".into(),
            ));
        };
        // два прошедших месяца (первый оплачен, второй ждёт оплаты) и текущий — на проверке у владельца
        let months = [
            (one_ago, ["104", "52", "1100", "430"], true),
            (now, ["109", "55", "1190", "465"], false),
        ];
        for (at, values, pay) in months {
            for (code, v) in ["cold", "hot", "t1", "t2"].iter().zip(values) {
                self.form_set_value(&tu, code, v, at).await?;
            }
            self.entry_submit(&tenant, &tu, at).await?;
            let id: i64 = sqlx::query_scalar(
                "SELECT id FROM bill WHERE property_id = ? AND status = 'draft' ORDER BY id DESC LIMIT 1",
            )
            .bind(pid)
            .fetch_one(&self.pool)
            .await?;
            if at == one_ago {
                self.bill_approve(&owner, id, at).await?;
                if pay {
                    self.bill_paid(&owner, id, at).await?;
                }
            }
        }
        Ok(pid)
    }
}

//! Меню тарифов для владельца: дата начала → цена по каждой позиции (или «Оставить») → сводка изменений → «Сохранить».
//! Те же правила, что и у /tariff с аргументами: новый тариф не меняет уже выставленные счета.

use crate::app::*;
use crate::util::*;
use chrono::{Datelike, NaiveDate, NaiveDateTime};
use rust_decimal::Decimal;

const CODES: [&str; 5] = ["cold", "hot", "sewer", "t1", "t2"];
const TOTAL: usize = CODES.len();

fn first_of_next_month(d: NaiveDate) -> NaiveDate {
    if d.month() == 12 {
        NaiveDate::from_ymd_opt(d.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(d.year(), d.month() + 1, 1)
    }
    .expect("valid date")
}

impl App {
    /// Действующая на дату цена позиции (или самая свежая, если на дату ничего нет).
    async fn price_on(&self, property_id: i64, code: &str, date: &str) -> Res<Option<Decimal>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT price FROM tariff WHERE property_id = ? AND code = ?
              ORDER BY (valid_from <= ?) DESC, valid_from DESC LIMIT 1",
        )
        .bind(property_id)
        .bind(code)
        .bind(date)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|(p,)| p.parse().ok()))
    }

    async fn tariff_prompt(
        &self,
        from: &Sender,
        property_id: i64,
        date: &str,
        step: usize,
    ) -> Res<Reply> {
        let code = CODES[step];
        let sewer_on: bool = sqlx::query_scalar("SELECT sewer_enabled FROM property WHERE id = ?")
            .bind(property_id)
            .fetch_one(&self.pool)
            .await?;
        let current = if code == "sewer" && !sewer_on {
            "Сейчас: арендатор водоотведение не оплачивает".to_string()
        } else {
            match self.price_on(property_id, code, date).await? {
                Some(p) => format!("Сейчас: {} ₽ за {}", money(p), unit(code)),
                None => "Сейчас: тарифа нет".to_string(),
            }
        };
        let keep_label = if code == "sewer" && !sewer_on {
            "Оставить выключенным"
        } else {
            "Оставить без изменений"
        };
        let mut rows = vec![vec![(keep_label, "tk".to_string())]];
        if code == "sewer" && sewer_on {
            rows.push(vec![("Выключить водоотведение", "toff".to_string())]);
        }
        rows.push(vec![("🗑 Отмена", "tx".to_string())]);
        Ok(Reply::with_buttons(
            from.chat_id,
            format!(
                "Шаг {} из {TOTAL} — {}\n{current}\nТариф действует с {date}. Введите новую цену числом (например, 66,87) или выберите действие.",
                step + 1,
                label(code)
            ),
            rows,
        ))
    }

    /// История тарифов по каждой позиции: от новых к старым, с изменением к предыдущему и пометкой «действует».
    pub(crate) async fn tariff_history(
        &self,
        from: &Sender,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(property_id) = self.property_id().await? else {
            return user_err("Объект не настроен. Начните с /setup");
        };
        let sewer_on: bool = sqlx::query_scalar("SELECT sewer_enabled FROM property WHERE id = ?")
            .bind(property_id)
            .fetch_one(&self.pool)
            .await?;
        let today = now.date().to_string();
        let mut blocks = vec![];
        for code in CODES {
            let rows: Vec<(String, String)> =
                sqlx::query_as("SELECT valid_from, price FROM tariff WHERE property_id = ? AND code = ? ORDER BY valid_from DESC")
                    .bind(property_id)
                    .bind(code)
                    .fetch_all(&self.pool)
                    .await?;
            let mut lines = vec![format!("{}, ₽ за {}:", label(code), unit(code))];
            if code == "sewer" && !sewer_on {
                lines.push("  сейчас арендатор не оплачивает".into());
            }
            if rows.is_empty() {
                lines.push("  тарифов нет".into());
            }
            // «действует» — самый свежий тариф, начавшийся не позже сегодняшнего дня
            let current = rows.iter().position(|(from, _)| *from <= today);
            for (i, (valid_from, price)) in rows.iter().enumerate() {
                let price: Decimal = price.parse().unwrap_or_default();
                let change = match rows.get(i + 1).and_then(|(_, p)| p.parse::<Decimal>().ok()) {
                    Some(prev) if price != prev => {
                        let d = price - prev;
                        format!(
                            " ({}{})",
                            if d.is_sign_positive() { "+" } else { "−" },
                            money(d.abs())
                        )
                    }
                    _ => String::new(),
                };
                let mark = if Some(i) == current {
                    " — действует"
                } else if *valid_from > today {
                    " — ещё не вступил в силу"
                } else {
                    ""
                };
                lines.push(format!("  с {valid_from}: {}{change}{mark}", money(price)));
            }
            blocks.push(lines.join("\n"));
        }
        Ok(vec![Reply::with_buttons(
            from.chat_id,
            format!(
                "История тарифов\n\n{}\n\nИзменить: /tariff",
                blocks.join("\n\n")
            ),
            vec![vec![("✏️ Изменить тарифы", "tnew".into())]],
        )])
    }

    /// `/tariff` без аргументов: выбор даты начала действия.
    pub(crate) async fn tariff_start(&self, from: &Sender, now: NaiveDateTime) -> Res<Vec<Reply>> {
        self.set_state(
            from.telegram_id,
            &State::Tariff {
                date: None,
                step: 0,
                answers: vec![],
            },
        )
        .await?;
        let today = now.date();
        let this_month =
            NaiveDate::from_ymd_opt(today.year(), today.month(), 1).expect("valid date");
        let next_month = first_of_next_month(today);
        Ok(vec![Reply::with_buttons(
            from.chat_id,
            "Новые тарифы. С какой даты они начинают действовать?\nВыберите кнопкой или введите дату (ГГГГ-ММ-ДД). Обычно тарифы меняются с 1-го числа.",
            vec![
                vec![(&*format!("С {next_month}"), format!("td:{next_month}"))],
                vec![(&*format!("С {this_month}"), format!("td:{this_month}"))],
                vec![
                    ("📜 История тарифов", "th".into()),
                    ("🗑 Отмена", "tx".into()),
                ],
            ],
        )])
    }

    pub(crate) async fn tariff_date(
        &self,
        from: &Sender,
        date: &str,
        _now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(d) = parse_date(date) else {
            return user_err(
                "Не получилось разобрать дату. Введите её как ГГГГ-ММ-ДД, например 2026-08-01.",
            );
        };
        let Some(property_id) = self.property_id().await? else {
            return user_err("Сначала настройте объект: /setup");
        };
        let date = d.to_string();
        self.set_state(
            from.telegram_id,
            &State::Tariff {
                date: Some(date.clone()),
                step: 0,
                answers: vec![],
            },
        )
        .await?;
        Ok(vec![self.tariff_prompt(from, property_id, &date, 0).await?])
    }

    /// Текст владельца, пока открыто меню тарифов: дата либо цена.
    pub(crate) async fn tariff_text(
        &self,
        from: &Sender,
        text: &str,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(State::Tariff { date, .. }) = self.state(from.telegram_id).await? else {
            return user_err("Меню тарифов закрыто. Откройте его заново: /tariff");
        };
        if date.is_none() {
            return self.tariff_date(from, text.trim(), now).await;
        }
        let Some(price) = parse_decimal(text) else {
            return user_err(
                "Нужно число, например 66,87. Или выберите действие кнопкой (/cancel — отмена).",
            );
        };
        self.tariff_answer(from, &price.to_string(), now).await
    }

    /// Ответ на текущий шаг: цена, `keep` или `off` (только для водоотведения).
    pub(crate) async fn tariff_answer(
        &self,
        from: &Sender,
        answer: &str,
        _now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(State::Tariff {
            date: Some(date),
            step,
            mut answers,
        }) = self.state(from.telegram_id).await?
        else {
            return user_err("Сначала выберите дату: /tariff");
        };
        if step >= TOTAL {
            return user_err("Все позиции уже заполнены — нажмите «Сохранить» или «Отмена».");
        }
        if answer == "off" && CODES[step] != "sewer" {
            return user_err("Выключить можно только водоотведение.");
        }
        let Some(property_id) = self.property_id().await? else {
            return user_err("Сначала настройте объект: /setup");
        };
        answers.push(answer.to_string());
        let step = step + 1;
        self.set_state(
            from.telegram_id,
            &State::Tariff {
                date: Some(date.clone()),
                step,
                answers: answers.clone(),
            },
        )
        .await?;
        if step < TOTAL {
            return Ok(vec![
                self.tariff_prompt(from, property_id, &date, step).await?,
            ]);
        }

        let mut lines = vec![];
        for (code, ans) in CODES.iter().zip(&answers) {
            let before = self
                .price_on(property_id, code, &date)
                .await?
                .map_or("нет".to_string(), |p| format!("{} ₽", money(p)));
            match ans.as_str() {
                "keep" => {}
                "off" => lines.push(format!(
                    "• {}: выключить (арендатор не оплачивает)",
                    label(code)
                )),
                p => lines.push(format!(
                    "• {}: {before} → {} ₽ за {}",
                    label(code),
                    money(p.parse().unwrap_or_default()),
                    unit(code)
                )),
            }
        }
        if lines.is_empty() {
            self.clear_state(from.telegram_id).await?;
            return Ok(vec![Reply::text(
                from.chat_id,
                "Ничего не меняется — тарифы остались прежними.",
            )]);
        }
        Ok(vec![Reply::with_buttons(
            from.chat_id,
            format!(
                "Проверьте новые тарифы с {date}:\n\n{}\n\nУже выставленные счета не меняются. Сохранить?",
                lines.join("\n")
            ),
            vec![vec![
                ("✅ Сохранить", "ts".into()),
                ("🗑 Отмена", "tx".into()),
            ]],
        )])
    }

    pub(crate) async fn tariff_save(&self, from: &Sender, now: NaiveDateTime) -> Res<Vec<Reply>> {
        let Some(State::Tariff {
            date: Some(date),
            step,
            answers,
        }) = self.state(from.telegram_id).await?
        else {
            return Ok(vec![Reply::text(
                from.chat_id,
                "Тарифы уже сохранены или меню закрыто ✔ Проверить: /status",
            )]);
        };
        if step != TOTAL || answers.len() != TOTAL {
            return user_err("Не все позиции заполнены — продолжайте по шагам.");
        }
        let Some(property_id) = self.property_id().await? else {
            return user_err("Сначала настройте объект: /setup");
        };
        let ts = now_iso(now);
        let mut tx = self.pool.begin().await?;
        for (code, ans) in CODES.iter().zip(&answers) {
            match ans.as_str() {
                "keep" => {}
                "off" => {
                    sqlx::query("UPDATE property SET sewer_enabled = 0 WHERE id = ?")
                        .bind(property_id)
                        .execute(&mut *tx)
                        .await?;
                }
                price => {
                    let inserted = sqlx::query("INSERT INTO tariff (property_id, code, price, valid_from, created_at) VALUES (?, ?, ?, ?, ?)")
                        .bind(property_id)
                        .bind(code)
                        .bind(price)
                        .bind(&date)
                        .bind(&ts)
                        .execute(&mut *tx)
                        .await;
                    if let Err(sqlx::Error::Database(e)) = &inserted
                        && e.is_unique_violation()
                    {
                        return user_err(format!(
                            "Тариф «{}» на {date} уже есть — выберите другую дату (/tariff).",
                            label(code)
                        ));
                    }
                    inserted?;
                    if *code == "sewer" {
                        sqlx::query("UPDATE property SET sewer_enabled = 1 WHERE id = ?")
                            .bind(property_id)
                            .execute(&mut *tx)
                            .await?;
                    }
                }
            }
        }
        tx.commit().await?;
        self.clear_state(from.telegram_id).await?;
        Ok(vec![Reply::text(
            from.chat_id,
            format!(
                "✅ Тарифы сохранены, действуют с {date}.\nЕсли арендатор уже прислал все показания, а расчёт ждал тарифов, выполните /calc."
            ),
        )])
    }
}

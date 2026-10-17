//! Тарифы по расчётным месяцам. Тариф действует с выбранного месяца (`ГГГГ-ММ`) целиком и на все следующие месяцы,
//! пока владелец его не заменит; календарной даты внутри месяца нет (никаких пропорций и разбивки по дням).
//! Меню: месяц → цена по каждой позиции (или «Оставить») → сводка изменений → «Сохранить».

use crate::app::*;
use crate::bills::{TariffRow, effective_tariff};
use crate::setup::insert_tariff;
use crate::util::*;
use chrono::{Datelike, NaiveDate, NaiveDateTime};
use rust_decimal::Decimal;

pub(crate) const CODES: [&str; 5] = ["cold", "hot", "sewer", "t1", "t2"];
const TOTAL: usize = CODES.len();

/// Изменение одной позиции.
#[derive(Clone)]
pub enum Change {
    Price(Decimal),
    /// Только водоотведение: с этого месяца арендатор его не оплачивает.
    Off,
}

fn next_month(period: &str) -> String {
    let (first, _) = period_bounds(period).expect("valid period");
    let next = if first.month() == 12 {
        NaiveDate::from_ymd_opt(first.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1)
    };
    period_of(next.expect("valid date"))
}

/// «66,87 ₽» либо «выключено» для выключенного водоотведения.
fn show(row: &TariffRow) -> String {
    if row.enabled {
        format!("{} ₽", money(row.price))
    } else {
        "выключено".to_string()
    }
}

impl App {
    /// Тариф позиции, действующий в месяце `month`.
    pub(crate) async fn tariff_at(
        &self,
        property_id: i64,
        code: &str,
        month: &str,
    ) -> Res<Option<TariffRow>> {
        let versions = self.tariff_versions(property_id, code).await?;
        Ok(effective_tariff(&versions, month).cloned())
    }

    /// История тарифов по каждой позиции: от новых месяцев к старым, с изменением к предыдущей цене.
    pub(crate) async fn tariff_history(
        &self,
        from: &Sender,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(property_id) = self.property_id().await? else {
            return user_err("Объект не настроен. Начните с /setup");
        };
        let current = period_of(now.date());
        let mut blocks = vec![];
        for code in CODES {
            let versions = self.tariff_versions(property_id, code).await?;
            let mut lines = vec![format!("{}, ₽ за {}:", label(code), unit(code))];
            if versions.is_empty() {
                lines.push("  тарифов нет".into());
            }
            let in_force = effective_tariff(&versions, &current).map(|r| r.month.clone());
            for (i, row) in versions.iter().enumerate().rev() {
                let change = match i.checked_sub(1).map(|j| &versions[j]) {
                    Some(prev) if row.enabled && prev.enabled && row.price != prev.price => {
                        let d = row.price - prev.price;
                        format!(
                            " ({}{})",
                            if d.is_sign_positive() { "+" } else { "−" },
                            money(d.abs())
                        )
                    }
                    _ => String::new(),
                };
                let mark = if Some(&row.month) == in_force.as_ref() {
                    " — действует"
                } else if row.month > current {
                    " — ещё не вступил в силу"
                } else {
                    ""
                };
                lines.push(format!(
                    "  с {}: {}{change}{mark}",
                    row.month,
                    if row.enabled {
                        money(row.price)
                    } else {
                        "выключено".into()
                    }
                ));
            }
            blocks.push(lines.join("\n"));
        }
        Ok(vec![Reply::with_buttons(
            from.chat_id,
            format!(
                "История тарифов (по расчётным месяцам)\n\n{}\n\nИзменить: /tariff",
                blocks.join("\n\n")
            ),
            vec![vec![("✏️ Изменить тарифы", "tnew".into())]],
        )])
    }

    /// `/tariff` без аргументов: выбор расчётного месяца (по умолчанию — текущий).
    pub(crate) async fn tariff_start(&self, from: &Sender, now: NaiveDateTime) -> Res<Vec<Reply>> {
        self.set_state(
            from.telegram_id,
            &State::Tariff {
                month: None,
                step: 0,
                answers: vec![],
            },
        )
        .await?;
        let current = period_of(now.date());
        let next = next_month(&current);
        Ok(vec![Reply::with_buttons(
            from.chat_id,
            "Новые тарифы. С какого расчётного месяца они действуют?\n\
             Тариф действует на месяц целиком и сохраняется на следующие месяцы, пока вы его не измените. \
             Выберите кнопкой или введите месяц (ГГГГ-ММ).",
            vec![
                vec![(&*format!("Этот месяц ({current})"), format!("tm:{current}"))],
                vec![(&*format!("Следующий месяц ({next})"), format!("tm:{next}"))],
                vec![
                    ("📜 История тарифов", "th".into()),
                    ("🗑 Отмена", "tx".into()),
                ],
            ],
        )])
    }

    pub(crate) async fn tariff_month(
        &self,
        from: &Sender,
        text: &str,
        _now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(month) = parse_month(text) else {
            return user_err(
                "Не получилось разобрать месяц. Введите его как ГГГГ-ММ, например 2026-11.",
            );
        };
        let Some(property_id) = self.property_id().await? else {
            return user_err("Сначала настройте объект: /setup");
        };
        self.set_state(
            from.telegram_id,
            &State::Tariff {
                month: Some(month.clone()),
                step: 0,
                answers: vec![],
            },
        )
        .await?;
        Ok(vec![
            self.tariff_prompt(from, property_id, &month, 0).await?,
        ])
    }

    async fn tariff_prompt(
        &self,
        from: &Sender,
        property_id: i64,
        month: &str,
        step: usize,
    ) -> Res<Reply> {
        let code = CODES[step];
        let row = self.tariff_at(property_id, code, month).await?;
        let sewer_on = code == "sewer" && row.as_ref().is_some_and(|r| r.enabled);
        let current = match &row {
            Some(r) if r.enabled || code != "sewer" => format!(
                "Сейчас: {} ₽ за {} (с {})",
                money(r.price),
                unit(code),
                r.month
            ),
            _ if code == "sewer" => "Сейчас: арендатор водоотведение не оплачивает".to_string(),
            _ => "Сейчас: тарифа нет".to_string(),
        };
        let keep_label = if code == "sewer" && !sewer_on {
            "Оставить выключенным"
        } else {
            "Оставить без изменений"
        };
        let mut rows = vec![vec![(keep_label, "tk".to_string())]];
        if sewer_on {
            rows.push(vec![("Выключить водоотведение", "toff".to_string())]);
        }
        rows.push(vec![("🗑 Отмена", "tx".to_string())]);
        Ok(Reply::with_buttons(
            from.chat_id,
            format!(
                "Шаг {} из {TOTAL} — {}\n{current}\nНовая цена действует с месяца {month} и дальше. Введите число (например, 66,87) или выберите действие.",
                step + 1,
                label(code)
            ),
            rows,
        ))
    }

    /// Текст владельца, пока открыто меню тарифов: месяц либо цена.
    pub(crate) async fn tariff_text(
        &self,
        from: &Sender,
        text: &str,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(State::Tariff { month, .. }) = self.state(from.telegram_id).await? else {
            return user_err("Меню тарифов закрыто. Откройте его заново: /tariff");
        };
        if month.is_none() {
            return self.tariff_month(from, text, now).await;
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
            month: Some(month),
            step,
            mut answers,
        }) = self.state(from.telegram_id).await?
        else {
            return user_err("Сначала выберите месяц: /tariff");
        };
        if step >= TOTAL {
            return user_err("Все позиции уже заполнены — нажмите «Сохранить» или «Отмена».");
        }
        let code = CODES[step];
        if answer == "off" && code != "sewer" {
            return user_err("Выключить можно только водоотведение.");
        }
        let Some(property_id) = self.property_id().await? else {
            return user_err("Сначала настройте объект: /setup");
        };
        // Цена, равная действующей в этом месяце, — не изменение: новая версия не создаётся.
        let current = self.tariff_at(property_id, code, &month).await?;
        let unchanged = match (answer, &current) {
            ("keep", _) => true,
            ("off", c) => !c.as_ref().is_some_and(|r| r.enabled),
            (p, Some(r)) => r.enabled && p.parse::<Decimal>().is_ok_and(|p| p == r.price),
            _ => false,
        };
        answers.push(if unchanged {
            "keep".to_string()
        } else {
            answer.to_string()
        });
        let step = step + 1;
        self.set_state(
            from.telegram_id,
            &State::Tariff {
                month: Some(month.clone()),
                step,
                answers: answers.clone(),
            },
        )
        .await?;
        if step < TOTAL {
            return Ok(vec![
                self.tariff_prompt(from, property_id, &month, step).await?,
            ]);
        }

        let mut lines = vec![];
        for (code, ans) in CODES.iter().zip(&answers) {
            if ans == "keep" {
                continue;
            }
            let before = self
                .tariff_at(property_id, code, &month)
                .await?
                .map_or("нет".to_string(), |r| show(&r));
            let after = if ans == "off" {
                "выключить (арендатор не оплачивает)".to_string()
            } else {
                format!(
                    "{} ₽ за {}",
                    money(ans.parse().unwrap_or_default()),
                    unit(code)
                )
            };
            lines.push(format!("• {}: {before} → {after}", label(code)));
            // следующий месяц с явной версией перекроет эту цену — лучше сказать об этом сейчас
            let versions = self.tariff_versions(property_id, code).await?;
            if let Some(later) = versions.iter().find(|v| v.month > month) {
                lines.push(format!(
                    "   ⚠️ с {} задана другая цена ({}) — с того месяца будет действовать она",
                    later.month,
                    show(later)
                ));
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
                "Проверьте новые тарифы с расчётного месяца {month} (и дальше, пока не измените):\n\n{}\n\nЧерновик за этот месяц будет пересчитан, утверждённые счета не изменятся. Сохранить?",
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
            month: Some(month),
            step,
            answers,
        }) = self.state(from.telegram_id).await?
        else {
            return Ok(vec![Reply::text(
                from.chat_id,
                "Тарифы уже сохранены или меню закрыто ✔ Проверить: /tariffs",
            )]);
        };
        if step != TOTAL || answers.len() != TOTAL {
            return user_err("Не все позиции заполнены — продолжайте по шагам.");
        }
        let Some(property_id) = self.property_id().await? else {
            return user_err("Сначала настройте объект: /setup");
        };
        let changes: Vec<(&str, Change)> = CODES
            .iter()
            .zip(&answers)
            .filter(|(_, a)| a.as_str() != "keep")
            .map(|(code, a)| {
                (
                    *code,
                    if a == "off" {
                        Change::Off
                    } else {
                        Change::Price(a.parse().unwrap_or_default())
                    },
                )
            })
            .collect();
        self.clear_state(from.telegram_id).await?;
        self.apply_tariffs(from.chat_id, property_id, &month, &changes, now)
            .await
    }

    /// `/tariff` одной строкой: `/tariff cold 66.87 [месяц]`, `/tariff sewer off [месяц]`; без аргументов — меню.
    pub(crate) async fn tariff_cmd(
        &self,
        from: &Sender,
        _u: &UserRow,
        text: &str,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(property_id) = self.property_id().await? else {
            return user_err("Сначала настройте объект: /setup");
        };
        let args: Vec<&str> = text.split_whitespace().skip(1).collect();
        if args.is_empty() {
            return self.tariff_start(from, now).await;
        }
        let usage = "Формат: /tariff <код> <цена> [месяц], например:\n/tariff cold 66.87 2026-11\nМесяц по умолчанию — текущий. Коды: cold, hot, sewer, t1, t2. \
                     Водоотведение выключается так: /tariff sewer off [месяц]. Или просто /tariff — меню по шагам.";
        let code = args[0];
        if !CODES.contains(&code) {
            return user_err(usage);
        }
        let month = match args.get(2) {
            Some(m) => parse_month(m).ok_or_else(|| AppError::User(usage.into()))?,
            None => period_of(now.date()),
        };
        let change = match args.get(1) {
            Some(&"off") if code == "sewer" => Change::Off,
            Some(p) => Change::Price(parse_decimal(p).ok_or_else(|| AppError::User(usage.into()))?),
            None => return user_err(usage),
        };
        // та же цена, что уже действует в этом месяце, — менять нечего
        let current = self.tariff_at(property_id, code, &month).await?;
        let unchanged = match (&change, &current) {
            (Change::Price(p), Some(r)) => r.enabled && r.price == *p,
            (Change::Off, c) => !c.as_ref().is_some_and(|r| r.enabled),
            _ => false,
        };
        if unchanged {
            return user_err(format!(
                "«{}»: с месяца {month} уже так — менять нечего.",
                label(code)
            ));
        }
        self.apply_tariffs(from.chat_id, property_id, &month, &[(code, change)], now)
            .await
    }

    /// Единая точка записи тарифов: версия месяца заменяется (а не дублируется), черновики пересчитываются,
    /// утверждённые и оплаченные счета не меняются.
    pub(crate) async fn apply_tariffs(
        &self,
        chat: i64,
        property_id: i64,
        month: &str,
        changes: &[(&str, Change)],
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let ts = now_iso(now);
        let mut tx = self.pool.begin().await?;
        let mut lines = vec![];
        for (code, change) in changes {
            match change {
                Change::Price(p) => {
                    insert_tariff(&mut tx, property_id, code, &p.to_string(), month, true, &ts)
                        .await?;
                    lines.push(format!(
                        "• {}: {} ₽ за {}",
                        label(code),
                        money(*p),
                        unit(code)
                    ));
                }
                Change::Off => {
                    // цену выключенного водоотведения не теряем: пригодится при повторном включении
                    let last: Option<String> = sqlx::query_scalar("SELECT price FROM tariff WHERE property_id = ? AND code = ? ORDER BY valid_from DESC LIMIT 1")
                        .bind(property_id)
                        .bind(code)
                        .fetch_optional(&mut *tx)
                        .await?;
                    insert_tariff(
                        &mut tx,
                        property_id,
                        code,
                        &last.unwrap_or_else(|| "0".into()),
                        month,
                        false,
                        &ts,
                    )
                    .await?;
                    lines.push(format!("• {}: выключено", label(code)));
                }
            }
        }
        tx.commit().await?;

        let mut text = format!(
            "✅ Тарифы сохранены. Действуют с расчётного месяца {month} и дальше, пока вы их не измените:\n{}",
            lines.join("\n")
        );
        let fixed: Vec<(String, String)> = sqlx::query_as(
            "SELECT period, status FROM bill WHERE property_id = ? AND period >= ? AND status IN ('approved', 'paid') ORDER BY period",
        )
        .bind(property_id)
        .bind(month)
        .fetch_all(&self.pool)
        .await?;
        if !fixed.is_empty() {
            let list: Vec<String> = fixed
                .iter()
                .map(|(p, s)| {
                    format!(
                        "{p} ({})",
                        if s == "paid" {
                            "оплачен"
                        } else {
                            "утверждён"
                        }
                    )
                })
                .collect();
            text.push_str(&format!(
                "\n\nℹ️ Выставленные счета не изменены: {}. Чтобы выпустить утверждённый счёт заново по новым тарифам — /reissue (оплаченные не перевыпускаются).",
                list.join(", ")
            ));
        }
        let period = self.open_period(property_id, now).await?;
        let waiting = self.received(property_id, &period).await?.len() == 4
            && !self.period_billed(property_id, &period).await?;
        let mut replies = vec![if waiting {
            Reply::with_buttons(
                chat,
                format!("{text}\n\nПоказания за {period} уже получены — можно рассчитать."),
                vec![vec![("▶️ Рассчитать период", "cc".into())]],
            )
        } else {
            Reply::text(chat, text)
        }];
        replies.extend(self.refresh_drafts(property_id, month, now).await?);
        Ok(replies)
    }
}

impl App {
    /// Несколько изменений одним запросом (Mini App): те, что ничего не меняют, отбрасываются;
    /// если не осталось ни одного — «менять нечего». Остальное — как у `/tariff`.
    pub async fn change_tariffs(
        &self,
        chat: i64,
        month: &str,
        changes: &[(&str, Change)],
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(property_id) = self.property_id().await? else {
            return user_err("Объект не настроен.");
        };
        let mut effective = vec![];
        for (code, change) in changes {
            let current = self.tariff_at(property_id, code, month).await?;
            let unchanged = match (change, &current) {
                (Change::Price(p), Some(r)) => r.enabled && r.price == *p,
                (Change::Off, c) => !c.as_ref().is_some_and(|r| r.enabled),
                _ => false,
            };
            if !unchanged {
                effective.push((*code, change.clone_change()));
            }
        }
        if effective.is_empty() {
            return user_err("Менять нечего: эти тарифы уже действуют в выбранном месяце.");
        }
        self.apply_tariffs(chat, property_id, month, &effective, now)
            .await
    }
}

impl Change {
    fn clone_change(&self) -> Change {
        self.clone()
    }
}

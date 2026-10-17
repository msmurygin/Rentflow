//! Поток арендатора: показания собираются в «форму» по шагам (ХВС → ГВС → T1 → T2), затем одна итоговая
//! сводка и одна кнопка «Отправить». Распознанные по фото значения попадают в ту же форму и подтверждаются
//! человеком на сводке вместе со всеми остальными. Ничего не считается и не уходит владельцу до «Отправить».

use crate::app::*;
use crate::ocr::Candidate;
use crate::util::*;
use chrono::NaiveDateTime;
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};

/// Черновое значение одного регистра (строка таблицы `pending_reading`).
struct Draft {
    code: String,
    value: Decimal,
    source: String,
    ai_value: Option<String>,
    ai_confidence: Option<String>,
    file_id: Option<String>,
    unique_id: Option<String>,
    photo_path: Option<String>,
}

impl Draft {
    fn has_photo(&self) -> bool {
        self.file_id.is_some()
    }
}

/// Строка формы по регистру: значение и/или уже приложенное фото (фото могло прийти раньше числа).
struct Existing {
    source: String,
    value: Option<Decimal>,
    ai_value: Option<String>,
    ai_confidence: Option<String>,
    file_id: Option<String>,
    unique_id: Option<String>,
    photo_path: Option<String>,
}

type ExistingRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

type DraftRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Регистр в форме ввода показаний (для Mini App).
#[derive(Debug, Clone, serde::Serialize)]
pub struct FormRegister {
    pub code: String,
    pub label: String,
    pub unit: String,
    pub previous: Option<String>,
    pub value: Option<String>,
    pub has_photo: bool,
    /// `manual` | `photo`: откуда число. `photo` — предложено ИИ, человек подтверждает.
    pub source: Option<String>,
    pub ai_value: Option<String>,
    pub ai_confidence: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FormView {
    pub period: String,
    /// По периоду уже есть счёт — ввод закрыт.
    pub billed: bool,
    pub proof_required: bool,
    pub ocr_enabled: bool,
    pub registers: Vec<FormRegister>,
    /// Все числа введены (и фото приложены, если они обязательны): можно отправлять.
    pub complete: bool,
}

const TOTAL: usize = REGISTERS.len();

impl App {
    /// Объект арендатора. Все дальнейшие обращения к данным идут через него: чужой объект недостижим.
    pub(crate) async fn tenancy_of(&self, user_id: i64) -> Res<i64> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT property_id FROM tenancy WHERE tenant_user_id = ?")
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some((p,)) => Ok(p),
            None => user_err("Вы не подключены ни к одному объекту."),
        }
    }

    pub(crate) async fn landlord_chat(&self, property_id: i64) -> Res<i64> {
        Ok(sqlx::query_scalar("SELECT l.chat_id FROM property p JOIN app_user l ON l.id = p.landlord_id WHERE p.id = ?")
            .bind(property_id)
            .fetch_one(&self.pool)
            .await?)
    }

    pub(crate) async fn period_billed(&self, property_id: i64, period: &str) -> Res<bool> {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM bill WHERE property_id = ? AND period = ? AND status IN ('draft','approved','paid')")
            .bind(property_id)
            .bind(period)
            .fetch_one(&self.pool)
            .await?;
        Ok(n > 0)
    }

    /// Период, в который попадают новые показания. Начатый черновик или отклонённый период продолжается, пока по нему
    /// нет действующего счёта: показания разных месяцев не смешиваются. Иначе — текущий месяц.
    pub(crate) async fn open_period(&self, property_id: i64, now: NaiveDateTime) -> Res<String> {
        let open: Option<(String,)> = sqlx::query_as(
            "SELECT period FROM (
                SELECT r.period AS period FROM reading r JOIN register g ON g.id = r.register_id
                 WHERE g.property_id = ?1 AND r.kind = 'submitted' AND r.status = 'confirmed'
                UNION
                SELECT b.period FROM bill b WHERE b.property_id = ?1 AND b.status = 'rejected'
                UNION
                SELECT p.period FROM pending_reading p JOIN tenancy t ON t.tenant_user_id = p.user_id
                 WHERE t.property_id = ?1 AND p.status = 'open')
             WHERE period NOT IN (SELECT period FROM bill WHERE property_id = ?1 AND status IN ('draft','approved','paid'))
             ORDER BY period ASC LIMIT 1",
        )
        .bind(property_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(open.map_or_else(|| period_of(now.date()), |(p,)| p))
    }

    /// Принятые (подтверждённые) показания периода по кодам регистров.
    pub(crate) async fn received(
        &self,
        property_id: i64,
        period: &str,
    ) -> Res<Vec<(String, String)>> {
        Ok(sqlx::query_as(
            "SELECT g.code, r.value FROM reading r JOIN register g ON g.id = r.register_id
              WHERE g.property_id = ? AND r.period = ? AND r.kind = 'submitted' AND r.status = 'confirmed'",
        )
        .bind(property_id)
        .bind(period)
        .fetch_all(&self.pool)
        .await?)
    }

    pub(crate) fn status_text(period: &str, got: &[(String, String)]) -> String {
        let mut lines = vec![format!("Период {period}")];
        let mut missing = vec![];
        for (code, lbl, unit) in REGISTERS {
            match got.iter().find(|g| g.0 == code) {
                Some((_, v)) => lines.push(format!(
                    "✅ {lbl}: {} {unit}",
                    num(v.parse::<Decimal>().unwrap_or_default())
                )),
                None => {
                    lines.push(format!("⬜ {lbl}: нет"));
                    missing.push(lbl);
                }
            }
        }
        if missing.is_empty() {
            lines.push("\nВсе показания приняты.".into());
        } else {
            lines.push(format!("\nНе хватает: {}.", missing.join(", ")));
        }
        lines.join("\n")
    }

    pub(crate) async fn tenant_readings(
        &self,
        from: &Sender,
        u: &UserRow,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let property_id = self.tenancy_of(u.id).await?;
        let period = self.open_period(property_id, now).await?;
        let got = self.received(property_id, &period).await?;
        let drafts = self.load_drafts(u.id, &period).await?;
        let mut text = Self::status_text(&period, &got);
        if self.period_billed(property_id, &period).await? {
            text.push_str("\nПо этому периоду расчёт уже отправлен владельцу.");
        } else if !drafts.is_empty() {
            let have: Vec<&str> = drafts.iter().map(|d| label(&d.code)).collect();
            text.push_str(&format!(
                "\n\nЧерновик (ещё не отправлен): {}.\nПродолжить: /enter",
                have.join(", ")
            ));
        }
        Ok(vec![Reply::text(from.chat_id, text)])
    }

    pub(crate) async fn landlord_status(
        &self,
        from: &Sender,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(property_id) = self.property_id().await? else {
            return user_err("Объект не настроен. Начните с /setup");
        };
        let name: String = sqlx::query_scalar("SELECT name FROM property WHERE id = ?")
            .bind(property_id)
            .fetch_one(&self.pool)
            .await?;
        let linked: Option<i64> =
            sqlx::query_scalar("SELECT tenant_user_id FROM tenancy WHERE property_id = ?")
                .bind(property_id)
                .fetch_one(&self.pool)
                .await?;
        let period = self.open_period(property_id, now).await?;
        let sewer = self.sewer_for(property_id, &period).await?.is_some();
        let got = self.received(property_id, &period).await?;
        let bills: Vec<(i64, String, String, String)> =
            sqlx::query_as("SELECT id, period, status, total FROM bill WHERE property_id = ? ORDER BY id DESC LIMIT 5")
                .bind(property_id)
                .fetch_all(&self.pool)
                .await?;
        let mut text = format!(
            "«{name}»\nАрендатор: {}\nВодоотведение: {}\n\n{}",
            if linked.is_some() {
                "подключён"
            } else {
                "не подключён (/invite)"
            },
            if sewer {
                "включено"
            } else {
                "выключено"
            },
            Self::status_text(&period, &got)
        );
        let proof = self.proof_required(property_id).await?;
        text.push_str(&format!(
            "\nФото-подтверждение: {}",
            if proof {
                "обязательно (/proof)"
            } else {
                "необязательно (/proof)"
            }
        ));
        if !bills.is_empty() {
            text.push_str("\n\nСчета:");
            for (id, p, status, total) in bills {
                let st = match status.as_str() {
                    "draft" => "ждёт вашего решения",
                    "void" => "заменён",
                    "approved" => "утверждён, ждёт оплаты",
                    "rejected" => "отклонён",
                    _ => "оплачен",
                };
                text.push_str(&format!(
                    "\n#{id} {p}: {} ₽ — {st}",
                    money(total.parse().unwrap_or_default())
                ));
            }
        }
        Ok(vec![Reply::text(from.chat_id, text)])
    }

    /// Владелец сбрасывает неполный период (например, арендатор ошибся в нескольких значениях).
    pub(crate) async fn reset_period(&self, from: &Sender, now: NaiveDateTime) -> Res<Vec<Reply>> {
        let Some(property_id) = self.property_id().await? else {
            return user_err("Объект не настроен.");
        };
        let period = self.open_period(property_id, now).await?;
        // Что делать, зависит от того, в каком состоянии счёт: кнопка «Отклонить» есть только у черновика.
        let active: Option<String> = sqlx::query_scalar(
            "SELECT status FROM bill WHERE property_id = ? AND period = ? AND status IN ('draft','approved','paid')",
        )
        .bind(property_id)
        .bind(&period)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(status) = active {
            return user_err(match status.as_str() {
                "draft" => format!(
                    "За {period} есть черновик счёта. Отклоните его кнопкой «Отклонить» под расчётом — показания откроются заново."
                ),
                "approved" => format!(
                    "Счёт за {period} уже утверждён и отправлен арендатору, сбросить показания нельзя. \
                     Пересчитать по текущим тарифам — /reissue, аннулировать (например, тестовый счёт) — /void."
                ),
                _ => format!(
                    "Счёт за {period} уже оплачен — сбросить показания нельзя. Если счёт тестовый или ошибочный, аннулируйте его: /void (с причиной и отдельным подтверждением)."
                ),
            });
        }
        self.supersede_period(property_id, &period).await?;
        Ok(vec![Reply::text(
            from.chat_id,
            format!("Показания периода {period} сброшены. Арендатор может ввести их заново."),
        )])
    }

    pub(crate) async fn supersede_period(&self, property_id: i64, period: &str) -> Res<()> {
        sqlx::query(
            "UPDATE reading SET status = 'superseded'
              WHERE period = ? AND kind = 'submitted' AND status = 'confirmed'
                AND register_id IN (SELECT id FROM register WHERE property_id = ?)",
        )
        .bind(period)
        .bind(property_id)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "UPDATE pending_reading SET status = 'discarded' WHERE period = ? AND status = 'open'",
        )
        .bind(period)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---------------------------------------------------------------------------------------------
    // Форма ввода
    // ---------------------------------------------------------------------------------------------

    async fn load_drafts(&self, user_id: i64, period: &str) -> Res<Vec<Draft>> {
        let rows: Vec<DraftRow> = sqlx::query_as(
            "SELECT register_code, value, source, ai_confidence, file_id, unique_id, photo_path, ai_value
               FROM pending_reading
              WHERE user_id = ? AND period = ? AND status = 'open' AND register_code IS NOT NULL AND value IS NOT NULL
              ORDER BY id",
        )
        .bind(user_id)
        .bind(period)
        .fetch_all(&self.pool)
        .await?;
        let mut drafts: Vec<Draft> = rows
            .into_iter()
            .filter_map(
                |(code, value, source, ai_confidence, file_id, unique_id, photo_path, ai_value)| {
                    Some(Draft {
                        code,
                        value: value.parse().ok()?,
                        source,
                        ai_value,
                        ai_confidence,
                        file_id,
                        unique_id,
                        photo_path,
                    })
                },
            )
            .collect();
        drafts.sort_by_key(|d| REGISTERS.iter().position(|r| r.0 == d.code));
        Ok(drafts)
    }

    /// Последнее подтверждённое показание регистра до этого периода (или стартовое).
    async fn previous_value(
        &self,
        property_id: i64,
        code: &str,
        period: &str,
    ) -> Res<Option<Decimal>> {
        let v: Option<(String,)> = sqlx::query_as(
            "SELECT r.value FROM reading r JOIN register g ON g.id = r.register_id
              WHERE g.property_id = ? AND g.code = ? AND r.status = 'confirmed' AND r.period < ?
              ORDER BY r.period DESC LIMIT 1",
        )
        .bind(property_id)
        .bind(code)
        .bind(period)
        .fetch_optional(&self.pool)
        .await?;
        Ok(v.and_then(|(v,)| v.parse().ok()))
    }

    /// `Some(текст)`, если значение меньше предыдущего (счётчик не крутится назад).
    async fn rejection(
        &self,
        property_id: i64,
        code: &str,
        period: &str,
        value: Decimal,
    ) -> Res<Option<String>> {
        Ok(
            match self.previous_value(property_id, code, period).await? {
                Some(prev) if value.floor() < prev.floor() => Some(format!(
                    "{}: {} меньше прошлого показания ({}). Счётчик не крутится назад — проверьте цифры.",
                    label(code),
                    num(value),
                    num(prev)
                )),
                _ => None,
            },
        )
    }

    /// Сохраняет черновое значение регистра (заменяя предыдущее черновое для того же регистра).
    #[allow(clippy::too_many_arguments)]
    async fn save_draft(
        &self,
        user_id: i64,
        period: &str,
        code: &str,
        value: Decimal,
        source: &str,
        ai: Option<(&str, &str)>,
        photo: Option<(&str, Option<&str>, Option<&str>)>,
        now: NaiveDateTime,
    ) -> Res<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE pending_reading SET status = 'discarded' WHERE user_id = ? AND period = ? AND register_code = ? AND status = 'open'")
            .bind(user_id)
            .bind(period)
            .bind(code)
            .execute(&mut *tx)
            .await?;
        let (file_id, unique_id, photo_path) =
            photo.map_or((None, None, None), |(f, u, p)| (Some(f), u, p));
        sqlx::query(
            "INSERT INTO pending_reading
               (user_id, period, register_code, value, ai_value, ai_confidence, file_id, unique_id, photo_path, source, status, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'open', ?)",
        )
        .bind(user_id)
        .bind(period)
        .bind(code)
        .bind(value.to_string())
        .bind(ai.map(|a| a.0))
        .bind(ai.map(|a| a.1))
        .bind(file_id)
        .bind(unique_id)
        .bind(photo_path)
        .bind(source)
        .bind(now_iso(now))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn proof_required(&self, property_id: i64) -> Res<bool> {
        Ok(
            sqlx::query_scalar("SELECT proof_required FROM property WHERE id = ?")
                .bind(property_id)
                .fetch_one(&self.pool)
                .await?,
        )
    }

    /// Строка формы по регистру (с числом, с фото или с тем и другим).
    async fn existing_row(&self, user_id: i64, period: &str, code: &str) -> Res<Option<Existing>> {
        let row: Option<ExistingRow> = sqlx::query_as(
            "SELECT source, value, ai_value, ai_confidence, file_id, unique_id, photo_path
               FROM pending_reading
              WHERE user_id = ? AND period = ? AND register_code = ? AND status = 'open' LIMIT 1",
        )
        .bind(user_id)
        .bind(period)
        .bind(code)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(source, value, ai_value, ai_confidence, file_id, unique_id, photo_path)| Existing {
                source,
                value: value.and_then(|v| v.parse().ok()),
                ai_value,
                ai_confidence,
                file_id,
                unique_id,
                photo_path,
            },
        ))
    }

    /// Прикрепляет фото-подтверждение к регистру: к уже введённому значению либо отдельной строкой «пока без числа».
    #[allow(clippy::too_many_arguments)]
    async fn attach_proof(
        &self,
        user_id: i64,
        period: &str,
        code: &str,
        file_id: &str,
        unique_id: &str,
        photo_path: Option<&str>,
        ai: Option<(&str, &str)>,
        now: NaiveDateTime,
    ) -> Res<()> {
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE pending_reading
                SET file_id = ?, unique_id = ?, photo_path = ?,
                    ai_value = COALESCE(?, ai_value), ai_confidence = COALESCE(?, ai_confidence)
              WHERE user_id = ? AND period = ? AND register_code = ? AND status = 'open'",
        )
        .bind(file_id)
        .bind(unique_id)
        .bind(photo_path)
        .bind(ai.map(|a| a.0))
        .bind(ai.map(|a| a.1))
        .bind(user_id)
        .bind(period)
        .bind(code)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            sqlx::query(
                "INSERT INTO pending_reading
                   (user_id, period, register_code, ai_value, ai_confidence, file_id, unique_id, photo_path, source, status, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'manual', 'open', ?)",
            )
            .bind(user_id)
            .bind(period)
            .bind(code)
            .bind(ai.map(|a| a.0))
            .bind(ai.map(|a| a.1))
            .bind(file_id)
            .bind(unique_id)
            .bind(photo_path)
            .bind(now_iso(now))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Записывает введённое значение в форму. Правка числа не теряет уже приложенное фото и исходное
    /// предложение ИИ: они остаются в истории. Возвращает, было ли к регистру фото.
    pub(crate) async fn store_value(
        &self,
        user_id: i64,
        period: &str,
        code: &str,
        value: Decimal,
        now: NaiveDateTime,
    ) -> Res<bool> {
        let existing = self
            .existing_row(user_id, period, code)
            .await?
            .filter(|e| e.file_id.is_some());
        match &existing {
            Some(e) => {
                let ai = e.ai_value.as_deref().zip(e.ai_confidence.as_deref());
                let photo = Some((
                    e.file_id.as_deref().unwrap_or(""),
                    e.unique_id.as_deref(),
                    e.photo_path.as_deref(),
                ));
                self.save_draft(user_id, period, code, value, &e.source, ai, photo, now)
                    .await?;
            }
            None => {
                self.save_draft(user_id, period, code, value, "manual", None, None, now)
                    .await?
            }
        }
        Ok(existing.is_some())
    }

    /// Это фото уже учтено (подтверждённым показанием или открытой формой)?
    async fn photo_used(&self, unique_id: &str) -> Res<bool> {
        let used: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM reading WHERE photo_unique_id = ?1 AND status = 'confirmed')
                  + (SELECT count(*) FROM pending_reading WHERE unique_id = ?1 AND status = 'open')",
        )
        .bind(unique_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(used > 0)
    }

    /// Состояние формы ввода показаний арендатора: период, по каждому регистру — прошлое, введённое, фото, подсказка ИИ.
    pub async fn form_view(&self, user_id: i64, now: NaiveDateTime) -> Res<FormView> {
        let property_id = self.tenancy_of(user_id).await?;
        let period = self.open_period(property_id, now).await?;
        let billed = self.period_billed(property_id, &period).await?;
        let mut registers = vec![];
        for (code, lbl, unit) in REGISTERS {
            let row = self.existing_row(user_id, &period, code).await?;
            registers.push(FormRegister {
                code: code.into(),
                label: lbl.into(),
                unit: unit.into(),
                previous: self
                    .previous_value(property_id, code, &period)
                    .await?
                    // машинный формат (точка): локализует и считает клиент; «0,5» ломало расчёт расхода на сводке
                    .map(|d| d.to_string()),
                value: row.as_ref().and_then(|r| r.value).map(|v| v.to_string()),
                has_photo: row.as_ref().is_some_and(|r| r.file_id.is_some()),
                source: row.as_ref().map(|r| r.source.clone()),
                ai_value: row.as_ref().and_then(|r| r.ai_value.clone()),
                ai_confidence: row.as_ref().and_then(|r| r.ai_confidence.clone()),
            });
        }
        let proof_required = self.proof_required(property_id).await?;
        let complete = registers
            .iter()
            .all(|r| r.value.is_some() && (r.has_photo || !proof_required));
        Ok(FormView {
            period,
            billed,
            proof_required,
            ocr_enabled: self.cfg.ocr_enabled,
            registers,
            complete,
        })
    }

    /// Значение регистра, введённое или исправленное вручную (проверка «не меньше прошлого» — здесь же).
    pub async fn form_set_value(
        &self,
        user_id: i64,
        code: &str,
        raw: &str,
        now: NaiveDateTime,
    ) -> Res<()> {
        if !is_register(code) {
            return user_err("Неизвестный счётчик.");
        }
        let property_id = self.tenancy_of(user_id).await?;
        let period = self.open_period(property_id, now).await?;
        if self.period_billed(property_id, &period).await? {
            return user_err(format!(
                "За {period} расчёт уже отправлен владельцу — новые показания не принимаются."
            ));
        }
        let Some(value) = parse_decimal(raw) else {
            return user_err("Нужно число, например 761 или 761,5.");
        };
        if let Some(msg) = self.rejection(property_id, code, &period, value).await? {
            return user_err(msg);
        }
        self.store_value(user_id, &period, code, value, now).await?;
        Ok(())
    }

    /// Фото счётчика `code`: сохраняется как подтверждение; ИИ только предлагает значение (если ручного ещё нет),
    /// человек подтверждает или исправляет. Недоступный ИИ не мешает: фото остаётся, число вводится вручную.
    pub async fn form_photo(
        &self,
        user_id: i64,
        code: &str,
        bytes: &[u8],
        now: NaiveDateTime,
    ) -> Res<()> {
        if !is_register(code) {
            return user_err("Неизвестный счётчик.");
        }
        if bytes.is_empty() {
            return user_err("Пустой файл.");
        }
        let property_id = self.tenancy_of(user_id).await?;
        let period = self.open_period(property_id, now).await?;
        if self.period_billed(property_id, &period).await? {
            return user_err(format!(
                "За {period} расчёт уже отправлен владельцу — новые показания не принимаются."
            ));
        }
        let unique = hex::encode(Sha256::digest(bytes));
        if self.photo_used(&unique).await? {
            return user_err(
                "Это фото уже принято (или уже в вашей форме). Одно фото нельзя использовать дважды.",
            );
        }
        let path = self.save_photo(&unique, bytes).await;
        let file_id = format!("local:{unique}");
        let ocr = self.extractor.extract(bytes).await;
        if let Err(e) = &ocr {
            tracing::warn!(error = %e, "photo recognition failed, falling back to manual entry");
        }
        let recognised = ocr
            .as_ref()
            .ok()
            .and_then(|c| c.iter().find(|c| c.register.as_deref() == Some(code)))
            .and_then(|c| {
                Some((
                    parse_decimal(c.value.as_deref()?)?,
                    c.value.clone()?,
                    c.confidence.clone(),
                ))
            });
        let typed = self
            .existing_row(user_id, &period, code)
            .await?
            .and_then(|e| e.value);
        match recognised {
            Some((value, raw, conf)) if typed.is_none() => {
                if self
                    .rejection(property_id, code, &period, value)
                    .await?
                    .is_none()
                {
                    self.save_draft(
                        user_id,
                        &period,
                        code,
                        value,
                        "photo",
                        Some((&raw, &conf)),
                        Some((&file_id, Some(&unique), path.as_deref())),
                        now,
                    )
                    .await?;
                } else {
                    self.attach_proof(
                        user_id,
                        &period,
                        code,
                        &file_id,
                        &unique,
                        path.as_deref(),
                        None,
                        now,
                    )
                    .await?;
                }
            }
            other => {
                let ai = other.as_ref().map(|(_, r, c)| (r.as_str(), c.as_str()));
                self.attach_proof(
                    user_id,
                    &period,
                    code,
                    &file_id,
                    &unique,
                    path.as_deref(),
                    ai,
                    now,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Путь к фото регистра в открытой форме этого арендатора.
    pub async fn form_photo_path(
        &self,
        user_id: i64,
        code: &str,
        now: NaiveDateTime,
    ) -> Res<Option<String>> {
        let property_id = self.tenancy_of(user_id).await?;
        let period = self.open_period(property_id, now).await?;
        Ok(self
            .existing_row(user_id, &period, code)
            .await?
            .and_then(|e| e.photo_path))
    }

    /// Приглашение ввести значение для регистра `code`.
    async fn prompt(
        &self,
        from: &Sender,
        property_id: i64,
        period: &str,
        code: &str,
        edit: bool,
    ) -> Res<Reply> {
        let idx = REGISTERS.iter().position(|r| r.0 == code).unwrap_or(0);
        let prev = match self.previous_value(property_id, code, period).await? {
            Some(p) => format!("Прошлое показание: {} {}", num(p), unit(code)),
            None => "Прошлого показания нет".to_string(),
        };
        let head = if edit {
            format!("✏️ {}", label(code))
        } else {
            format!("Шаг {} из {TOTAL} — {}", idx + 1, label(code))
        };
        let proof = if self.proof_required(property_id).await? {
            "\n📎 К показанию нужно приложить фото счётчика — пришлите его до или после числа."
        } else {
            "\n📎 Можно приложить фото счётчика как подтверждение (необязательно)."
        };
        Ok(Reply::with_buttons(
            from.chat_id,
            format!(
                "{head}\n{prev}\nВведите текущее показание числом (например, 761 или 761,5).{proof}"
            ),
            vec![vec![("🗑 Отмена", "cx".into())]],
        ))
    }

    /// Сводка всех четырёх значений с расходом и кнопкой «Отправить».
    async fn summary(
        &self,
        from: &Sender,
        property_id: i64,
        period: &str,
        drafts: &[Draft],
    ) -> Res<Reply> {
        let required = self.proof_required(property_id).await?;
        let mut lines = vec![format!("Проверьте показания за {period}:\n")];
        let mut no_photo = vec![];
        for (code, lbl, unit) in REGISTERS {
            let Some(d) = drafts.iter().find(|d| d.code == code) else {
                continue;
            };
            let usage = match self.previous_value(property_id, code, period).await? {
                Some(p) => format!(" (расход {} {unit})", num(d.value.floor() - p.floor())),
                None => String::new(),
            };
            let mark = if d.has_photo() {
                if matches!(d.ai_confidence.as_deref(), Some("low") | Some("medium"))
                    && d.source == "photo"
                {
                    " 📎⚠️ сверьте с фото"
                } else {
                    " 📎"
                }
            } else if required {
                no_photo.push(lbl);
                " ❌ нет фото"
            } else {
                ""
            };
            lines.push(format!("• {lbl}: {} {unit}{usage}{mark}", num(d.value)));
        }
        let mut rows = vec![];
        if no_photo.is_empty() {
            lines.push("\nВсё верно? Нажмите «Отправить» — расчёт уйдёт владельцу вместе с фото. Ошиблись в цифре — нажмите ✏️ рядом со счётчиком.".into());
            rows.push(vec![("✅ Отправить показания", "send".to_string())]);
        } else {
            lines.push(format!(
                "\nВладелец требует фото-подтверждение. Не хватает фото: {}.\nНажмите ✏️ нужного счётчика и пришлите фото.",
                no_photo.join(", ")
            ));
        }
        rows.push(vec![
            ("✏️ ХВС", "ed:cold".to_string()),
            ("✏️ ГВС", "ed:hot".to_string()),
        ]);
        rows.push(vec![
            ("✏️ T1", "ed:t1".to_string()),
            ("✏️ T2", "ed:t2".to_string()),
        ]);
        rows.push(vec![("🗑 Отмена", "cx".to_string())]);
        Ok(Reply::with_buttons(from.chat_id, lines.join("\n"), rows))
    }

    /// Следующий шаг формы: приглашение к первому незаполненному регистру либо итоговая сводка.
    async fn next_step(
        &self,
        from: &Sender,
        property_id: i64,
        period: &str,
        user_id: i64,
        lead: Option<String>,
    ) -> Res<Vec<Reply>> {
        let drafts = self.load_drafts(user_id, period).await?;
        let mut replies = vec![];
        if let Some(text) = lead.filter(|t| !t.is_empty()) {
            replies.push(Reply::text(from.chat_id, text));
        }
        match REGISTERS
            .iter()
            .find(|r| !drafts.iter().any(|d| d.code == r.0))
        {
            Some((code, _, _)) => {
                self.set_state(
                    from.telegram_id,
                    &State::Entry {
                        code: (*code).into(),
                        edit: false,
                    },
                )
                .await?;
                replies.push(self.prompt(from, property_id, period, code, false).await?);
            }
            None => {
                self.clear_state(from.telegram_id).await?;
                replies.push(self.summary(from, property_id, period, &drafts).await?);
            }
        }
        Ok(replies)
    }

    /// Шаг для `code` закончен (число и, если нужно, фото есть): возвращаемся к сводке (правка) или идём дальше.
    async fn finish_step(
        &self,
        from: &Sender,
        property_id: i64,
        period: &str,
        user_id: i64,
        edit: bool,
        lead: String,
    ) -> Res<Vec<Reply>> {
        if edit {
            self.clear_state(from.telegram_id).await?;
            let drafts = self.load_drafts(user_id, period).await?;
            if drafts.len() == TOTAL {
                let mut replies = vec![Reply::text(from.chat_id, lead)];
                replies.push(self.summary(from, property_id, period, &drafts).await?);
                return Ok(replies);
            }
        }
        self.next_step(from, property_id, period, user_id, Some(lead))
            .await
    }

    /// `/proof`: сделать фото-подтверждение обязательным или необязательным.
    pub(crate) async fn proof_cmd(&self, from: &Sender) -> Res<Vec<Reply>> {
        let Some(property_id) = self.property_id().await? else {
            return user_err("Объект не настроен. Начните с /setup");
        };
        let on = self.proof_required(property_id).await?;
        Ok(vec![Reply::with_buttons(
            from.chat_id,
            format!(
                "Фото-подтверждение показаний сейчас: {}.\n\nОбязательно — арендатор не сможет отправить показания, пока не приложит фото к каждому счётчику. Необязательно — фото можно приложить по желанию. Фото приходят вам вместе с расчётом.",
                if on {
                    "ОБЯЗАТЕЛЬНО"
                } else {
                    "необязательно"
                }
            ),
            vec![vec![
                ("📎 Обязательно", "pr:on".into()),
                ("Необязательно", "pr:off".into()),
            ]],
        )])
    }

    pub(crate) async fn proof_set(&self, from: &Sender, mode: &str) -> Res<Vec<Reply>> {
        let Some(property_id) = self.property_id().await? else {
            return user_err("Объект не настроен. Начните с /setup");
        };
        let on = match mode {
            "on" => true,
            "off" => false,
            _ => return user_err("Неизвестный режим."),
        };
        sqlx::query("UPDATE property SET proof_required = ? WHERE id = ?")
            .bind(on)
            .bind(property_id)
            .execute(&self.pool)
            .await?;
        Ok(vec![Reply::text(
            from.chat_id,
            if on {
                "✅ Фото-подтверждение обязательно: без фото ко всем четырём счётчикам показания отправить нельзя."
            } else {
                "Фото-подтверждение необязательно."
            },
        )])
    }

    pub(crate) async fn entry_start(
        &self,
        from: &Sender,
        u: &UserRow,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let property_id = self.tenancy_of(u.id).await?;
        let period = self.open_period(property_id, now).await?;
        if self.period_billed(property_id, &period).await? {
            return user_err(format!(
                "За {period} расчёт уже отправлен владельцу — новые показания не принимаются."
            ));
        }
        let drafts = self.load_drafts(u.id, &period).await?;
        let lead = if drafts.is_empty() {
            format!("Ввод показаний за {period}. Отмена в любой момент: /cancel")
        } else {
            format!("Продолжаем ввод показаний за {period}.")
        };
        self.next_step(from, property_id, &period, u.id, Some(lead))
            .await
    }

    pub(crate) async fn entry_edit(
        &self,
        from: &Sender,
        u: &UserRow,
        code: &str,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        if !is_register(code) {
            return user_err("Неизвестный счётчик.");
        }
        let property_id = self.tenancy_of(u.id).await?;
        let period = self.open_period(property_id, now).await?;
        self.set_state(
            from.telegram_id,
            &State::Entry {
                code: code.into(),
                edit: true,
            },
        )
        .await?;
        Ok(vec![
            self.prompt(from, property_id, &period, code, true).await?,
        ])
    }

    pub(crate) async fn entry_cancel(
        &self,
        from: &Sender,
        u: &UserRow,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let property_id = self.tenancy_of(u.id).await?;
        let period = self.open_period(property_id, now).await?;
        sqlx::query("UPDATE pending_reading SET status = 'discarded' WHERE user_id = ? AND period = ? AND status = 'open'")
            .bind(u.id)
            .bind(&period)
            .execute(&self.pool)
            .await?;
        self.clear_state(from.telegram_id).await?;
        Ok(vec![Reply::with_buttons(
            from.chat_id,
            "Ввод отменён, введённые значения удалены.",
            vec![vec![("▶️ Ввести показания заново", "enter".into())]],
        )])
    }

    /// Текст арендатора: число для текущего шага формы.
    pub(crate) async fn tenant_text(
        &self,
        from: &Sender,
        u: &UserRow,
        text: &str,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let (code, edit) = match self.state(from.telegram_id).await? {
            Some(State::Entry { code, edit }) | Some(State::Proof { code, edit }) => (code, edit),
            _ => {
                return Ok(vec![Reply::with_buttons(
                    from.chat_id,
                    "Чтобы ввести показания, нажмите /enter или кнопку ниже. Можно прислать фото счётчика.",
                    vec![vec![("▶️ Ввести показания", "enter".into())]],
                )]);
            }
        };
        let property_id = self.tenancy_of(u.id).await?;
        let period = self.open_period(property_id, now).await?;
        if self.period_billed(property_id, &period).await? {
            self.clear_state(from.telegram_id).await?;
            return user_err(format!(
                "За {period} расчёт уже отправлен владельцу — новые показания не принимаются."
            ));
        }
        let Some(value) = parse_decimal(text) else {
            return user_err(format!(
                "Нужно число, например 761 или 761,5.\n{}: введите показание ещё раз (или /cancel).",
                label(&code)
            ));
        };
        if let Some(msg) = self.rejection(property_id, &code, &period, value).await? {
            return user_err(format!("{msg}\nВведите показание ещё раз."));
        }
        let had_photo = self.store_value(u.id, &period, &code, value, now).await?;
        let accepted = format!("✔ {}: {} {}", label(&code), num(value), unit(&code));
        if !had_photo && self.proof_required(property_id).await? {
            // число есть, фото ещё нет — ждём фото именно к этому счётчику
            self.set_state(
                from.telegram_id,
                &State::Proof {
                    code: code.clone(),
                    edit,
                },
            )
            .await?;
            return Ok(vec![Reply::with_buttons(
                from.chat_id,
                format!(
                    "{accepted}\n📎 Теперь пришлите фото счётчика «{}» — оно нужно владельцу как подтверждение.\nОшиблись в числе? Просто отправьте правильное.",
                    label(&code)
                ),
                vec![vec![("🗑 Отмена", "cx".into())]],
            )]);
        }
        self.finish_step(from, property_id, &period, u.id, edit, accepted)
            .await
    }

    pub(crate) async fn tenant_photo(
        &self,
        from: &Sender,
        u: &UserRow,
        file_id: &str,
        unique_id: &str,
        bytes: &[u8],
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let property_id = self.tenancy_of(u.id).await?;
        let period = self.open_period(property_id, now).await?;
        if self.period_billed(property_id, &period).await? {
            return user_err(format!(
                "За {period} расчёт уже отправлен владельцу — новые показания не принимаются."
            ));
        }
        if self.photo_used(unique_id).await? {
            return user_err(
                "Это фото уже принято (или уже в вашей форме). Одно фото нельзя использовать дважды: /readings",
            );
        }
        let target = match self.state(from.telegram_id).await? {
            Some(State::Entry { code, edit }) | Some(State::Proof { code, edit }) => {
                Some((code, edit))
            }
            _ => None,
        };

        let photo_path = self.save_photo(unique_id, bytes).await;
        let ocr = self.extractor.extract(bytes).await;
        if let Err(e) = &ocr {
            tracing::warn!(error = %e, "photo recognition failed, falling back to manual entry");
        }
        match target {
            Some((code, edit)) => {
                self.photo_for_step(
                    from,
                    u,
                    property_id,
                    &period,
                    &code,
                    edit,
                    file_id,
                    unique_id,
                    photo_path.as_deref(),
                    ocr,
                    now,
                )
                .await
            }
            None => {
                self.photo_free(
                    from,
                    u,
                    property_id,
                    &period,
                    file_id,
                    unique_id,
                    photo_path.as_deref(),
                    ocr,
                    now,
                )
                .await
            }
        }
    }

    /// Фото пришло, пока открыт шаг конкретного счётчика: это подтверждение именно к нему.
    #[allow(clippy::too_many_arguments)]
    async fn photo_for_step(
        &self,
        from: &Sender,
        u: &UserRow,
        property_id: i64,
        period: &str,
        code: &str,
        edit: bool,
        file_id: &str,
        unique_id: &str,
        photo_path: Option<&str>,
        ocr: Result<Vec<Candidate>, String>,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let existing = self.existing_row(u.id, period, code).await?;
        let typed = existing.as_ref().and_then(|e| e.value);
        // Распознавание — только подсказка; учитывается лишь значение именно этого счётчика.
        let recognised = ocr
            .as_ref()
            .ok()
            .and_then(|cands| cands.iter().find(|c| c.register.as_deref() == Some(code)))
            .and_then(|c| {
                Some((
                    parse_decimal(c.value.as_deref()?)?,
                    c.value.clone()?,
                    c.confidence.clone(),
                ))
            });

        if let Some(typed) = typed {
            // число уже введено вручную: фото — доказательство к нему; ИИ сверяет
            let ai = recognised
                .as_ref()
                .map(|(_, raw, conf)| (raw.as_str(), conf.as_str()));
            self.attach_proof(u.id, period, code, file_id, unique_id, photo_path, ai, now)
                .await?;
            let mut lead = format!("📎 Фото прикреплено к «{}».", label(code));
            if let Some((seen, _, _)) = &recognised
                && seen.floor() != typed.floor()
            {
                lead.push_str(&format!(
                    "\n⚠️ На фото я вижу {}, а вы ввели {}. Проверьте цифры (✏️ на сводке).",
                    num(*seen),
                    num(typed)
                ));
            }
            return self
                .finish_step(from, property_id, period, u.id, edit, lead)
                .await;
        }

        // числа пока нет: берём подсказку ИИ, если она проходит проверку на прошлое показание
        if let Some((value, raw, conf)) = recognised
            && self
                .rejection(property_id, code, period, value)
                .await?
                .is_none()
        {
            self.save_draft(
                u.id,
                period,
                code,
                value,
                "photo",
                Some((&raw, &conf)),
                Some((file_id, Some(unique_id), photo_path)),
                now,
            )
            .await?;
            let lead = format!(
                "📷 Распознано: {} = {} {}. Проверьте на итоговой сводке.",
                label(code),
                num(value),
                unit(code)
            );
            return self
                .finish_step(from, property_id, period, u.id, edit, lead)
                .await;
        }
        // не распознано (или ИИ выключен): фото сохраняем как подтверждение и просим число
        self.attach_proof(
            u.id, period, code, file_id, unique_id, photo_path, None, now,
        )
        .await?;
        self.set_state(
            from.telegram_id,
            &State::Entry {
                code: code.into(),
                edit,
            },
        )
        .await?;
        let why = if ocr.is_err() {
            ""
        } else {
            " Цифры на нём я не смог уверенно прочитать."
        };
        let mut reply = self.prompt(from, property_id, period, code, edit).await?;
        reply.text = format!(
            "📎 Фото сохранено как подтверждение для «{}».{why}\n\n{}",
            label(code),
            reply.text
        );
        Ok(vec![reply])
    }

    /// Фото пришло вне шага: распознаём, какие счётчики на нём, и заполняем форму.
    #[allow(clippy::too_many_arguments)]
    async fn photo_free(
        &self,
        from: &Sender,
        u: &UserRow,
        property_id: i64,
        period: &str,
        file_id: &str,
        unique_id: &str,
        photo_path: Option<&str>,
        ocr: Result<Vec<Candidate>, String>,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let candidates = match ocr {
            Ok(c) => c,
            Err(_) => {
                let lead = "Автоматическое распознавание сейчас недоступно. Введите показания вручную по шагам и прикладывайте фото к каждому счётчику.".to_string();
                return self
                    .next_step(from, property_id, period, u.id, Some(lead))
                    .await;
            }
        };
        if candidates.is_empty() {
            let lead = "Не нашёл на фото показание счётчика. Введите показания по шагам; фото к каждому счётчику можно приложить на его шаге.".to_string();
            return self
                .next_step(from, property_id, period, u.id, Some(lead))
                .await;
        }
        let mut notes = vec![];
        let mut photo_attached = false;
        for c in candidates.iter().take(2) {
            let code = c.register.clone().filter(|r| is_register(r));
            let value = c.value.as_deref().and_then(parse_decimal);
            let (Some(code), Some(value)) = (code, value) else {
                notes.push("⚠️ На фото не удалось надёжно определить счётчик или цифры — введите это показание вручную.".to_string());
                continue;
            };
            if let Some(msg) = self.rejection(property_id, &code, period, value).await? {
                notes.push(format!("⚠️ По фото: {msg}\nНе принято. Возможно, распознан не тот регистр — введите значение вручную."));
                continue;
            }
            // уникальный id фото привязываем к первому принятому значению (второе значение с того же фото его не дублирует)
            let unique = (!photo_attached).then_some(unique_id);
            photo_attached = true;
            self.save_draft(
                u.id,
                period,
                &code,
                value,
                "photo",
                Some((c.value.as_deref().unwrap_or(""), c.confidence.as_str())),
                Some((file_id, unique, photo_path)),
                now,
            )
            .await?;
            notes.push(format!(
                "📷 Распознано: {} = {} {}. Проверьте на итоговой сводке.",
                label(&code),
                num(value),
                unit(&code)
            ));
        }
        self.next_step(from, property_id, period, u.id, Some(notes.join("\n")))
            .await
    }

    async fn save_photo(&self, unique_id: &str, bytes: &[u8]) -> Option<String> {
        let dir = self.cfg.photo_dir.as_ref()?;
        let safe: String = unique_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        let path = dir.join(format!("{safe}.jpg"));
        match tokio::fs::create_dir_all(dir).await {
            Ok(()) => match tokio::fs::write(&path, bytes).await {
                Ok(()) => Some(path.to_string_lossy().into_owned()),
                Err(e) => {
                    tracing::warn!(error = %e, "could not save photo");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "could not create photo dir");
                None
            }
        }
    }

    /// «Отправить»: все четыре значения записываются как подтверждённые показания одной транзакцией,
    /// после чего считается черновик счёта и уходит владельцу.
    pub(crate) async fn entry_submit(
        &self,
        from: &Sender,
        u: &UserRow,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let property_id = self.tenancy_of(u.id).await?;
        let period = self.open_period(property_id, now).await?;
        if self.period_billed(property_id, &period).await? {
            return Ok(vec![Reply::text(
                from.chat_id,
                "Показания уже отправлены владельцу ✔ Статус: /readings",
            )]);
        }
        let drafts = self.load_drafts(u.id, &period).await?;
        if drafts.is_empty() {
            return Ok(vec![Reply::text(
                from.chat_id,
                "Нечего отправлять. Начните ввод: /enter",
            )]);
        }
        let missing: Vec<&str> = REGISTERS
            .iter()
            .filter(|r| !drafts.iter().any(|d| d.code == r.0))
            .map(|r| r.1)
            .collect();
        if !missing.is_empty() {
            return user_err(format!(
                "Не хватает: {}. Продолжить ввод: /enter",
                missing.join(", ")
            ));
        }
        if self.proof_required(property_id).await? {
            let bare: Vec<&str> = drafts
                .iter()
                .filter(|d| !d.has_photo())
                .map(|d| label(&d.code))
                .collect();
            if !bare.is_empty() {
                return user_err(format!(
                    "Владелец требует фото-подтверждение. Не хватает фото: {}.\nНажмите ✏️ нужного счётчика на сводке и пришлите фото.",
                    bare.join(", ")
                ));
            }
        }
        for d in &drafts {
            if let Some(msg) = self
                .rejection(property_id, &d.code, &period, d.value)
                .await?
            {
                return user_err(format!("{msg}\nИсправьте значение кнопкой ✏️."));
            }
        }

        let mut tx = self.pool.begin().await?;
        // Занимаем черновики атомарно: второе параллельное «Отправить» получит 0 строк и ничего не запишет.
        let claimed = sqlx::query("UPDATE pending_reading SET status = 'confirmed' WHERE user_id = ? AND period = ? AND status = 'open'")
            .bind(u.id)
            .bind(&period)
            .execute(&mut *tx)
            .await?;
        if claimed.rows_affected() as usize != drafts.len() {
            return Ok(vec![Reply::text(from.chat_id, "Уже отправлено ✔")]);
        }
        for d in &drafts {
            let register_id: i64 =
                sqlx::query_scalar("SELECT id FROM register WHERE property_id = ? AND code = ?")
                    .bind(property_id)
                    .bind(&d.code)
                    .fetch_one(&mut *tx)
                    .await?;
            sqlx::query("UPDATE reading SET status = 'superseded' WHERE register_id = ? AND period = ? AND status = 'confirmed'")
                .bind(register_id)
                .bind(&period)
                .execute(&mut *tx)
                .await?;
            let inserted = sqlx::query(
                "INSERT INTO reading (register_id, period, kind, value, read_on, source, photo_file_id, photo_unique_id, photo_path,
                                      ai_value, ai_confidence, status, confirmed_by, created_at)
                 VALUES (?, ?, 'submitted', ?, ?, ?, ?, ?, ?, ?, ?, 'confirmed', ?, ?)",
            )
            .bind(register_id)
            .bind(&period)
            .bind(d.value.to_string())
            .bind(now.date().to_string())
            .bind(&d.source)
            .bind(&d.file_id)
            .bind(&d.unique_id)
            .bind(&d.photo_path)
            .bind(&d.ai_value)
            .bind(&d.ai_confidence)
            .bind(u.id)
            .bind(now_iso(now))
            .execute(&mut *tx)
            .await;
            if let Err(sqlx::Error::Database(e)) = &inserted
                && e.is_unique_violation()
            {
                return user_err("Это фото уже принято ранее. Повторно оно не учитывается.");
            }
            inserted?;
        }
        tx.commit().await?;
        self.clear_state(from.telegram_id).await?;

        let mut replies = vec![];
        match self.create_draft(property_id, &period, now).await {
            Ok(draft) => {
                replies.push(Reply::text(
                    from.chat_id,
                    "✅ Показания отправлены. Владелец проверит расчёт — итог придёт вам после его подтверждения.",
                ));
                replies.push(self.draft_to_landlord(property_id, &draft).await?);
            }
            Err(AppError::User(reason)) => {
                replies.push(Reply::text(
                    from.chat_id,
                    "✅ Показания отправлены. Расчёт пока невозможен — владелец уведомлён.",
                ));
                let chat = self.landlord_chat(property_id).await?;
                replies.push(Reply::text(
                    chat,
                    format!("Арендатор прислал все показания за {period}, но расчёт не выполнен:\n{reason}\n\nИсправьте причину (например, /tariff) и нажмите /calc."),
                ));
            }
            Err(e) => return Err(e),
        }
        Ok(replies)
    }

    /// Владелец повторно запускает расчёт открытого периода, когда все показания уже приняты.
    pub(crate) async fn calc_cmd(&self, from: &Sender, now: NaiveDateTime) -> Res<Vec<Reply>> {
        let Some(property_id) = self.property_id().await? else {
            return user_err("Объект не настроен.");
        };
        let period = self.open_period(property_id, now).await?;
        if self.period_billed(property_id, &period).await? {
            return user_err(format!("По периоду {period} счёт уже есть: /status"));
        }
        let got = self.received(property_id, &period).await?;
        if got.len() < TOTAL {
            return user_err(format!(
                "Показания неполные.\n\n{}",
                Self::status_text(&period, &got)
            ));
        }
        let draft = self.create_draft(property_id, &period, now).await?;
        let mut reply = self.draft_to_landlord(property_id, &draft).await?;
        reply.chat_id = from.chat_id;
        Ok(vec![reply])
    }
}

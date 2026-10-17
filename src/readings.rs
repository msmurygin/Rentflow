//! Поток арендатора: фото → распознавание → подтверждение человеком → подтверждённое показание.

use crate::app::*;
use crate::ocr::Candidate;
use crate::util::*;
use chrono::NaiveDateTime;
use rust_decimal::Decimal;

struct Pending {
    id: i64,
    period: String,
    register_code: Option<String>,
    value: Option<String>,
    ai_value: Option<String>,
    ai_confidence: Option<String>,
    file_id: Option<String>,
    unique_id: Option<String>,
    photo_path: Option<String>,
    source: String,
}

type PendingRow = (
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

const PENDING_COLS: &str = "id, period, register_code, value, ai_value, ai_confidence, file_id, unique_id, photo_path, source";

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

    /// Период, в который попадают новые показания. Неоконченный или отклонённый период продолжается,
    /// пока по нему нет действующего счёта: показания разных периодов не смешиваются. Иначе — текущий месяц.
    pub(crate) async fn open_period(&self, property_id: i64, now: NaiveDateTime) -> Res<String> {
        let open: Option<(String,)> = sqlx::query_as(
            "SELECT period FROM (
                SELECT r.period AS period FROM reading r JOIN register g ON g.id = r.register_id
                 WHERE g.property_id = ?1 AND r.kind = 'submitted' AND r.status = 'confirmed'
                UNION
                SELECT b.period FROM bill b WHERE b.property_id = ?1 AND b.status = 'rejected')
             WHERE period NOT IN (SELECT period FROM bill WHERE property_id = ?1 AND status IN ('draft','approved','paid'))
             ORDER BY period ASC LIMIT 1",
        )
        .bind(property_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(open.map_or_else(|| period_of(now.date()), |(p,)| p))
    }

    /// Принятые показания периода по кодам регистров.
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
        let mut text = Self::status_text(&period, &got);
        if self.period_billed(property_id, &period).await? {
            text.push_str("\nПо этому периоду расчёт уже отправлен владельцу.");
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
        let (name, sewer): (String, bool) =
            sqlx::query_as("SELECT name, sewer_enabled FROM property WHERE id = ?")
                .bind(property_id)
                .fetch_one(&self.pool)
                .await?;
        let linked: Option<i64> =
            sqlx::query_scalar("SELECT tenant_user_id FROM tenancy WHERE property_id = ?")
                .bind(property_id)
                .fetch_one(&self.pool)
                .await?;
        let period = self.open_period(property_id, now).await?;
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
        if !bills.is_empty() {
            text.push_str("\n\nСчета:");
            for (id, p, status, total) in bills {
                let st = match status.as_str() {
                    "draft" => "ждёт вашего решения",
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
        if self.period_billed(property_id, &period).await? {
            return user_err(
                "По этому периоду уже есть счёт. Отклоните его кнопкой «Отклонить» — показания откроются заново.",
            );
        }
        self.supersede_period(property_id, &period).await?;
        Ok(vec![Reply::text(
            from.chat_id,
            format!("Показания периода {period} сброшены. Арендатор может прислать их заново."),
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

    async fn new_pending(
        &self,
        user_id: i64,
        period: &str,
        source: &str,
        now: NaiveDateTime,
    ) -> Res<i64> {
        Ok(sqlx::query("INSERT INTO pending_reading (user_id, period, source, status, created_at) VALUES (?, ?, ?, 'open', ?)")
            .bind(user_id)
            .bind(period)
            .bind(source)
            .bind(now_iso(now))
            .execute(&self.pool)
            .await?
            .last_insert_rowid())
    }

    async fn load_pending(&self, user_id: i64, id: i64) -> Res<Pending> {
        let row: Option<PendingRow> = sqlx::query_as(&format!(
            "SELECT {PENDING_COLS} FROM pending_reading WHERE id = ? AND user_id = ? AND status = 'open'"
        ))
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some((
                id,
                period,
                register_code,
                value,
                ai_value,
                ai_confidence,
                file_id,
                unique_id,
                photo_path,
                source,
            )) => Ok(Pending {
                id,
                period,
                register_code,
                value,
                ai_value,
                ai_confidence,
                file_id,
                unique_id,
                photo_path,
                source,
            }),
            None => user_err("Эта карточка уже обработана или недоступна."),
        }
    }

    /// Карточка показания: что именно ждём от человека на этом шаге.
    async fn show_pending(&self, from: &Sender, p: &Pending, header: &str) -> Res<Reply> {
        match (&p.register_code, &p.value) {
            (None, _) => Ok(register_picker(from.chat_id, p.id, header)),
            (Some(code), None) => {
                self.set_state(from.telegram_id, &State::AwaitValue { pending_id: p.id })
                    .await?;
                Ok(Reply::with_buttons(
                    from.chat_id,
                    format!(
                        "{header}\n{}: введите показание цифрами (например, 761 или 761,5).",
                        label(code)
                    ),
                    vec![vec![("🗑 Отмена", format!("no:{}", p.id))]],
                ))
            }
            (Some(code), Some(v)) => {
                let shown = num(v.parse::<Decimal>().unwrap_or_default());
                let warn = if p.ai_confidence.as_deref() == Some("low") {
                    "\n⚠️ Распознавание неуверенное — сверьтесь с фото."
                } else {
                    ""
                };
                Ok(Reply::with_buttons(
                    from.chat_id,
                    format!(
                        "{header}\n{}: {shown} {}{warn}\n\nВсё верно?",
                        label(code),
                        unit(code)
                    ),
                    vec![
                        vec![
                            ("✅ Верно", format!("ok:{}", p.id)),
                            ("✏️ Исправить значение", format!("fix:{}", p.id)),
                        ],
                        vec![
                            ("🔁 Другой счётчик", format!("chg:{}", p.id)),
                            ("🗑 Отмена", format!("no:{}", p.id)),
                        ],
                    ],
                ))
            }
        }
    }

    pub(crate) async fn tenant_enter(
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
        let id = self.new_pending(u.id, &period, "manual", now).await?;
        Ok(vec![register_picker(
            from.chat_id,
            id,
            "Ручной ввод показания.",
        )])
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
        let used: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM reading WHERE photo_unique_id = ? AND status = 'confirmed'",
        )
        .bind(unique_id)
        .fetch_one(&self.pool)
        .await?;
        if used > 0 {
            return user_err("Это фото уже принято ранее. Повторно оно не учитывается.");
        }
        let open_dup: Option<(i64,)> =
            sqlx::query_as("SELECT id FROM pending_reading WHERE user_id = ? AND unique_id = ? AND status = 'open'")
                .bind(u.id)
                .bind(unique_id)
                .fetch_optional(&self.pool)
                .await?;
        if let Some((id,)) = open_dup {
            let p = self.load_pending(u.id, id).await?;
            return Ok(vec![
                self.show_pending(from, &p, "Это фото уже на проверке:")
                    .await?,
            ]);
        }

        let photo_path = self.save_photo(unique_id, bytes).await;
        let (candidates, note) = match self.extractor.extract(bytes).await {
            Ok(c) if !c.is_empty() => (c, "📷 Распознано автоматически — проверьте по фото."),
            Ok(_) => (
                vec![],
                "Не удалось распознать показание на фото. Введите его вручную.",
            ),
            Err(e) => {
                tracing::warn!(error = %e, "photo recognition failed, falling back to manual entry");
                (
                    vec![],
                    "Автоматическое распознавание сейчас недоступно. Введите показание вручную.",
                )
            }
        };
        let mut replies = vec![];
        let candidates: Vec<Candidate> = if candidates.is_empty() {
            vec![Candidate {
                register: None,
                value: None,
                confidence: "low".into(),
            }]
        } else {
            candidates.into_iter().take(2).collect()
        };
        for (i, c) in candidates.iter().enumerate() {
            let register = c.register.clone().filter(|r| is_register(r));
            let value = c
                .value
                .as_deref()
                .and_then(parse_decimal)
                .map(|d| d.to_string());
            let id = sqlx::query(
                "INSERT INTO pending_reading
                   (user_id, period, register_code, value, ai_value, ai_confidence, file_id, unique_id, photo_path, source, status, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'photo', 'open', ?)",
            )
            .bind(u.id)
            .bind(&period)
            .bind(&register)
            .bind(&value)
            .bind(&c.value)
            .bind(&c.confidence)
            .bind(file_id)
            // уникальный id фото привязываем к первой карточке; вторая карточка того же фото его не дублирует
            .bind(if i == 0 { Some(unique_id) } else { None })
            .bind(&photo_path)
            .bind(now_iso(now))
            .execute(&self.pool)
            .await?
            .last_insert_rowid();
            let p = self.load_pending(u.id, id).await?;
            replies.push(self.show_pending(from, &p, note).await?);
        }
        Ok(replies)
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

    pub(crate) async fn tenant_text(
        &self,
        from: &Sender,
        u: &UserRow,
        text: &str,
        _now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let Some(State::AwaitValue { pending_id }) = self.state(from.telegram_id).await? else {
            return Ok(vec![Reply::text(
                from.chat_id,
                "Отправьте фото счётчика или нажмите /enter, чтобы ввести показание вручную.",
            )]);
        };
        let p = self.load_pending(u.id, pending_id).await?;
        let Some(value) = parse_decimal(text) else {
            return user_err(
                "Нужно число, например 761 или 761,5. Попробуйте ещё раз или /cancel.",
            );
        };
        sqlx::query(
            "UPDATE pending_reading SET value = ? WHERE id = ? AND user_id = ? AND status = 'open'",
        )
        .bind(value.to_string())
        .bind(pending_id)
        .bind(u.id)
        .execute(&self.pool)
        .await?;
        self.clear_state(from.telegram_id).await?;
        let p = Pending {
            value: Some(value.to_string()),
            ..p
        };
        Ok(vec![
            self.show_pending(from, &p, "Проверьте введённое значение:")
                .await?,
        ])
    }

    pub(crate) async fn pending_fix(&self, from: &Sender, u: &UserRow, id: i64) -> Res<Vec<Reply>> {
        let p = self.load_pending(u.id, id).await?;
        let Some(code) = &p.register_code else {
            return Ok(vec![register_picker(
                from.chat_id,
                id,
                "Сначала выберите счётчик.",
            )]);
        };
        self.set_state(from.telegram_id, &State::AwaitValue { pending_id: id })
            .await?;
        Ok(vec![Reply::with_buttons(
            from.chat_id,
            format!("{}: введите правильное показание цифрами.", label(code)),
            vec![vec![("🗑 Отмена", format!("no:{id}"))]],
        )])
    }

    pub(crate) async fn pending_pick_register(
        &self,
        from: &Sender,
        u: &UserRow,
        id: i64,
    ) -> Res<Vec<Reply>> {
        self.load_pending(u.id, id).await?;
        Ok(vec![register_picker(
            from.chat_id,
            id,
            "Выберите, к какому счётчику относится показание:",
        )])
    }

    pub(crate) async fn pending_set_register(
        &self,
        from: &Sender,
        u: &UserRow,
        id: i64,
        code: &str,
    ) -> Res<Vec<Reply>> {
        let p = self.load_pending(u.id, id).await?;
        if !is_register(code) {
            return user_err("Неизвестный счётчик.");
        }
        sqlx::query("UPDATE pending_reading SET register_code = ? WHERE id = ? AND user_id = ? AND status = 'open'")
            .bind(code)
            .bind(id)
            .bind(u.id)
            .execute(&self.pool)
            .await?;
        let p = Pending {
            register_code: Some(code.to_string()),
            ..p
        };
        Ok(vec![self.show_pending(from, &p, "Счётчик выбран.").await?])
    }

    pub(crate) async fn pending_discard(
        &self,
        from: &Sender,
        u: &UserRow,
        id: i64,
    ) -> Res<Vec<Reply>> {
        self.load_pending(u.id, id).await?;
        sqlx::query("UPDATE pending_reading SET status = 'discarded' WHERE id = ? AND user_id = ?")
            .bind(id)
            .bind(u.id)
            .execute(&self.pool)
            .await?;
        self.clear_state(from.telegram_id).await?;
        Ok(vec![Reply::text(
            from.chat_id,
            "Показание отменено. Отправьте фото заново или нажмите /enter.",
        )])
    }

    pub(crate) async fn pending_confirm(
        &self,
        from: &Sender,
        u: &UserRow,
        id: i64,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let p = match self.load_pending(u.id, id).await {
            Ok(p) => p,
            // повторное нажатие «Верно» — не ошибка и не второе показание
            Err(AppError::User(_)) => {
                return Ok(vec![Reply::text(from.chat_id, "Уже подтверждено ✔")]);
            }
            Err(e) => return Err(e),
        };
        let property_id = self.tenancy_of(u.id).await?;
        let (Some(code), Some(raw)) = (p.register_code.clone(), p.value.clone()) else {
            return user_err("Сначала выберите счётчик и введите значение.");
        };
        let value: Decimal = raw
            .parse()
            .map_err(|_| AppError::User("Некорректное значение.".into()))?;
        if self.period_billed(property_id, &p.period).await? {
            return user_err(format!(
                "За {} расчёт уже отправлен владельцу — показания не меняются.",
                p.period
            ));
        }
        // Регистр берётся только из объекта арендатора — по коду, не по идентификатору из сообщения.
        let register_id: i64 =
            sqlx::query_scalar("SELECT id FROM register WHERE property_id = ? AND code = ?")
                .bind(property_id)
                .bind(&code)
                .fetch_one(&self.pool)
                .await?;
        let prev: Option<(String,)> = sqlx::query_as(
            "SELECT value FROM reading WHERE register_id = ? AND status = 'confirmed' AND period < ? ORDER BY period DESC LIMIT 1",
        )
        .bind(register_id)
        .bind(&p.period)
        .fetch_optional(&self.pool)
        .await?;
        if let Some((prev,)) = prev {
            let prev: Decimal = prev.parse().unwrap_or_default();
            if value.floor() < prev.floor() {
                return Ok(vec![Reply::with_buttons(
                    from.chat_id,
                    format!(
                        "{}: {} меньше предыдущего показания ({}). Так не бывает — проверьте цифры на фото.",
                        label(&code),
                        num(value),
                        num(prev)
                    ),
                    vec![vec![
                        ("✏️ Исправить значение", format!("fix:{id}")),
                        ("🗑 Отмена", format!("no:{id}")),
                    ]],
                )]);
            }
        }

        let mut tx = self.pool.begin().await?;
        // Занимаем карточку атомарно: второй параллельный «Верно» получит 0 строк и ничего не запишет.
        let claimed = sqlx::query("UPDATE pending_reading SET status = 'confirmed' WHERE id = ? AND user_id = ? AND status = 'open'")
            .bind(id)
            .bind(u.id)
            .execute(&mut *tx)
            .await?;
        if claimed.rows_affected() != 1 {
            return Ok(vec![Reply::text(from.chat_id, "Уже подтверждено ✔")]);
        }
        sqlx::query("UPDATE reading SET status = 'superseded' WHERE register_id = ? AND period = ? AND status = 'confirmed'")
            .bind(register_id)
            .bind(&p.period)
            .execute(&mut *tx)
            .await?;
        let inserted = sqlx::query(
            "INSERT INTO reading (register_id, period, kind, value, read_on, source, photo_file_id, photo_unique_id, photo_path,
                                  ai_value, ai_confidence, status, confirmed_by, created_at)
             VALUES (?, ?, 'submitted', ?, ?, ?, ?, ?, ?, ?, ?, 'confirmed', ?, ?)",
        )
        .bind(register_id)
        .bind(&p.period)
        .bind(value.to_string())
        .bind(now.date().to_string())
        .bind(&p.source)
        .bind(&p.file_id)
        .bind(&p.unique_id)
        .bind(&p.photo_path)
        .bind(&p.ai_value)
        .bind(&p.ai_confidence)
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
        tx.commit().await?;
        self.clear_state(from.telegram_id).await?;
        self.progress(from, property_id, &p.period, &code, value, now)
            .await
    }

    /// После каждого подтверждения: либо сообщаем, чего не хватает, либо, когда всё есть, считаем черновик.
    async fn progress(
        &self,
        from: &Sender,
        property_id: i64,
        period: &str,
        code: &str,
        value: Decimal,
        now: NaiveDateTime,
    ) -> Res<Vec<Reply>> {
        let got = self.received(property_id, period).await?;
        let accepted = format!(
            "✅ Принято: {} = {} {}",
            label(code),
            num(value),
            unit(code)
        );
        if got.len() < REGISTERS.len() {
            return Ok(vec![Reply::text(
                from.chat_id,
                format!("{accepted}\n\n{}", Self::status_text(period, &got)),
            )]);
        }
        let mut replies = vec![];
        match self.create_draft(property_id, period, now).await {
            Ok(draft) => {
                replies.push(Reply::text(
                    from.chat_id,
                    format!("{accepted}\n\nВсе показания получены. Расчёт отправлен владельцу на проверку — итог придёт вам после его подтверждения."),
                ));
                replies.push(self.draft_to_landlord(property_id, &draft).await?);
            }
            Err(AppError::User(reason)) => {
                replies.push(Reply::text(
                    from.chat_id,
                    format!("{accepted}\n\nВсе показания получены. Расчёт пока невозможен — владелец уведомлён."),
                ));
                let chat = self.landlord_chat(property_id).await?;
                replies.push(Reply::text(chat, format!("Арендатор прислал все показания за {period}, но расчёт не выполнен:\n{reason}\n\nИсправьте причину (например, /tariff) и выполните /calc.")));
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
        if got.len() < REGISTERS.len() {
            return user_err(format!(
                "Показания неполные.\n\n{}",
                Self::status_text(&period, &got)
            ));
        }
        let draft = self.create_draft(property_id, &period, now).await?;
        Ok(vec![self.draft_to_landlord(property_id, &draft).await?]
            .into_iter()
            .map(|mut r| {
                r.chat_id = from.chat_id;
                r
            })
            .collect())
    }
}

fn register_picker(chat_id: i64, id: i64, header: &str) -> Reply {
    Reply::with_buttons(
        chat_id,
        header.to_string(),
        vec![
            vec![
                ("💧 ХВС", format!("r:{id}:cold")),
                ("🔥 ГВС", format!("r:{id}:hot")),
            ],
            vec![
                ("⚡ Электричество T1", format!("r:{id}:t1")),
                ("⚡ Электричество T2", format!("r:{id}:t2")),
            ],
            vec![("🗑 Отмена", format!("no:{id}"))],
        ],
    )
}

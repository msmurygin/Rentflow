# v0.3 — развёртывание и откат на Hetzner (`rentflow-prod-01`)

Правило: **ничего не выкатывается автоматически после push.** Каждый этап — резервная копия → проверка на копии → ручное
подтверждение → выкладка → smoke-тесты → наблюдение. Никогда не выполнять `docker compose down -v` (удаляет данные).

## 0. Один раз: подготовка резервных копий (веха 1)

1. На своём компьютере: `age-keygen -o rentflow-backup.key` — **закрытый ключ хранить вне сервера** (менеджер паролей).
   Публичный ключ (`age1…`) пойдёт в `.env`.
2. На сервере: `apt install age rclone`; `rclone config` — удалённое хранилище вне VPS (Hetzner Storage Box по SFTP или S3).
3. В `.env`: `OFFSITE_EXPECTED=1`, `BACKUP_REMOTE=<remote>:rentflow`, `AGE_RECIPIENT=age1…`, `ADMIN_TELEGRAM_ID=<ваш id>`.
4. Ночной запуск (`crontab -e`): `10 3 * * * cd /opt/rentflow && deploy/backup-offsite.sh >> /var/log/rentflow-backup.log 2>&1`.
5. Внешний мониторинг (UptimeRobot/Healthchecks): `https://<домен>/readyz` — отвечает 503, если не отвечает база/Telegram,
   мало места или копии устарели. Если сервер лежит целиком, об этом скажет только внешняя проверка.
6. **Проверка восстановления до Beta** (повторять после каждого релиза схемы):
   `deploy/restore-staging.sh <копия.tar.gz.age> rentflow-backup.key` — расшифрует копию, проверит контрольные суммы,
   восстановит в отдельный том и применит миграции текущего образа. Production-том и бот не затрагиваются.

## 1. Релиз (на каждый этап)

```bash
cd /opt/rentflow
git fetch && git log --oneline -1 origin/claude/dev          # какой коммит выкладываем — записать
git rev-parse HEAD > /var/backups/rentflow/previous_release   # коммит, на который откатываемся
```
1. **Резервная копия перед релизом.** Первый переход на v0.3 (старый образ ещё без команды `backup`):
   `deploy/backup.sh` (останавливает бота на несколько секунд, копирует том целиком). Дальше — без остановки:
   `docker compose exec -T rentflow rentflow backup` и `deploy/backup-offsite.sh`. **Убедитесь, что копия лежит вне сервера.**
2. **Проверка миграций на копии:** `deploy/restore-staging.sh …` (п. 0.6) с образом нового релиза
   (`docker compose build rentflow` собирает его, не запуская).
3. **Ручное подтверждение:** результат п. 2 «миграции применены, база в порядке» и число строк совпадает с production.
4. **Выкладка:** `git checkout <коммит>` → `docker compose up -d --build`. Один процесс бота: не запускайте второй `rentflow`
   с тем же токеном (Telegram вернёт 409). Прерывание polling — секунды.
5. **Smoke-тесты:** `curl -fsS https://<домен>/healthz` и `/readyz` → `ok`; в Telegram: бот отвечает на `/status`;
   Mini App открывается кнопкой меню; владелец видит квартиру, арендатор — свой счёт; `docker compose logs rentflow --tail 50`
   без ошибок миграций.
6. **Наблюдение:** сутки следите за `/readyz` и сообщениями администратору; затем решение — продолжать или откатить.

## 2. Откат

Миграции v0.3 только добавляют таблицы/колонки (expand), но **старый бинарник на новой схеме запускать нельзя без проверки**,
а если выпуск уже принимал новые данные, простой запуск старого образа потеряет согласованность. Поэтому:

**A. Откат без потери данных (если миграция была только расширяющей и новых данных нет):**
`git checkout $(cat /var/backups/rentflow/previous_release) && docker compose up -d --build`.

**B. Откат восстановлением снимка (если схема/данные повреждены или нужен возврат к состоянию до релиза):**
1. `docker compose stop rentflow`.
2. Сохранить текущий том как улику: `docker run --rm -v rentflow_rentflow-data:/d -v $PWD:/b alpine tar czf /b/failed-$(date +%s).tgz -C /d .`
3. Взять копию «до релиза»; проверить: `docker compose run --rm --entrypoint rentflow rentflow verify-backup /data/backups/<имя>`
   (или расшифровать из offsite: `age -d -i rentflow-backup.key <файл> | tar xz`).
4. Восстановить в **пустой** том: `docker volume create rentflow_new && docker run --rm -v rentflow_new:/data -v <каталог_копии>:/in:ro --entrypoint rentflow <старый_образ> restore /in /data`
   (`restore` откажется писать в непустой каталог — данные не перезапишутся случайно).
5. Переключить `docker-compose.yml` на том `rentflow_new` (или переименовать тома), `git checkout <прежний коммит>`, `docker compose up -d --build`.
6. Smoke-тесты п. 1.5.

**Риск:** всё, что произошло после снимка (показания, счета, оплаты, регистрации), при откате B **теряется**.
Перед откатом B сообщите владельцам; сохранённый том из шага 2 позволяет вручную вернуть отдельные записи.

## 3. Флаги и переменные

| Переменная | Назначение |
|---|---|
| `ADMIN_TELEGRAM_ID` | получатель алертов мониторинга; прав на чужие данные не даёт |
| `ALLOW_VOID_PAID=1` | разрешает аннулировать **оплаченный** счёт (только для тестовых данных; по умолчанию выключено) |
| `OFFSITE_EXPECTED=1` | `/readyz` требует свежую отметку выгрузки копий (≤36 ч) |
| `BACKUP_REMOTE`, `AGE_RECIPIENT`, `BACKUP_RETENTION_DAYS` | `deploy/backup-offsite.sh` |

## 4. Веха 2 (мультиарендность): заметки к релизу

**Миграция `0008_membership.sql`** — только расширяющая: новая таблица `membership` и колонка `app_user.active_property_id`.
Существующие ID объектов, пользователей, счетов не меняются. Права переносятся из записанного в базе:
`property.landlord_id` → `owner`, `tenancy.tenant_user_id` → `tenant`. Никому права не назначаются по знанию Telegram ID
(проверено тестом `tests/migration_membership.rs`).

**Что меняется в поведении:**
- `LANDLORD_TELEGRAM_ID` **больше не даёт прав** (читается только как получатель алертов, если не задан `ADMIN_TELEGRAM_ID`).
  Прежний владелец остаётся владельцем, потому что записан в `property.landlord_id`.
- Любой пользователь Telegram может стать владельцем сам (`/setup` или кнопка «Я сдаю квартиру»); арендатором — только по приглашению.
- Один пользователь может иметь несколько квартир и быть арендатором в чужой (`/objects` в боте, заголовок `X-Property-Id` в API).
- Чужой объект/счёт/фото неотличим от несуществующего (404); неутверждённый счёт арендатору не виден.

**Порядок выкатки (поверх п. 1):** резервная копия → `restore-staging.sh` с новым образом (проверка, что 0008 применяется к
копии production и число объектов/счетов не изменилось) → выкладка → smoke: прежний владелец видит свою квартиру и счета
(`/status`, `/reissue`), прежний арендатор открывает приложение, **посторонний Telegram-аккаунт ничего не видит**.

**Откат:** схема 0008 расширяющая, поэтому до появления первых новых владельцев можно вернуть предыдущий образ (вариант A выше):
он игнорирует `membership`. **После регистрации новых владельцев предыдущий образ их не знает** (считает «незнакомцами» и
показывает им первый объект) — в этом случае откат только вариантом B (восстановление снимка) с потерей данных после снимка.
Поэтому первые сутки после релиза не приглашайте новых владельцев.

## Milestone 4 — migration 0010 and behaviour changes

- Migration `0010_recurring.sql` is additive (`reading_last`, `payment_days`, `bill.due_date`, `notification_log`); no data is rewritten. Existing approved bills get `due_date` NULL (no overdue).
- Reminders: tenant at the due day and +2 days, owner when readings are still missing; reading day 29–31 means the last day of the month; time is evaluated in the property's own timezone.
- Payment deadline: set per property (`payment_days`), `due_date` is fixed at approval; overdue is computed in the property timezone and also triggers a reminder.
- Delivery failures are logged in `notification_log` (safe reason codes only, no message text); more than 5 in 24h alerts `ADMIN_TELEGRAM_ID` ("notify").
- Rollback: the image can be rolled back (new tables/columns are ignored by old code); no restore needed.

## Milestone 5 — заметки к релизу

Миграций нет, переменных окружения не добавлено. Выкладка — по п. 1; обязательно после накопленных 0008–0010:
`deploy/restore-staging.sh` с образом релиза на копии production.

Smoke после выкладки (дополнительно): `/privacy` в боте отвечает; в Mini App у незнакомого аккаунта работает «Посмотреть демо»
(виден баннер; кнопки изменения дают «Демо только для просмотра»); `docker compose exec -T rentflow rentflow metrics`
выводит JSON. Демо-квартира создаётся при первом нажатии (одна запись `property` у пользователя с отрицательным Telegram-ID).

**Откат:** образ M5 схему не меняет — вариант A (`git checkout $(cat /var/backups/rentflow/previous_release) && docker compose up -d --build`).
Старый образ демо-пользователей не знает: если демо уже создано, они остаются в базе как неактивные записи (безвредно).
Если нужно убрать демо целиком — см. `docs/v0.3/PRIVACY.md`/админ: удалить квартиры `landlord_id` пользователя с `telegram_id = -1000` после копии базы.

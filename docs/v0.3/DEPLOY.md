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

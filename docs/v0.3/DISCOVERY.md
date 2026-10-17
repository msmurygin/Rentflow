# v0.3 — разведка кода (факты, не гипотезы)

Снято с ветки `claude/dev` (база — `main` @ 26ff7d7).

## Существующие сущности (БД, SQLite, миграции 0001–0007)
| Логическая модель спецификации | Что есть в коде | Решение |
|---|---|---|
| User | `app_user` (telegram_id UNIQUE, chat_id, `role` landlord/tenant, display_name) | **Переиспользуем**; колонка `role` становится устаревшей, права — только из membership |
| Membership | **нет** (владелец = `property.landlord_id`, арендатор = `tenancy.tenant_user_id`) | Новая таблица `membership`, заполняется миграцией из существующих данных |
| Property | `property` (landlord_id, name, currency, sewer_enabled, proof_required, reading_day) | Переиспользуем; добавляем status/timezone/active |
| Meter / Register | `register` (property_id, code cold/hot/t1/t2, label, unit). Физический счётчик (T1+T2) не моделируется | **ACTION FOR AGENT**: принадлежность T1/T2 одному прибору хранить колонкой `register.meter_group`, а не новой сущностью |
| Reading / Photo | `reading` (kind baseline/submitted, status, photo_path…), `pending_reading` (форма) | Переиспользуем |
| Tariff | `tariff` (property_id, code, valid_from=YYYY-MM-01, enabled), уникально на месяц | Переиспользуем (Monthly Tariff Policy) |
| Bill / Bill Line | `bill` (status draft/approved/rejected/paid/void, snapshot JSON со строками) | Переиспользуем; «Voided» = статус `void` |
| Payment Confirmation | `bill.paid_at` | Переиспользуем |
| Notification Schedule | `property.reading_day`, `reminder_log` | Переиспользуем |
| Invite | `invite` (token, property_id, expires_at, used_*) | Переиспользуем; роль цели — арендатор |
| Session | `api_session` (token_hash) | Переиспользуем |
| Audit | `audit_log` | Переиспользуем |

## Где код предполагает «один объект / один владелец»
- `App::property_id()` — глобально «первый объект» (setup.rs, tariffs.rs ×6, readings.rs ×4, reminders.rs, api/mod.rs ×3).
- `App::who()` и `accepts_photos_from()` — особая ветка `cfg.landlord_telegram_id` (минует membership).
- `create_property()` — запрещает второй объект; `setup_start()` — тоже.
- `app_user.role` — один пользователь не может быть владельцем и арендатором одновременно.
- `tenancy.property_id UNIQUE` — один арендатор на объект (оставляем: в v0.3 нужен один арендатор на квартиру).

## Уже реализовано и переиспользуется (не переписывать)
`/void` (bot + `POST /api/bills/{id}/void`), `/reset`, `/reissue`, `/schedule` + `reminder_log`, месячные тарифы, снимок счёта, `audit_log`, initData-аутентификация, `bill_one_active`.

## Расхождения со спецификацией (решения)
- §7: «Paid не разрешается void». Раньше по прямой просьбе владельца для тестового счёта добавлен void оплаченного с отдельным подтверждением. **Теперь выключено по умолчанию** (`ALLOW_VOID_PAID=1` включает, только для тестовых данных).
- §8: напоминание «в день и через 2 дня», `reading_day` 29–31 → последний день месяца, срок оплаты и overdue — реализуется в вехе 4 (сейчас: накануне / в срок / +1..3 дня).

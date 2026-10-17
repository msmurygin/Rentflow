# v0.3 — этапы и статус

| # | Этап | Статус |
|---|---|---|
| 1 | Stabilization & Safety: void по спецификации, backup/restore, health/alerts | **готово**: ALLOW_VOID_PAID, backup/verify/restore/migrate-check + тесты, offsite-скрипт (age+rclone), /readyz, алерты, DEPLOY.md. Не проверено на живом сервере: offsite-скрипты, docker healthcheck |
| 2 | Multi-Tenant Core: membership, property-scoped авторизация, миграция prod, изоляция | **готово**: миграция 0008, права из membership (бот + API), несколько квартир, смешанные роли, self-register владельца, тесты изоляции (проверены мутациями) и миграции на prod-подобных данных. Не проверено: на копии реального production (нужен `restore-staging.sh`) |
| 3 | Self-Service Mini App: регистрация, мастер с resume, переключение квартир | **в работе**: бэкенд готов (черновик мастера, идемпотентное «Готово», отключаемые регистры T2/ГВС…, часовой пояс, приём приглашения из приложения, тесты). Не сделано: экраны Svelte (выбор роли, мастер, переключатель квартир, онбординг) |
| 4 | Recurring Operations: timezone объекта, reading_day 29–31, срок оплаты/overdue, история | — |
| 5 | Private Beta Readiness: demo, метрики, приватность | — |

Работа ведётся только в ветке `claude/dev`. В `main` не коммитим; на production ничего не выкатывается автоматически.

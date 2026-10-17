# v0.3 — этапы и статус

| # | Этап | Статус |
|---|---|---|
| 1 | Stabilization & Safety: void по спецификации, backup/restore, health/alerts | **частично**: готовы void-флаг, backup/verify/restore (код), /readyz, алерты; нет тестов backup/restore, offsite-скриптов и DEPLOY.md |
| 2 | Multi-Tenant Core: membership, property-scoped авторизация, миграция prod, изоляция | — |
| 3 | Self-Service Mini App: регистрация, мастер с resume, переключение квартир | — |
| 4 | Recurring Operations: timezone объекта, reading_day 29–31, срок оплаты/overdue, история | — |
| 5 | Private Beta Readiness: demo, метрики, приватность | — |

Работа ведётся только в ветке `claude/dev`. В `main` не коммитим; на production ничего не выкатывается автоматически.

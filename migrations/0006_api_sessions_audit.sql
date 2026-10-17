-- Mini App: серверные сессии (хранится хеш токена, не сам токен) и журнал изменений.
CREATE TABLE api_session (
    token_hash  TEXT PRIMARY KEY,
    telegram_id INTEGER NOT NULL,
    name        TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL
);
CREATE INDEX api_session_expiry ON api_session (expires_at);

-- Кто, когда и что изменил: тарифы, счета, настройки объекта, приглашения. Только добавление.
CREATE TABLE audit_log (
    id          INTEGER PRIMARY KEY,
    at          TEXT NOT NULL,
    actor       INTEGER NOT NULL,
    action      TEXT NOT NULL,
    entity      TEXT NOT NULL,
    entity_id   TEXT,
    details     TEXT
);

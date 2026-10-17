-- RentFlow MVP (SQLite). Деньги и показания — строки с десятичными числами (rust_decimal), даты — ISO-текст.

CREATE TABLE app_user (
    id           INTEGER PRIMARY KEY,
    telegram_id  INTEGER NOT NULL UNIQUE,
    chat_id      INTEGER NOT NULL,
    role         TEXT NOT NULL CHECK (role IN ('landlord', 'tenant')),
    display_name TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL
);

CREATE TABLE property (
    id            INTEGER PRIMARY KEY,
    landlord_id   INTEGER NOT NULL REFERENCES app_user(id),
    name          TEXT NOT NULL,
    currency      TEXT NOT NULL DEFAULT 'RUB',
    sewer_enabled INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL
);

-- Четыре регистра: ХВС, ГВС, электричество T1 и T2 (два регистра одного физического прибора).
CREATE TABLE register (
    id          INTEGER PRIMARY KEY,
    property_id INTEGER NOT NULL REFERENCES property(id),
    code        TEXT NOT NULL CHECK (code IN ('cold', 'hot', 't1', 't2')),
    label       TEXT NOT NULL,
    unit        TEXT NOT NULL,
    UNIQUE (property_id, code)
);

CREATE TABLE tenancy (
    id             INTEGER PRIMARY KEY,
    property_id    INTEGER NOT NULL UNIQUE REFERENCES property(id),
    tenant_user_id INTEGER REFERENCES app_user(id),
    tenant_name    TEXT NOT NULL
);

CREATE TABLE invite (
    token       TEXT PRIMARY KEY,
    property_id INTEGER NOT NULL REFERENCES property(id),
    expires_at  TEXT NOT NULL,
    used_by     INTEGER REFERENCES app_user(id),
    used_at     TEXT,
    created_at  TEXT NOT NULL
);

-- Версионные тарифы: действует последний с valid_from <= начала расчётного периода.
CREATE TABLE tariff (
    id          INTEGER PRIMARY KEY,
    property_id INTEGER NOT NULL REFERENCES property(id),
    code        TEXT NOT NULL CHECK (code IN ('cold', 'hot', 'sewer', 't1', 't2')),
    price       TEXT NOT NULL,
    valid_from  TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    UNIQUE (property_id, code, valid_from)
);

-- Подтверждённые показания. Исправление не перезаписывает строку: старая получает 'superseded'.
-- baseline: период '0000-00', дата может быть неизвестна (NULL) — её не подставляют.
CREATE TABLE reading (
    id              INTEGER PRIMARY KEY,
    register_id     INTEGER NOT NULL REFERENCES register(id),
    period          TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('baseline', 'submitted')),
    value           TEXT NOT NULL,
    read_on         TEXT,
    source          TEXT NOT NULL CHECK (source IN ('baseline', 'photo', 'manual')),
    photo_file_id   TEXT,
    photo_unique_id TEXT,
    photo_path      TEXT,
    ai_value        TEXT,
    ai_confidence   TEXT,
    status          TEXT NOT NULL CHECK (status IN ('confirmed', 'superseded')),
    confirmed_by    INTEGER REFERENCES app_user(id),
    created_at      TEXT NOT NULL
);
CREATE UNIQUE INDEX reading_one_confirmed ON reading (register_id, period) WHERE status = 'confirmed';
-- одно и то же фото не может быть принято дважды
CREATE UNIQUE INDEX reading_photo_once ON reading (photo_unique_id)
    WHERE photo_unique_id IS NOT NULL AND status = 'confirmed';

-- Показание, ожидающее подтверждения человеком (после распознавания или ручного ввода).
CREATE TABLE pending_reading (
    id            INTEGER PRIMARY KEY,
    user_id       INTEGER NOT NULL REFERENCES app_user(id),
    period        TEXT NOT NULL,
    register_code TEXT,
    value         TEXT,
    ai_value      TEXT,
    ai_confidence TEXT,
    file_id       TEXT,
    unique_id     TEXT,
    photo_path    TEXT,
    source        TEXT NOT NULL CHECK (source IN ('photo', 'manual')),
    status        TEXT NOT NULL CHECK (status IN ('open', 'confirmed', 'discarded')),
    created_at    TEXT NOT NULL
);

CREATE TABLE bill (
    id          INTEGER PRIMARY KEY,
    property_id INTEGER NOT NULL REFERENCES property(id),
    period      TEXT NOT NULL,
    version     INTEGER NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('draft', 'approved', 'rejected', 'paid')),
    total       TEXT NOT NULL,
    snapshot    TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    decided_at  TEXT,
    paid_at     TEXT,
    UNIQUE (property_id, period, version)
);
-- не больше одного действующего счёта на период — защита от повторного начисления
CREATE UNIQUE INDEX bill_one_active ON bill (property_id, period) WHERE status IN ('draft', 'approved', 'paid');

CREATE TABLE chat_state (
    telegram_id INTEGER PRIMARY KEY,
    state       TEXT NOT NULL
);

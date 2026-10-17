-- RentFlow R0 core schema (spec §5). Money: numeric(14,2); readings: numeric(14,3). Keys: UUID.

CREATE TABLE account (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name       text NOT NULL,
    locale     text NOT NULL DEFAULT 'ru',
    timezone   text NOT NULL DEFAULT 'Europe/Moscow',
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE app_user (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id  uuid NOT NULL REFERENCES account(id),
    telegram_id bigint NOT NULL UNIQUE,
    role        text NOT NULL CHECK (role IN ('owner', 'manager', 'tenant')),
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE property (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id uuid NOT NULL REFERENCES account(id),
    name       text NOT NULL,
    country    text NOT NULL DEFAULT 'RU',
    currency   char(3) NOT NULL DEFAULT 'RUB',
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE tenancy (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    property_id uuid NOT NULL REFERENCES property(id),
    tenant_id   uuid NOT NULL REFERENCES app_user(id),
    starts_on   date NOT NULL,
    ends_on     date,
    status      text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'ended')),
    CHECK (ends_on IS NULL OR ends_on >= starts_on)
);
-- one active tenancy per property
CREATE UNIQUE INDEX tenancy_one_active_per_property ON tenancy (property_id) WHERE status = 'active';

CREATE TABLE invite (
    token_hash  bytea PRIMARY KEY,
    property_id uuid NOT NULL REFERENCES property(id),
    created_by  uuid NOT NULL REFERENCES app_user(id),
    expires_at  timestamptz NOT NULL,
    used_at     timestamptz,
    used_by     uuid REFERENCES app_user(id)
);

CREATE TABLE meter (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    property_id uuid NOT NULL REFERENCES property(id),
    kind        text NOT NULL,
    label       text NOT NULL,
    unit        text NOT NULL,
    active      boolean NOT NULL DEFAULT true,
    UNIQUE (property_id, label)
);

CREATE TABLE document (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    storage_key   text NOT NULL UNIQUE,
    sha256        bytea NOT NULL,
    mime          text NOT NULL,
    original_name text,
    uploader_id   uuid REFERENCES app_user(id),
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE meter_reading (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    meter_id        uuid NOT NULL REFERENCES meter(id),
    value           numeric(14,3) NOT NULL CHECK (value >= 0),
    read_at         timestamptz NOT NULL,
    submitted_at    timestamptz NOT NULL DEFAULT now(),
    photo_id        uuid REFERENCES document(id),
    source          text NOT NULL CHECK (source IN ('initial', 'photo', 'manual')),
    extracted_value numeric(14,3),
    confidence      numeric(4,3),
    correction_reason text,
    status          text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'confirmed', 'rejected')),
    confirmed_by    uuid REFERENCES app_user(id),
    revision        integer NOT NULL DEFAULT 1,
    -- a confirmed reading always says who confirmed it
    CHECK (status <> 'confirmed' OR confirmed_by IS NOT NULL)
);
CREATE INDEX meter_reading_meter_read_at ON meter_reading (meter_id, read_at);

CREATE TABLE tariff (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    meter_id   uuid NOT NULL REFERENCES meter(id),
    unit_price numeric(14,4) NOT NULL CHECK (unit_price >= 0),
    currency   char(3) NOT NULL DEFAULT 'RUB',
    valid_from date NOT NULL,
    valid_to   date,
    source     text NOT NULL DEFAULT 'owner',
    CHECK (valid_to IS NULL OR valid_to >= valid_from)
);

CREATE TABLE billing_period (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenancy_id uuid NOT NULL REFERENCES tenancy(id),
    starts_on  date NOT NULL,
    ends_on    date NOT NULL,
    state      text NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'billed')),
    UNIQUE (tenancy_id, starts_on, ends_on),
    CHECK (ends_on >= starts_on)
);

-- A bill is an immutable snapshot of one calculation. Versions of one period are separate rows.
CREATE TABLE bill (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    billing_period_id uuid NOT NULL REFERENCES billing_period(id),
    version           integer NOT NULL CHECK (version >= 1),
    status            text NOT NULL DEFAULT 'draft'
                      CHECK (status IN ('draft', 'approved', 'paid', 'voided')),
    currency          char(3) NOT NULL DEFAULT 'RUB',
    total             numeric(14,2) NOT NULL CHECK (total >= 0),
    snapshot          jsonb NOT NULL,
    void_reason       text,
    approved_by       uuid REFERENCES app_user(id),
    approved_at       timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (billing_period_id, version),
    CHECK (status NOT IN ('approved', 'paid') OR (approved_by IS NOT NULL AND approved_at IS NOT NULL)),
    CHECK (status <> 'voided' OR void_reason IS NOT NULL)
);
-- spec §6: exactly one final (approved or paid) bill per tenancy period
CREATE UNIQUE INDEX bill_one_final_per_period ON bill (billing_period_id) WHERE status IN ('approved', 'paid');

CREATE TABLE bill_line (
    id        uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    bill_id   uuid NOT NULL REFERENCES bill(id),
    meter_id  uuid NOT NULL REFERENCES meter(id),
    previous  numeric(14,3) NOT NULL,
    current   numeric(14,3) NOT NULL,
    usage     numeric(14,3) NOT NULL,
    unit_price numeric(14,4) NOT NULL,
    amount    numeric(14,2) NOT NULL
);

CREATE TABLE payment_record (
    id        uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    bill_id   uuid NOT NULL REFERENCES bill(id),
    amount    numeric(14,2) NOT NULL CHECK (amount > 0),
    paid_on   date NOT NULL,
    source    text NOT NULL DEFAULT 'owner_marked',
    marked_by uuid NOT NULL REFERENCES app_user(id),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE audit_event (
    id          bigserial PRIMARY KEY,
    actor_id    uuid REFERENCES app_user(id),
    action      text NOT NULL,
    object_type text NOT NULL,
    object_id   uuid NOT NULL,
    details     jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_event_object ON audit_event (object_type, object_id);

-- Written in the same transaction as the state change; a worker delivers after commit.
-- dedup_key makes the logical notification unique even if the command is replayed.
CREATE TABLE outbox_event (
    id         bigserial PRIMARY KEY,
    dedup_key  text NOT NULL UNIQUE,
    kind       text NOT NULL,
    payload    jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    sent_at    timestamptz
);

-- Telegram webhook replay protection: (bot_id, update_id).
CREATE TABLE telegram_update (
    bot_id      bigint NOT NULL,
    update_id   bigint NOT NULL,
    received_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (bot_id, update_id)
);

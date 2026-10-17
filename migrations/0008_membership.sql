-- Мультиарендность: права доступа задаёт членство пользователя в объекте (владелец или арендатор).
-- Существующие данные переносятся как есть: владелец — тот, кто записан в property.landlord_id, арендатор —
-- tenancy.tenant_user_id. Никаких назначений «по знанию Telegram ID»: берётся только то, что уже записано в базе.
CREATE TABLE membership (
    id          INTEGER PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES app_user(id),
    property_id INTEGER NOT NULL REFERENCES property(id),
    role        TEXT NOT NULL CHECK (role IN ('owner', 'tenant')),
    created_at  TEXT NOT NULL,
    UNIQUE (user_id, property_id)
);
CREATE INDEX membership_property ON membership (property_id);

INSERT INTO membership (user_id, property_id, role, created_at)
SELECT landlord_id, id, 'owner', created_at FROM property;

INSERT OR IGNORE INTO membership (user_id, property_id, role, created_at)
SELECT tenant_user_id, property_id, 'tenant', (SELECT created_at FROM property WHERE id = tenancy.property_id)
  FROM tenancy WHERE tenant_user_id IS NOT NULL;

-- Активный объект пользователя (переключатель квартир); NULL — первый из его объектов.
ALTER TABLE app_user ADD COLUMN active_property_id INTEGER REFERENCES property(id);

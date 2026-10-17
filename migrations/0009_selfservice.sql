-- Self-service: регистры можно отключать (например, T2 при однотарифном счётчике), у объекта свой часовой пояс,
-- черновик мастера настройки хранится на сервере (возобновляется при повторном входе, не создаёт квартир до «Готово»).
ALTER TABLE register ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;
-- Физический прибор: T1 и T2 — два регистра одного электросчётчика (одна группа), независимые по тарифам.
ALTER TABLE register ADD COLUMN meter_group TEXT;
UPDATE register SET meter_group = 'electricity' WHERE code IN ('t1', 't2');
UPDATE register SET meter_group = code WHERE code IN ('cold', 'hot');

ALTER TABLE property ADD COLUMN tz_offset_hours INTEGER NOT NULL DEFAULT 3 CHECK (tz_offset_hours BETWEEN -12 AND 14);

CREATE TABLE property_draft (
    user_id     INTEGER PRIMARY KEY REFERENCES app_user(id),
    step        INTEGER NOT NULL DEFAULT 0,
    data        TEXT NOT NULL DEFAULT '{}',
    property_id INTEGER REFERENCES property(id),   -- заполнено, когда мастер завершён: повторное «Готово» идемпотентно
    claimed     INTEGER NOT NULL DEFAULT 0,        -- 1, пока «Готово» выполняется: параллельный запрос получает отказ
    updated_at  TEXT NOT NULL
);

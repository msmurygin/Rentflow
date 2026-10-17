-- Месячная политика тарифов: тариф действует с расчётного месяца (ГГГГ-ММ) и дальше, пока не заменён более поздним.
-- Даты внутри месяца не существуют: `valid_from` всегда первое число месяца.

-- 1. Включённость позиции (для водоотведения) — теперь свойство месячной версии тарифа.
ALTER TABLE tariff ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;

-- 2. Нормализация дневных версий: на (объект, позиция, месяц) остаётся одна — введённая последней.
DELETE FROM tariff
 WHERE id NOT IN (SELECT max(id) FROM tariff GROUP BY property_id, code, substr(valid_from, 1, 7));
UPDATE tariff SET valid_from = substr(valid_from, 1, 7) || '-01';
CREATE UNIQUE INDEX tariff_one_per_month ON tariff (property_id, code, substr(valid_from, 1, 7));

-- 3. Выключенное водоотведение (раньше — флаг объекта) становится месячной версией: выключено с текущего месяца.
INSERT OR REPLACE INTO tariff (property_id, code, price, valid_from, created_at, enabled)
SELECT p.id, 'sewer',
       COALESCE((SELECT t.price FROM tariff t WHERE t.property_id = p.id AND t.code = 'sewer' ORDER BY t.valid_from DESC LIMIT 1), '0'),
       strftime('%Y-%m-01', 'now'), p.created_at, 0
  FROM property p WHERE p.sewer_enabled = 0;

-- 4. Новый статус счёта 'void' (заменён: устаревший черновик или счёт, выпущенный заново) и пометка причины.
--    Таблица пересобирается, потому что SQLite не умеет менять CHECK; все существующие счета копируются как есть.
CREATE TABLE bill_new (
    id          INTEGER PRIMARY KEY,
    property_id INTEGER NOT NULL REFERENCES property(id),
    period      TEXT NOT NULL,
    version     INTEGER NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('draft', 'approved', 'rejected', 'paid', 'void')),
    total       TEXT NOT NULL,
    snapshot    TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    decided_at  TEXT,
    paid_at     TEXT,
    note        TEXT,
    UNIQUE (property_id, period, version)
);
INSERT INTO bill_new (id, property_id, period, version, status, total, snapshot, created_at, decided_at, paid_at)
SELECT id, property_id, period, version, status, total, snapshot, created_at, decided_at, paid_at FROM bill;
DROP TABLE bill;
ALTER TABLE bill_new RENAME TO bill;
CREATE UNIQUE INDEX bill_one_active ON bill (property_id, period) WHERE status IN ('draft', 'approved', 'paid');

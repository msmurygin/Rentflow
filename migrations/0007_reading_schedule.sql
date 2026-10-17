-- День месяца (1–28), до которого арендатор передаёт показания; NULL — расписание не задано.
ALTER TABLE property ADD COLUMN reading_day INTEGER CHECK (reading_day IS NULL OR reading_day BETWEEN 1 AND 28);

-- Какие напоминания за какой период уже отправлены: рестарт бота не вызывает повторов.
CREATE TABLE reminder_log (
    property_id INTEGER NOT NULL REFERENCES property(id),
    period      TEXT NOT NULL,
    kind        TEXT NOT NULL,
    sent_at     TEXT NOT NULL,
    PRIMARY KEY (property_id, period, kind)
);

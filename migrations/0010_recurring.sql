-- Повторяющиеся процессы: срок передачи «в последний день месяца» (для 29–31), срок оплаты счёта, журнал доставки.
ALTER TABLE property ADD COLUMN reading_last INTEGER NOT NULL DEFAULT 0;
-- Сколько дней у арендатора на оплату после утверждения счёта.
ALTER TABLE property ADD COLUMN payment_days INTEGER NOT NULL DEFAULT 10 CHECK (payment_days BETWEEN 1 AND 90);
-- Срок оплаты фиксируется при утверждении. У уже утверждённых счетов его нет (NULL): о них просрочек не объявляем.
ALTER TABLE bill ADD COLUMN due_date TEXT;

-- Диагностика доставки: что не дошло и почему (без текста сообщений и персональных данных).
CREATE TABLE notification_log (
    id         INTEGER PRIMARY KEY,
    at         TEXT NOT NULL,
    chat_id    INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    ok         INTEGER NOT NULL,
    error      TEXT
);
CREATE INDEX notification_log_failed ON notification_log (ok, at);

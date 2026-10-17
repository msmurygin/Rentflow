-- Фото-подтверждение показаний: владелец выбирает, обязательно ли оно (по умолчанию — обязательно).
ALTER TABLE property ADD COLUMN proof_required INTEGER NOT NULL DEFAULT 1;

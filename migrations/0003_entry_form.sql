-- Ввод показаний теперь идёт «формой»: по одному черновому значению на регистр, итог отправляется одной кнопкой.
-- Старые незавершённые карточки не соответствуют новой модели — снимаем их (подтверждённые показания не затрагиваются).
UPDATE pending_reading SET status = 'discarded' WHERE status = 'open';
-- не больше одного открытого черновика на (арендатор, период, регистр)
CREATE UNIQUE INDEX pending_one_open_per_register
    ON pending_reading (user_id, period, register_code)
    WHERE status = 'open' AND register_code IS NOT NULL;

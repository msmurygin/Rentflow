#!/usr/bin/env bash
# Ночная резервная копия: согласованный снимок -> проверка -> шифрование (age) -> выгрузка (rclone) -> отметка -> ротация.
# Запускается НА СЕРВЕРЕ из каталога проекта (cron/systemd timer), см. docs/v0.3/DEPLOY.md.
# Нужны на хосте: docker compose, age, rclone. Настройки — в .env:
#   BACKUP_REMOTE=hetzner-box:rentflow     (путь rclone вне этого сервера)
#   AGE_RECIPIENT=age1...                  (ПУБЛИЧНЫЙ ключ; закрытый хранится не на сервере)
#   BACKUP_RETENTION_DAYS=30
set -euo pipefail
cd "$(dirname "$0")/.."
set -a; . ./.env; set +a
: "${BACKUP_REMOTE:?задайте BACKUP_REMOTE в .env}" "${AGE_RECIPIENT:?задайте AGE_RECIPIENT в .env}"
RETENTION="${BACKUP_RETENTION_DAYS:-30}"
WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT

alert() {
  # без секретов в тексте; админу в Telegram
  if [ -n "${TELEGRAM_BOT_TOKEN:-}" ] && [ -n "${ADMIN_TELEGRAM_ID:-${LANDLORD_TELEGRAM_ID:-}}" ]; then
    curl -fsS -m 15 "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/sendMessage" \
      -d chat_id="${ADMIN_TELEGRAM_ID:-$LANDLORD_TELEGRAM_ID}" -d text="🚨 RentFlow backup: $1" >/dev/null || true
  fi
}
trap 'alert "ошибка на строке $LINENO (ночная копия не создана или не выгружена)"' ERR

# 1. согласованный снимок + проверка внутри контейнера (rentflow сам проверяет копию перед успехом)
SNAP="$(docker compose exec -T rentflow rentflow backup | tail -n1)"
NAME="$(basename "$SNAP")"
# 2. забираем на хост, шифруем, выгружаем
docker compose cp "rentflow:$SNAP" "$WORK/$NAME"
tar -C "$WORK" -czf - "$NAME" | age -r "$AGE_RECIPIENT" > "$WORK/$NAME.tar.gz.age"
rclone copyto "$WORK/$NAME.tar.gz.age" "$BACKUP_REMOTE/$NAME.tar.gz.age"
# 3. проверяем, что файл на месте и размер совпал
LOCAL_SIZE="$(stat -c %s "$WORK/$NAME.tar.gz.age")"
REMOTE_SIZE="$(rclone size --json "$BACKUP_REMOTE/$NAME.tar.gz.age" | python3 -c 'import json,sys; print(json.load(sys.stdin)["bytes"])')"
[ "$LOCAL_SIZE" = "$REMOTE_SIZE" ] || { alert "размер выгруженного файла не совпал"; exit 1; }
# 4. отметка для мониторинга и ротация (локально в томе и на удалённой стороне)
docker compose exec -T rentflow rentflow backup-mark-offsite
docker compose exec -T rentflow sh -c "find /data/backups -maxdepth 1 -name 'rentflow-*' -mtime +3 -exec rm -rf {} +"
rclone delete "$BACKUP_REMOTE" --min-age "${RETENTION}d" --include 'rentflow-*.tar.gz.age'
echo "ok: $NAME"

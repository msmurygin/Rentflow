#!/usr/bin/env bash
# Проверка восстановления в ОТДЕЛЬНОМ томе (не production): расшифровать копию, восстановить, применить миграции текущего образа.
# Использование: deploy/restore-staging.sh <файл .tar.gz.age> <файл с закрытым ключом age>
# Production-том и бот не затрагиваются; Telegram здесь не запускается.
set -euo pipefail
cd "$(dirname "$0")/.."
FILE="$1"; KEY="$2"
VOL="rentflow-staging-data-$(date +%s)"
WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
age -d -i "$KEY" "$FILE" | tar -C "$WORK" -xzf -
SNAP="$(ls -d "$WORK"/rentflow-* | head -n1)"
IMG="$(docker compose images -q rentflow | head -n1)"
docker volume create "$VOL" >/dev/null
docker run --rm -v "$VOL":/data -v "$WORK":/in:ro --entrypoint rentflow "$IMG" verify-backup "/in/$(basename "$SNAP")"
docker run --rm -v "$VOL":/data -v "$WORK":/in:ro --entrypoint rentflow "$IMG" restore "/in/$(basename "$SNAP")" /data
docker run --rm -v "$VOL":/data -e DATABASE_URL=sqlite:///data/rentflow.db --entrypoint rentflow "$IMG" migrate-check
echo "Восстановление проверено в томе $VOL (удалить: docker volume rm $VOL)"

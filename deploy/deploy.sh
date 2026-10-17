#!/usr/bin/env bash
# Обновление на сервере: резервная копия -> новая сборка -> перезапуск. Миграции базы применяются при старте бота.
set -euo pipefail
cd "$(dirname "$0")/.."
git pull --ff-only
deploy/backup.sh
docker compose up -d --build
docker compose ps
echo "Проверка: curl -fsS https://\$DOMAIN/healthz"

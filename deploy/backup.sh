#!/usr/bin/env bash
# Резервная копия тома с базой и фото. Бот на это время останавливается — так копия гарантированно согласована.
# Использование: deploy/backup.sh [каталог для копий, по умолчанию ./backups]
set -euo pipefail
cd "$(dirname "$0")/.."
dest="${1:-./backups}"
mkdir -p "$dest"
file="rentflow-$(date +%Y%m%d-%H%M%S).tgz"
volume="$(docker compose config --format json | python3 -c 'import json,sys; c=json.load(sys.stdin); print(c.get("name","rentflow")+"_rentflow-data")')"
if docker volume inspect "$volume" >/dev/null 2>&1; then
  docker compose stop rentflow
  docker run --rm -v "$volume":/data:ro -v "$(cd "$dest" && pwd)":/backup alpine tar czf "/backup/$file" -C /data .
  docker compose start rentflow
  echo "Резервная копия: $dest/$file"
else
  echo "Тома $volume ещё нет (первый запуск) — копировать нечего."
fi

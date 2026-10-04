#!/usr/bin/env bash
# Validate docker-compose.yml with Docker's own parser.
#
# Compose's interpolation errors are terse ("invalid hostPort: 1.2.3.4"), which
# makes a typo'd port or a missing variable hard to diagnose from the GUI. This
# renders the file with the environment below and reports what Docker thinks.
#
#   ./scripts/check-compose.sh
#
# Override the values it checks with by exporting them first, e.g.
#   MOSQUITTO_HOST=10.0.0.5 ./scripts/check-compose.sh

set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v docker >/dev/null 2>&1; then
  echo "docker not found: cannot validate" >&2
  exit 2
fi

export VALETUDO_URL="${VALETUDO_URL:-http://192.0.2.46}"
export MOSQUITTO_HOST="${MOSQUITTO_HOST:-192.0.2.1}"
export MOSQUITTO_PORT="${MOSQUITTO_PORT:-1883}"

echo "--- docker compose config ---"
docker compose config

echo
echo "--- published ports ---"
docker compose config --format json \
  | grep -o '"published":"[^"]*"' \
  | sort -u \
  || echo "(none)"

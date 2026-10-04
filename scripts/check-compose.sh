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
#   VALETUDO_URL=http://10.0.0.9:80 ./scripts/check-compose.sh
#
# Add -f docker-compose.mqtt.yml to the config command to check the overlay too.

set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v docker >/dev/null 2>&1; then
  echo "docker not found: cannot validate" >&2
  exit 2
fi

export VALETUDO_URL="${VALETUDO_URL:-http://192.0.2.46}"

echo "--- docker compose config ---"
docker compose config

echo
echo "--- published ports ---"
docker compose config --format json \
  | grep -o '"published":"[^"]*"' \
  | sort -u \
  || echo "(none)"

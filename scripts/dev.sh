#!/usr/bin/env bash
# Run the API (serving the built web app) and one worker locally against the
# disposable PostgreSQL started by scripts/test-db.sh. Ctrl-C stops both.
set -euo pipefail
cd "$(dirname "$0")/.."

server_url="$(scripts/test-db.sh start)"
# Same server, separate database for development data.
dev_url="${server_url/\/usie\?/\/usie_dev?}"

if command -v psql >/dev/null 2>&1; then
  psql_cmd=psql
else
  psql_cmd="$(ls -d /usr/lib/postgresql/*/bin | head -1)/psql"
fi
if ! "$psql_cmd" "$server_url" -tAc "SELECT 1 FROM pg_database WHERE datname = 'usie_dev'" | grep -q 1; then
  "$psql_cmd" "$server_url" -c "CREATE DATABASE usie_dev" >/dev/null
fi

export USIE_DATABASE_URL="$dev_url"
export USIE_DATA_DIR="${USIE_DATA_DIR:-$PWD/.data}"
export USIE_ENGINE_BIN="$PWD/engines/target/release/usie-engine"
export USIE_WEB_DIR="$PWD/apps/web/dist"
export USIE_HTTP_ADDR="${USIE_HTTP_ADDR:-127.0.0.1:8080}"
export USIE_COOKIE_SECURE=false
export USIE_LOG_LEVEL="${USIE_LOG_LEVEL:-info}"

trap 'kill 0' EXIT
bin/usie-api &
sleep 1
bin/usie-worker &
echo "USIE is running at http://${USIE_HTTP_ADDR}"
wait

#!/usr/bin/env bash
# Entrypoint of the single-machine deployment (Fly.io). Runs as root only to
# prepare the volume and start PostgreSQL, then runs USIE as an unprivileged
# user.
#
# Without USIE_DATABASE_URL, PostgreSQL runs in this machine with its data on
# the volume, reachable only through a Unix socket (no TCP listener) with
# peer authentication. Set USIE_DATABASE_URL to use an external database.
set -euo pipefail

DATA="${USIE_VOLUME:-/data}"
RUN_AS="${USIE_RUN_AS:-usie}"
PG_BIN="${PG_BIN:-$(ls -d /usr/lib/postgresql/*/bin | sort -V | tail -1)}"
PG_DATA="$DATA/postgres"
PG_SOCKET="${PG_SOCKET:-/run/postgresql}"

mkdir -p "$DATA/usie"
chown "$RUN_AS:$RUN_AS" "$DATA/usie"
chmod 700 "$DATA/usie"
export USIE_DATA_DIR="$DATA/usie"

as() { local user="$1"; shift; setpriv --reuid="$user" --regid="$user" --init-groups -- "$@"; }

embedded_pg=false
if [ -z "${USIE_DATABASE_URL:-}" ]; then
  embedded_pg=true
  mkdir -p "$PG_DATA" "$PG_SOCKET"
  chown postgres:postgres "$PG_DATA" "$PG_SOCKET"
  chmod 700 "$PG_DATA"
  if [ ! -f "$PG_DATA/PG_VERSION" ]; then
    as postgres "$PG_BIN/initdb" -D "$PG_DATA" -U postgres --auth-local=peer --auth-host=reject -E UTF8 --locale=C.UTF-8 >/dev/null
  fi
  as postgres "$PG_BIN/pg_ctl" -D "$PG_DATA" -w -l "$PG_DATA/server.log" \
    -o "-c listen_addresses='' -c unix_socket_directories=$PG_SOCKET" start >/dev/null
  psql() { as postgres "$PG_BIN/psql" -h "$PG_SOCKET" -U postgres -d postgres -v ON_ERROR_STOP=1 -tAq "$@"; }
  psql -c "SELECT 1 FROM pg_roles WHERE rolname = '$RUN_AS'" | grep -q 1 || psql -c "CREATE ROLE \"$RUN_AS\" LOGIN"
  psql -c "SELECT 1 FROM pg_database WHERE datname = 'usie'" | grep -q 1 || psql -c "CREATE DATABASE usie OWNER \"$RUN_AS\""
  export USIE_DATABASE_URL="postgres:///usie?host=$PG_SOCKET&user=$RUN_AS&sslmode=disable"
fi

shutdown() {
  if [ -n "${app:-}" ]; then kill -TERM "$app" 2>/dev/null; wait "$app" 2>/dev/null; fi
  if $embedded_pg; then as postgres "$PG_BIN/pg_ctl" -D "$PG_DATA" -m fast stop >/dev/null || true; fi
}
trap 'shutdown; exit 0' TERM INT

# Not through the `as` function: backgrounding a function forks a subshell,
# and the TERM sent on shutdown would stop at that subshell.
setpriv --reuid="$RUN_AS" --regid="$RUN_AS" --init-groups -- "${USIE_APP_CMD:-usie-all}" &
app=$!
set +e
wait "$app"
status=$?
app=""
shutdown
exit "$status"

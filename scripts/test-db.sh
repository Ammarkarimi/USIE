#!/usr/bin/env bash
# Start (or stop) a disposable PostgreSQL cluster for tests and local runs.
#
#   scripts/test-db.sh start   # prints the connection URL
#   scripts/test-db.sh stop
#
# The cluster lives under $USIE_TEST_PG_DIR (default: /tmp/usie-pg), listens
# on localhost only, and uses trust auth: never use it for real data.
set -euo pipefail

PG_DIR="${USIE_TEST_PG_DIR:-/tmp/usie-pg}"
PORT="${USIE_TEST_PG_PORT:-54329}"
URL="postgres://usie@127.0.0.1:${PORT}/usie?sslmode=disable"

find_bin() {
  if command -v pg_ctl >/dev/null 2>&1; then dirname "$(command -v pg_ctl)"; return; fi
  for d in /usr/lib/postgresql/*/bin /usr/local/opt/postgresql*/bin /opt/homebrew/opt/postgresql*/bin; do
    [ -x "$d/pg_ctl" ] && { echo "$d"; return; }
  done
  echo "PostgreSQL server binaries not found" >&2; exit 1
}
BIN="$(find_bin)"

# PostgreSQL refuses to run as root; fall back to the postgres system user.
run() {
  if [ "$(id -u)" = "0" ]; then
    local cmd; cmd="$(printf '%q ' "$@")"
    su -s /bin/bash postgres -c "$cmd"
  else
    "$@"
  fi
}

case "${1:-start}" in
  start)
    if [ ! -f "$PG_DIR/PG_VERSION" ]; then
      mkdir -p "$PG_DIR"
      [ "$(id -u)" = "0" ] && chown postgres "$PG_DIR"
      run "$BIN/initdb" -D "$PG_DIR" -U usie -A trust -E UTF8 --locale=C >/dev/null
    fi
    if ! run "$BIN/pg_ctl" -D "$PG_DIR" status >/dev/null 2>&1; then
      run "$BIN/pg_ctl" -D "$PG_DIR" -w -l "$PG_DIR/server.log" \
        -o "-p $PORT -c listen_addresses=127.0.0.1 -c unix_socket_directories=$PG_DIR -c fsync=off" start >/dev/null
    fi
    run "$BIN/psql" -h 127.0.0.1 -p "$PORT" -U usie -d postgres -tAc "SELECT 1 FROM pg_database WHERE datname='usie'" | grep -q 1 \
      || run "$BIN/createdb" -h 127.0.0.1 -p "$PORT" -U usie usie
    echo "$URL"
    ;;
  stop)
    run "$BIN/pg_ctl" -D "$PG_DIR" -m fast stop >/dev/null 2>&1 || true
    ;;
  *) echo "usage: $0 start|stop" >&2; exit 2 ;;
esac

#!/usr/bin/env bash
# Runs the API and one worker in the same container (they share the blob
# store directory). If either exits, the other is stopped and the container
# exits with that status, so the platform restarts both together.
set -uo pipefail

usie-api &
api=$!
usie-worker &
worker=$!

stop() { kill -TERM "$api" "$worker" 2>/dev/null; }
trap stop TERM INT

wait -n "$api" "$worker"
status=$?
stop
wait "$api" "$worker" 2>/dev/null
exit "$status"

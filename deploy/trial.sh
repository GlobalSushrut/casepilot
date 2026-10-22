#!/bin/sh
# One process tree for the public trial. Render's free web service has no
# private network and no disk, so Surreal and Connector stay on localhost
# and the case file lives only until this instance sleeps or restarts.
set -eu
export CONNECTOR_HOST="${CONNECTOR_HOST:-127.0.0.1}"
export CONNECTOR_PORT="${CONNECTOR_PORT:-9091}"
export CONNECTOR_DATA_DIR="${CONNECTOR_DATA_DIR:-/tmp/connector}"
export CONNECTOR_PRESET="${CONNECTOR_PRESET:-local}"
export CONNECTOR_ENV="${CONNECTOR_ENV:-development}"
export CONNECTOR_DEV_MODE="${CONNECTOR_DEV_MODE:-1}"
export CONNECTOR_API_KEY="${CONNECTOR_API_KEY:-dev-token}"
export CONNECTOR_ALLOW_IN_PROCESS_EFFECTS="${CONNECTOR_ALLOW_IN_PROCESS_EFFECTS:-1}"
export CONNECTOR_DEV_AGENT_CAP="${CONNECTOR_DEV_AGENT_CAP:-64}"
export SURREAL_USER="${SURREAL_USER:-root}"
export SURREAL_PASS="${SURREAL_PASS:-root}"
export SURREAL_URL="${SURREAL_URL:-http://127.0.0.1:8000}"
export CONNECTOR_API_URL="${CONNECTOR_API_URL:-http://127.0.0.1:9091}"
mkdir -p /tmp/casepilot "$CONNECTOR_DATA_DIR" /tmp/surreal
chmod 777 "$CONNECTOR_DATA_DIR" /tmp/surreal /tmp/casepilot

/usr/local/bin/surreal start \
  --log info \
  --user "$SURREAL_USER" \
  --pass "$SURREAL_PASS" \
  --bind 127.0.0.1:8000 \
  rocksdb:/tmp/surreal/custody.db &
surreal_pid=$!

/usr/local/bin/connector-start.sh &
connector_pid=$!

/usr/local/bin/casepilot &
web_pid=$!

trap 'kill "$web_pid" "$connector_pid" "$surreal_pid" 2>/dev/null || true' TERM INT
wait "$web_pid"

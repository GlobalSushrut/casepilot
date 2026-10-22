#!/bin/sh
# Render mounts the data disk as root. The platform process should still
# run as the connector user the package created.
set -eu
data="${CONNECTOR_DATA_DIR:-/var/lib/connector}"
export CONNECTOR_HOST="${CONNECTOR_HOST:-0.0.0.0}"
export CONNECTOR_PORT="${CONNECTOR_PORT:-${PORT:-9091}}"
if [ "$(id -u)" = "0" ]; then
  mkdir -p "$data"
  chown -R connector:connector "$data"
  exec su --preserve-environment -s /bin/sh connector -c 'exec /usr/bin/connector-platform'
fi
exec /usr/bin/connector-platform

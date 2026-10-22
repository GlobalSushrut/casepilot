#!/usr/bin/env bash
# Connector OS installs as software. If it is already answering, leave it.
# The package provides connector-platform, connectorctl, and
# /etc/connector/connector.yaml. This script does not unpack a tarball.
set -euo pipefail
URL="${CONNECTOR_API_URL:-http://127.0.0.1:9091}"

if curl -fsS --max-time 2 "$URL/healthz" >/dev/null 2>&1; then
  echo "connector_already_live"
  exit 0
fi

if command -v systemctl >/dev/null 2>&1 && systemctl cat connector-platform.service >/dev/null 2>&1; then
  systemctl start connector-platform.service
  for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    if curl -fsS --max-time 2 "$URL/healthz" >/dev/null 2>&1; then
      echo "connector_started"
      exit 0
    fi
    sleep 1
  done
  echo "connector_not_live" >&2
  exit 1
fi

echo "connector_not_installed: install Connector OS, then start CasePilot again." >&2
echo "  curl -fsSL https://raw.githubusercontent.com/GlobalSushrut/connector-oss/main/platform/release/install.sh | sudo bash" >&2
echo "  or: curl -fLO https://github.com/GlobalSushrut/connector-oss/releases/download/prebuilt/connector-os_amd64.deb && sudo apt install ./connector-os_amd64.deb" >&2
exit 2

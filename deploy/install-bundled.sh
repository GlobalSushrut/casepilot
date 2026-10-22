#!/usr/bin/env bash
# Install the Connector OS tarball shipped beside this script.
# Refuses to continue when the archive is missing. Does not download a build.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ARCHIVE="$ROOT/deploy/bundle/connector-os.tar.gz"
if [[ ! -f "$ARCHIVE" ]]; then
  echo "connector_package_missing: $ARCHIVE" >&2
  exit 1
fi
if [[ -f "$ARCHIVE.sha256" ]]; then
  (cd "$(dirname "$ARCHIVE")" && sha256sum -c connector-os.tar.gz.sha256)
fi
WORK="${TMPDIR:-/tmp}/casepilot-connector-bundle"
rm -rf "$WORK"
mkdir -p "$WORK"
tar -xzf "$ARCHIVE" -C "$WORK"
INSTALLER="$(find "$WORK" -name install.sh -type f | head -n 1)"
if [[ -z "$INSTALLER" ]]; then
  echo "tarball has no install.sh" >&2
  exit 1
fi
if [[ "$(id -u)" -ne 0 ]]; then
  echo "Checksummed package is present. Re-run as root: bash $INSTALLER" >&2
  echo "CasePilot will not mark Connector installed until that command succeeds." >&2
  exit 1
fi
bash "$INSTALLER"
systemctl enable --now connector-platform
echo "connector-platform enable requested"

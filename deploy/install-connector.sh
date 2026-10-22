#!/usr/bin/env bash
# Install a pinned Connector OS build package. Does not compile Connector.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
PIN="$ROOT/connector.pin"
WORK="${TMPDIR:-/tmp}/casepilot-connector-install"

read_pin() {
  local key="$1"
  awk -F= -v k="$key" '$1 == k { print substr($0, index($0, "=") + 1) }' "$PIN"
}

URL="$(read_pin url)"
SHA="$(read_pin sha256)"

if [[ -z "$URL" || -z "$SHA" ]]; then
  echo "deploy/connector.pin needs url and sha256 for a released connector-os tarball." >&2
  echo "CasePilot will not download an unpinned build and will not clone the Connector project." >&2
  exit 1
fi

mkdir -p "$WORK"
curl -fsSL -o "$WORK/connector-os.tar.gz" "$URL"
printf '%s  %s\n' "$SHA" "$WORK/connector-os.tar.gz" > "$WORK/SHA256SUMS"
sha256sum -c "$WORK/SHA256SUMS"
tar -xzf "$WORK/connector-os.tar.gz" -C "$WORK"
INSTALLER="$(find "$WORK" -name install.sh -type f | head -n 1)"
if [[ -z "$INSTALLER" ]]; then
  echo "tarball has no install.sh" >&2
  exit 1
fi
echo "Checksum matched. Run as root: $INSTALLER"
echo "Then: systemctl enable --now connector-platform"

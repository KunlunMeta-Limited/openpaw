#!/bin/bash
# Launch the merged engine-host inside the build container, foreground.
# Invoked detached:  TREE=/src-merge docker exec -d -e TREE openpaw-build bash /start-host.sh <scenario>
set -uo pipefail
SCEN="${1:-tools}"
TREE="${TREE:-/src-merge}"
BIN="${BIN:-/target/debug/openpaw}"

pkill -f 'openpaw --engine-host' >/dev/null 2>&1
sleep 1

rm -rf /data/verify-home
mkdir -p /data/verify-home /data/verify-ws
cd "$TREE" || exit 1

export OPENPAW_HOME=/data/verify-home
export OPENPAW_ENGINE_MOCK="$SCEN"
export OPENPAW_ENGINE_PERMISSIONS=manual
export OPENPAW_OFFICE_ACTIONS_URL=http://office-gateway:8765/actions
export OPENCODE_SERVER_USERNAME=dev
export OPENCODE_SERVER_PASSWORD=dev

exec "$BIN" \
  --engine-host \
  --engine-host-hostname 0.0.0.0 \
  --engine-host-port 8788 \
  --provider mock \
  --workspace /data/verify-ws

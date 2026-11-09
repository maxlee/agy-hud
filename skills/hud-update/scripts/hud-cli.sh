#!/bin/sh
# Pure POSIX helper entrypoint for the `/hud-update` skill (zero Node.js dependencies).
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PLUGIN_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)

exec "$PLUGIN_ROOT/scripts/agy-hud" "$@"

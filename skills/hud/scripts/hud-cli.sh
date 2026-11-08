#!/bin/sh
# Pure POSIX helper entrypoint for the `/hud` skill (zero Node.js dependencies).
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PLUGIN_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)

if [ -x "$PLUGIN_ROOT/scripts/bin/agy-hud" ]; then
  exec "$PLUGIN_ROOT/scripts/bin/agy-hud" "$@"
fi

if [ -x "$PLUGIN_ROOT/target/release/agy-hud" ]; then
  exec "$PLUGIN_ROOT/target/release/agy-hud" "$@"
fi

cargo build --release --manifest-path "$PLUGIN_ROOT/Cargo.toml" --quiet
exec "$PLUGIN_ROOT/target/release/agy-hud" "$@"

#!/bin/sh
# Pure POSIX helper entrypoint for the `/hud` skill (zero Node.js dependencies).
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PLUGIN_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)

# 1. Plugin-local router (repo checkout, unpacked Marketplace plugin, or staged bundle)
if [ -x "${PLUGIN_ROOT%/}/scripts/agy-hud" ]; then
  exec "${PLUGIN_ROOT%/}/scripts/agy-hud" "$@"
fi

# 2. Standalone / global skill invocation (`~/.gemini/config/skills/hud`)
HOME_DIR="${HOME:-${USERPROFILE:-}}"

expand_tilde() {
  case "$1" in
    "~") printf '%s' "$HOME_DIR" ;;
    "~/"*) printf '%s/%s' "$HOME_DIR" "${1#~/}" ;;
    *) printf '%s' "$1" ;;
  esac
}

BIN_NAME="agy-hud"
case "$(uname -s 2>/dev/null || echo unknown)" in
  MINGW*|MSYS*|CYGWIN*) BIN_NAME="agy-hud.exe" ;;
esac

CLI_DIR=$(expand_tilde "${AGY_CLI_DIR:-$HOME_DIR/.gemini/antigravity-cli}")
HUD_HOME=$(expand_tilde "${AGY_HUD_HOME:-$CLI_DIR/agy-hud}")
CONFIG_DIR=$(expand_tilde "${AGY_CONFIG_DIR:-$HOME_DIR/.gemini/config}")

if [ -x "${HUD_HOME%/}/bin/$BIN_NAME" ]; then
  exec "${HUD_HOME%/}/bin/$BIN_NAME" "$@"
fi

if [ -x "${HUD_HOME%/}/plugin/scripts/agy-hud" ]; then
  exec "${HUD_HOME%/}/plugin/scripts/agy-hud" "$@"
fi

if [ -x "${CONFIG_DIR%/}/plugins/agy-hud/scripts/agy-hud" ]; then
  exec "${CONFIG_DIR%/}/plugins/agy-hud/scripts/agy-hud" "$@"
fi

printf '%s\n' "agy-hud: native binary not found. Run \`agy-hud install\` or install via: curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/install.sh | sh" >&2
exit 1

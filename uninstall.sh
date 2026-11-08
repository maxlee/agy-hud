#!/bin/sh
# One-line clean uninstaller for agy-hud:
#   curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/uninstall.sh | sh
# Restores Antigravity CLI's official default statusLine, unregisters the `agy-hud` plugin,
# removes tracked settings.json backups, and deletes ~/.gemini/antigravity-cli/agy-hud cleanly.
set -eu

expand_tilde() {
  case "$1" in
    "~") printf '%s' "$HOME" ;;
    "~/"*) printf '%s/%s' "$HOME" "${1#~/}" ;;
    *) printf '%s' "$1" ;;
  esac
}

CLI_DIR_RAW="${AGY_CLI_DIR:-$HOME/.gemini/antigravity-cli}"
CLI_DIR=$(expand_tilde "$CLI_DIR_RAW")
HUD_HOME_RAW="${AGY_HUD_HOME:-$CLI_DIR/agy-hud}"
HUD_HOME=$(expand_tilde "$HUD_HOME_RAW")

KEEP_CONFIG=0
for arg in "$@"; do
  if [ "$arg" = "--keep-config" ]; then
    KEEP_CONFIG=1
  fi
done

if [ -x "$HUD_HOME/bin/agy-hud" ]; then
  printf '==> Running native agy-hud clean uninstaller...\n'
  "$HUD_HOME/bin/agy-hud" uninstall --plugin "$@"
else
  # Fallback cleanup if binary was already deleted manually
  if [ "${AGY_SKIP_PLUGIN_INSTALL:-0}" != "1" ] && command -v agy >/dev/null 2>&1; then
    agy plugin uninstall agy-hud >/dev/null 2>&1 || true
  fi
  if [ -d "$HUD_HOME" ] && [ "$HUD_HOME" != "$CLI_DIR" ] && [ "$HUD_HOME" != "$HOME" ]; then
    rm -rf "$HUD_HOME/bin" "$HUD_HOME/plugin" "$HUD_HOME/probe-cache" "$HUD_HOME/data"
    rm -f "$HUD_HOME/heartbeat.json" "$HUD_HOME/install-meta.json" "$HUD_HOME/previous-statusline.json" "$HUD_HOME/config.undo.json"
    if [ "$KEEP_CONFIG" -eq 0 ]; then
      rm -f "$HUD_HOME/config.json"
      rm -rf "$HUD_HOME"
    fi
  fi
  printf '==> Cleaned up agy-hud directory (%s).\n' "$HUD_HOME"
  if [ -f "$CLI_DIR/settings.json" ] && grep -q 'agy-hud' "$CLI_DIR/settings.json" 2>/dev/null; then
    printf 'Note: Run `/statusline delete` inside `agy` CLI to clear statusLine in %s/settings.json.\n' "$CLI_DIR"
  fi
fi

printf '==> agy-hud has been cleanly uninstalled and official defaults restored.\n'

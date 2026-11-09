#!/bin/sh
# One-line zero-Rust installer for agy-hud:
#   curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/install.sh | sh -s -- --stack
#   curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/install.sh | sh -s -- --uninstall
set -eu

REPO="${AGY_HUD_REPO:-maxlee/agy-hud}"
VERSION="${AGY_HUD_VERSION:-latest}"

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

if [ "${1:-}" = "--uninstall" ] || [ "${1:-}" = "uninstall" ]; then
  shift
  if [ -x "$HUD_HOME/bin/agy-hud" ]; then
    printf '==> Running native agy-hud clean uninstaller...\n'
    "$HUD_HOME/bin/agy-hud" uninstall --plugin "$@"
    printf '==> agy-hud has been cleanly uninstalled and official defaults restored.\n'
    exit 0
  fi
  if [ "${AGY_SKIP_PLUGIN_INSTALL:-0}" != "1" ] && command -v agy >/dev/null 2>&1; then
    agy plugin uninstall agy-hud >/dev/null 2>&1 || true
  fi
  if [ -d "$HUD_HOME" ] && [ "$HUD_HOME" != "$CLI_DIR" ] && [ "$HUD_HOME" != "$HOME" ]; then
    rm -rf "$HUD_HOME"
  fi
  printf '==> agy-hud directory cleaned.\n'
  exit 0
fi

detect_platform() {
  os_raw=$(uname -s 2>/dev/null || echo unknown)
  arch_raw=$(uname -m 2>/dev/null || echo unknown)
  case "$os_raw" in
    Darwin) os="darwin" ;;
    Linux)  os="linux" ;;
    *)
      printf 'Unsupported operating system: %s (Windows users: download agy-hud-windows-x64.zip from https://github.com/%s/releases)\n' "$os_raw" "$REPO" >&2
      exit 1
      ;;
  esac
  case "$arch_raw" in
    arm64|aarch64) arch="arm64" ;;
    x86_64|amd64)  arch="x64" ;;
    *)
      printf 'Unsupported CPU architecture: %s\n' "$arch_raw" >&2
      exit 1
      ;;
  esac
  printf '%s-%s' "$os" "$arch"
}

fetch_file() {
  url="$1"
  dest="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$dest"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$dest" "$url"
  else
    printf 'Error: neither curl nor wget is installed.\n' >&2
    exit 1
  fi
}

compute_sha256() {
  file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$file" | awk '{print $NF}'
  else
    printf 'Error: no SHA-256 tool found (need sha256sum, shasum, or openssl).\n' >&2
    exit 1
  fi
}

PLATFORM=$(detect_platform)
ARCHIVE_NAME="agy-hud-${PLATFORM}.tar.gz"

PREV_VERSION=""
if [ -x "$HUD_HOME/bin/agy-hud" ]; then
  PREV_VERSION=$("$HUD_HOME/bin/agy-hud" --version 2>/dev/null | awk '{print $2}' || true)
fi

if [ "$VERSION" = "latest" ]; then
  BASE_URL="https://github.com/${REPO}/releases/latest/download"
else
  BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
fi

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/agy-hud-install.XXXXXX")
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

if [ -n "${AGY_HUD_ARCHIVE_DIR:-}" ]; then
  printf '==> Using local release bundle from %s\n' "$AGY_HUD_ARCHIVE_DIR"
  cp "$AGY_HUD_ARCHIVE_DIR/$ARCHIVE_NAME" "$TMP_DIR/$ARCHIVE_NAME"
  cp "$AGY_HUD_ARCHIVE_DIR/SHA256SUMS" "$TMP_DIR/SHA256SUMS"
else
  printf '==> Downloading %s (%s) from %s...\n' "$ARCHIVE_NAME" "$VERSION" "$REPO"
  fetch_file "${BASE_URL}/${ARCHIVE_NAME}" "$TMP_DIR/$ARCHIVE_NAME"
  fetch_file "${BASE_URL}/SHA256SUMS" "$TMP_DIR/SHA256SUMS"
fi

EXPECTED_HASH=$(awk -v target="$ARCHIVE_NAME" '$2 == target || $2 == ("*" target) { print $1; exit }' "$TMP_DIR/SHA256SUMS")
if [ -z "$EXPECTED_HASH" ]; then
  printf 'Error: checksum entry for %s not found in SHA256SUMS.\n' "$ARCHIVE_NAME" >&2
  exit 1
fi

ACTUAL_HASH=$(compute_sha256 "$TMP_DIR/$ARCHIVE_NAME")
if [ "$EXPECTED_HASH" != "$ACTUAL_HASH" ]; then
  printf 'Error: SHA-256 mismatch for %s!\n  Expected: %s\n  Actual:   %s\n' "$ARCHIVE_NAME" "$EXPECTED_HASH" "$ACTUAL_HASH" >&2
  exit 1
fi
printf '==> SHA-256 verified (%s)\n' "$ACTUAL_HASH"

mkdir -p "$TMP_DIR/extract"
tar -xzf "$TMP_DIR/$ARCHIVE_NAME" -C "$TMP_DIR/extract"

EXTRACTED_PLUGIN="$TMP_DIR/extract/agy-hud"
BIN_IN_BUNDLE="$EXTRACTED_PLUGIN/scripts/bin/$PLATFORM/agy-hud"
if [ ! -f "$EXTRACTED_PLUGIN/plugin.json" ] || [ ! -f "$BIN_IN_BUNDLE" ]; then
  printf 'Error: release archive %s is missing expected plugin files.\n' "$ARCHIVE_NAME" >&2
  exit 1
fi
chmod 0755 "$BIN_IN_BUNDLE" "$EXTRACTED_PLUGIN/scripts/agy-hud" "$EXTRACTED_PLUGIN/skills/hud/scripts/hud-cli.sh"
if [ -f "$EXTRACTED_PLUGIN/skills/hud-update/scripts/hud-cli.sh" ]; then
  chmod 0755 "$EXTRACTED_PLUGIN/skills/hud-update/scripts/hud-cli.sh"
fi
if [ -f "$EXTRACTED_PLUGIN/uninstall.sh" ]; then
  chmod 0755 "$EXTRACTED_PLUGIN/uninstall.sh"
fi

BUNDLE_VERSION=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$EXTRACTED_PLUGIN/plugin.json" | head -n 1)
if [ -z "$BUNDLE_VERSION" ]; then
  BUNDLE_VERSION=$("$BIN_IN_BUNDLE" --version 2>/dev/null | awk '{print $2}' || echo "unknown")
fi

if [ -n "$PREV_VERSION" ] && [ "$PREV_VERSION" != "$BUNDLE_VERSION" ]; then
  printf '==> Installing agy-hud v%s (upgrading from v%s)...\n' "$BUNDLE_VERSION" "$PREV_VERSION"
elif [ -n "$PREV_VERSION" ]; then
  printf '==> Reinstalling agy-hud v%s...\n' "$BUNDLE_VERSION"
else
  printf '==> Installing agy-hud v%s...\n' "$BUNDLE_VERSION"
fi

mkdir -p "$CLI_DIR" "$HUD_HOME"
chmod 0700 "$CLI_DIR" "$HUD_HOME" 2>/dev/null || true

PLUGIN_STAGE="$HUD_HOME/plugin"
rm -rf "${PLUGIN_STAGE}.next"
mv "$EXTRACTED_PLUGIN" "${PLUGIN_STAGE}.next"
rm -rf "$PLUGIN_STAGE"
mv "${PLUGIN_STAGE}.next" "$PLUGIN_STAGE"

PLUGIN_FLAG=""
if [ "${AGY_SKIP_PLUGIN_INSTALL:-0}" != "1" ] && command -v agy >/dev/null 2>&1; then
  printf '==> Registering Antigravity plugin (/agy-hud:hud & /agy-hud:hud-update skills) [v%s]...\n' "$BUNDLE_VERSION"
  if agy plugin install "$PLUGIN_STAGE"; then
    PLUGIN_FLAG="--plugin-installed"
  else
    printf 'Warning: `agy plugin install` returned non-zero; continuing with statusLine binary deployment.\n' >&2
  fi
fi

printf '==> Deploying native agy-hud v%s statusLine binary...\n' "$BUNDLE_VERSION"
if [ -n "$PLUGIN_FLAG" ]; then
  "$PLUGIN_STAGE/scripts/bin/$PLATFORM/agy-hud" install "$PLUGIN_FLAG" "$@"
else
  "$PLUGIN_STAGE/scripts/bin/$PLATFORM/agy-hud" install "$@"
fi
printf '==> Done! agy-hud v%s is active. Launch `agy` and try `/agy-hud:hud` or `/agy-hud:hud-update` (if `agy` is already open, run `/skills reload` first).\n' "$BUNDLE_VERSION"

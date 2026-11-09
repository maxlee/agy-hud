#!/bin/sh
# Maintainer helper: downloads & SHA-256-verifies multi-platform binaries from a GitHub Actions
# release workflow run (--from-run <run_id>) or published GitHub Release (--tag vX.Y.Z) into
# scripts/bin/{darwin-arm64,darwin-x64,linux-arm64,linux-x64}/agy-hud with 0755 permissions for
# Antigravity Marketplace bundling.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
REPO="${AGY_HUD_REPO:-maxlee/agy-hud}"

MODE=""
TARGET_VAL=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --from-run)
      MODE="run"
      TARGET_VAL="${2:-}"
      shift 2
      ;;
    --tag)
      MODE="tag"
      TARGET_VAL="${2:-}"
      shift 2
      ;;
    -h|--help)
      printf 'Usage:\n  %s --from-run <gh_actions_run_id>\n  %s --tag <vX.Y.Z>\n' "$0" "$0"
      exit 0
      ;;
    *)
      printf 'Unknown argument: %s\n' "$1" >&2
      exit 1
      ;;
  esac
done

if [ -z "$MODE" ] || [ -z "$TARGET_VAL" ]; then
  printf 'Error: specify either `--from-run <run_id>` or `--tag <vX.Y.Z>`.\n' >&2
  exit 1
fi

calc_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  fi
}

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/agy-hud-sync.XXXXXX")
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

ART_DIR="$TMP_DIR/artifacts"
mkdir -p "$ART_DIR"

PLATFORMS="darwin-arm64 darwin-x64 linux-arm64 linux-x64"

if [ "$MODE" = "run" ]; then
  printf '==> Downloading release-bundle-all from GitHub Actions run %s (%s)...\n' "$TARGET_VAL" "$REPO"
  gh run download "$TARGET_VAL" --repo "$REPO" --name release-bundle-all --dir "$ART_DIR"
else
  printf '==> Downloading release archives for %s (%s)...\n' "$TARGET_VAL" "$REPO"
  BASE_URL="https://github.com/${REPO}/releases/download/${TARGET_VAL}"
  curl -fsSL "${BASE_URL}/SHA256SUMS" -o "$ART_DIR/SHA256SUMS"
  for plat in $PLATFORMS; do
    curl -fsSL "${BASE_URL}/agy-hud-${plat}.tar.gz" -o "$ART_DIR/agy-hud-${plat}.tar.gz"
  done
fi

for plat in $PLATFORMS; do
  archive="agy-hud-${plat}.tar.gz"
  expected=$(awk -v t="$archive" '$2 == t || $2 == ("*" t) { print $1; exit }' "$ART_DIR/SHA256SUMS")
  if [ -z "$expected" ]; then
    printf 'Error: missing checksum for %s in SHA256SUMS\n' "$archive" >&2
    exit 1
  fi
  actual=$(calc_sha256 "$ART_DIR/$archive")
  if [ "$expected" != "$actual" ]; then
    printf 'Error: SHA-256 mismatch for %s (expected %s, got %s)\n' "$archive" "$expected" "$actual" >&2
    exit 1
  fi
  printf '  [verified] %s (%s)\n' "$archive" "$actual"

  ex_dir="$TMP_DIR/extract-$plat"
  mkdir -p "$ex_dir"
  tar -xzf "$ART_DIR/$archive" -C "$ex_dir"
  src_bin="$ex_dir/agy-hud/scripts/bin/$plat/agy-hud"
  if [ ! -f "$src_bin" ]; then
    printf 'Error: binary missing inside %s\n' "$archive" >&2
    exit 1
  fi

  dest_dir="$REPO_ROOT/scripts/bin/$plat"
  mkdir -p "$dest_dir"
  cp "$src_bin" "$dest_dir/agy-hud"
  chmod 0755 "$dest_dir/agy-hud"
done

HOST_OUT=$("$REPO_ROOT/scripts/agy-hud" --version)
printf '==> Synced 4 Marketplace binaries under scripts/bin/<os>-<arch>/agy-hud (0755). Host check: %s\n' "$HOST_OUT"

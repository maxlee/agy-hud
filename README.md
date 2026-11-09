# AGY HUD (`agy-hud`) v1.1.0 — 100% Pure Rust Edition

[![CI](https://github.com/maxlee/agy-hud/actions/workflows/ci.yml/badge.svg)](https://github.com/maxlee/agy-hud/actions/workflows/ci.yml)
[![Release](https://github.com/maxlee/agy-hud/actions/workflows/release.yml/badge.svg)](https://github.com/maxlee/agy-hud/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Ultra-fast, native **100% Pure Rust** real-time heads-up display (HUD) plugin for **Google Antigravity CLI (`agy`)**, inspired by [`jarrodwatts/claude-hud`](https://github.com/jarrodwatts/claude-hud).

Displays live agent state, execution mode (`PLAN` / `AUTO` / `ACCEPT` + Vim mode), model & reasoning effort, workspace + multi-VCS status (`git` + worktrees, Jujutsu `jj` bookmarks/change-IDs/conflicts, Mercurial `hg`), fine-grained context window bar, prompt cache hit rate, quota buckets & reset timers, active tools, running subagents, and `task.md` checklist progress right below your CLI prompt — with **~3 ms internal execution time** (`~11–14 ms` end-to-end process wall-clock) and **zero Node.js dependencies**.

> 🌐 English | [简体中文文档](README.zh-CN.md) | [Architecture](ARCHITECTURE.md) | [Releases](https://github.com/maxlee/agy-hud/releases)

## Preview

```text
╭─ 🔧 TOOL │ [AUTO] │ [Gemini 4 Argon · med] │ agy-hud git:(feat/rust-hud* ↑2) │ ⏱ 12m
├─ Ctx ██░····· 28.4% (152k/1.0M) · ⚡75% Cache │ Quota(weekly) ▓···· 16% (4d 18h) │ ~$0.38 · agents 1/2
├─ ◐ run_command: Cargo test execution · ✓ edit_file ×2: render.rs · ✓ read_file ×5: SKILL.md │ Skills: hud
├─ ◐ CLI Docs Researcher [research] (1m 15s) │ ✓ Security Auditor [self] (1m)
╰─ ▸ Verify compile-time arch_guard boundaries (2/4 · 50%)
```

## Requirements

- **Antigravity CLI (`agy`)** v1.4.5+
- **Zero Rust / Node.js / npm required** when installing prebuilt binaries via `install.sh` or [GitHub Releases](https://github.com/maxlee/agy-hud/releases) (`rustc` 1.85+ only needed if building from source)

## Quick Install

### Option A: One-Line Prebuilt Binary + Plugin Install (Recommended)

Automatically detects your OS (`macOS` Apple Silicon / Intel, `Linux` x64 / ARM64), verifies the SHA-256 checksum against `SHA256SUMS`, registers the `/hud` skill plugin, deploys the native binary to `~/.gemini/antigravity-cli/agy-hud/bin/agy-hud`, and configures `settings.json`:

```bash
curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/install.sh | sh
```

Need to stack below the built-in status line or replace an existing custom `statusLine`? Pass flags via `sh -s --`:

```bash
curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/install.sh | sh -s -- --stack
```

Inside `agy` CLI, run `/hud` anytime to customize layouts, language, or elements interactively, or `/statusline` to toggle the bar on/off.

### Option B: Prebuilt Release Archives (macOS / Linux / Windows)

Download the archive for your platform from [**GitHub Releases**](https://github.com/maxlee/agy-hud/releases/latest):

| Platform | Archive | Target Triple |
| :--- | :--- | :--- |
| macOS (Apple Silicon) | `agy-hud-darwin-arm64.tar.gz` | `aarch64-apple-darwin` |
| macOS (Intel) | `agy-hud-darwin-x64.tar.gz` | `x86_64-apple-darwin` |
| Linux (x64, static musl) | `agy-hud-linux-x64.tar.gz` | `x86_64-unknown-linux-musl` |
| Linux (ARM64, static musl) | `agy-hud-linux-arm64.tar.gz` | `aarch64-unknown-linux-musl` |
| Windows (x64) | `agy-hud-windows-x64.zip` | `x86_64-pc-windows-msvc` |

### Option C: Build from Source (`git clone` + `cargo`)

```bash
git clone https://github.com/maxlee/agy-hud.git
cd agy-hud
cargo build --release
agy plugin install .
./target/release/agy-hud install
```

## CLI Subcommands

Once installed (`~/.gemini/antigravity-cli/agy-hud/bin/agy-hud` or `./scripts/agy-hud` inside a checkout):

```bash
./scripts/agy-hud doctor                                    # JSON health check (runtime, plugin, statusLine, config)
./scripts/agy-hud doctor --pretty                           # Human-readable diagnostics (~ paths + glyph alignment probe)
./scripts/agy-hud preview --state showcase                  # Preview all 5 layout/language scenarios
./scripts/agy-hud preview --set language=zh-Hans            # Preview Simplified Chinese UI
./scripts/agy-hud schema                                    # List all 41 configurable keys and types
./scripts/agy-hud set display.glyphMode=nerd                # Switch glyph set: unicode | nerd | ascii
./scripts/agy-hud set display.showTitle=true                # Show sanitized conversation title (<=24 cols)
./scripts/agy-hud set quota.backgroundRefresh=true          # Optional background `agy -p /usage` refresh (default: false)
./scripts/agy-hud quota refresh                             # Manually refresh official `/usage` quota cache once
./scripts/agy-hud set language=zh-Hans vcs.showFileStats=true
./scripts/agy-hud set vcs.cacheTtlMs=1500                   # Tune sharded VCS mtime+TTL cache window (0 disables)
./scripts/agy-hud set --undo                                # Revert the previous config change
./scripts/agy-hud report --note "Describe issue"            # Generate privacy-scrubbed bug report JSON
./scripts/agy-hud uninstall                                 # Cleanly remove HUD, plugin & files; restore official default
```

### Clean Uninstall (Zero Residue & Restore Official Default)

Uninstall at any time with one command — removes `"statusLine"` from `~/.gemini/antigravity-cli/settings.json` (restoring Antigravity CLI's official default status bar while preserving all other settings), cleans tracked `.bak.*` backups, unregisters the `agy-hud` plugin, and deletes `~/.gemini/antigravity-cli/agy-hud`:

```bash
curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/uninstall.sh | sh
```

(Or offline: `~/.gemini/antigravity-cli/agy-hud/bin/agy-hud uninstall --plugin`. Add `--keep-config` if you wish to retain `config.json`.)

Disable the HUD for a single terminal session without changing `settings.json`:

```bash
AGY_HUD_DISABLE=1 agy
```

## Security & Performance

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design:

- **Compile-time crate isolation (`hud-core` vs. `hud-cli`)**: `crates/hud-core` enforces `#![forbid(unsafe_code)]`, depends only on `serde` + `serde_json`, and is statically verified by `arch_guard.rs` to contain **zero** `std::fs`, `std::process`, `std::net`, or `std::env` capabilities.
- **Hardened read-only VCS probes**: Enforces `-c protocol.allow=never`, `-c core.fsmonitor=false`, `--no-optional-locks`, dynamic repo-local `filter.*` neutralization for Git, `--ignore-working-copy --at-operation=@ --no-pager` for Jujutsu (`jj`), and `HGPLAIN=1` for Mercurial (`hg`).
- **Sharded per-workspace cache**: Stores VCS snapshots at `<hudHome>/probe-cache/<fnv64>.json`, eliminating multi-terminal write races.
- **Terminal escape firewall (`terminal_filter`)**: Strips OSC 52 clipboard writes, title hijacks, cursor CSI escapes, and Unicode bidi overrides, permitting only SGR color sequences and `file://` OSC 8 links.

## Development & Verification

```bash
cargo fmt --all -- --check                             # Formatting check
cargo clippy --workspace --all-targets -- -D warnings  # Strict zero-warning lint check
cargo test --workspace                                 # Unit, CLI e2e, Git filter canary, and arch_guard tests
cargo build --release                                  # LTO stripped native binary (~929 KB)
agy plugin validate --strict .                         # Antigravity strict plugin schema check
```

## License

Released under the [MIT License](LICENSE).

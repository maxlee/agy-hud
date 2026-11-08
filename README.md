# AGY HUD (`agy-hud`) v1.0.0 — 100% Pure Rust Edition

Ultra-fast, native **100% Pure Rust** real-time heads-up display (HUD) plugin for **Google Antigravity CLI (`agy`)**, inspired by [`jarrodwatts/claude-hud`](https://github.com/jarrodwatts/claude-hud).

Displays live agent state, execution mode (`PLAN` / `AUTO` / `ACCEPT` + Vim mode), model & reasoning effort, workspace + multi-VCS status (`git` + worktrees, Jujutsu `jj` bookmarks/change-IDs/conflicts, Mercurial `hg`), fine-grained context window bar, prompt cache hit rate, quota buckets & reset timers, active tools, running subagents, and `task.md` checklist progress right below your CLI prompt — with **~3 ms internal execution time** (`~11–14 ms` end-to-end process wall-clock) and **zero Node.js dependencies**.

> 🌐 English | [简体中文文档](README.zh-CN.md) | [Architecture](ARCHITECTURE.md)

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
- **Rust / Cargo** (`rustc` 1.85+ if building from source; prebuilt `scripts/bin/agy-hud` included)
- **Zero Node.js / npm runtime required**

## Quick Install

### Option A: Install as an Antigravity Plugin + Deploy Native Binary

```bash
# 1. Build (optional if scripts/bin/agy-hud is already built for your platform)
cargo build --release && cp target/release/agy-hud scripts/bin/agy-hud

# 2. Validate & install the plugin into Antigravity
agy plugin validate --strict /path/to/agy-hud
agy plugin install /path/to/agy-hud

# 3. Deploy native binary to ~/.gemini/antigravity-cli/agy-hud/bin/agy-hud & update settings.json
/path/to/agy-hud/scripts/bin/agy-hud install
```

Inside `agy` CLI, run `/hud` anytime to customize layouts, language, or elements interactively, or `/statusline` to toggle the bar on/off.

### Option B: Direct `/statusline` Command Inside `agy` TUI

```text
/statusline /path/to/agy-hud/scripts/bin/agy-hud
```

## CLI Subcommands

```bash
scripts/bin/agy-hud doctor                                    # Check setup, native binary, heartbeat, and config
scripts/bin/agy-hud preview --state showcase                  # Preview all 5 layout/language scenarios
scripts/bin/agy-hud preview --set language=zh-Hans            # Preview Simplified Chinese UI
scripts/bin/agy-hud schema                                    # List all 38 configurable keys and types
scripts/bin/agy-hud set language=zh-Hans vcs.showFileStats=true
scripts/bin/agy-hud set vcs.cacheTtlMs=1500                   # Tune sharded VCS mtime+TTL cache window (0 disables)
scripts/bin/agy-hud set --undo                                # Revert the previous config change
scripts/bin/agy-hud report --note "Describe issue"            # Generate privacy-scrubbed bug report JSON
scripts/bin/agy-hud uninstall                                 # Remove statusLine (or restore previous script)
```

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
cargo test --workspace                                 # Unit, CLI e2e, Git filter canary, and arch_guard tests
cargo clippy --workspace --all-targets -- -D warnings  # Strict zero-warning lint check
cargo build --release                                  # LTO stripped native binary (~929 KB)
agy plugin validate --strict .                         # Antigravity strict plugin schema check
```

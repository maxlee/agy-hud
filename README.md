<div align="center">

<img src="assets/logo.svg" alt="AGY HUD Logo" width="88" height="88" />

# AGY HUD (`agy-hud`)

**Ultra-fast, zero-dependency, 100% Pure Rust real-time statusline HUD for Google Antigravity CLI (`agy`)**

[![CI](https://github.com/maxlee/agy-hud/actions/workflows/ci.yml/badge.svg)](https://github.com/maxlee/agy-hud/actions/workflows/ci.yml)
[![Release](https://github.com/maxlee/agy-hud/actions/workflows/release.yml/badge.svg)](https://github.com/maxlee/agy-hud/releases/latest)
[![Rust 2024](https://img.shields.io/badge/Rust-2024_Edition-orange.svg)](ARCHITECTURE.md)
[![Unsafe Forbidden](https://img.shields.io/badge/unsafe-forbidden-brightgreen.svg)](ARCHITECTURE.md)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

[**English**](README.md) · [**简体中文**](README.zh-CN.md) · [**Quick Install**](#-installation) · [**Features**](#-highlights) · [**CLI & Config**](#️-cli--configuration) · [**Architecture**](ARCHITECTURE.md) · [**Releases**](https://github.com/maxlee/agy-hud/releases)

</div>

---

**AGY HUD** renders a live, theme-adaptive heads-up display directly beneath your **Antigravity CLI (`agy`)** prompt — surfacing agent state, execution mode, model & reasoning effort, TTFT-stripped token throughput (`~tok/s`), multi-VCS workspace status (`git`, `jj`, `hg`), context window pressure, prompt cache hit rate, quota pace, active tools, subagents, and `task.md` checklist progress in **~1–3 ms** warm execution time with **zero Node.js or runtime VM dependencies**.

> Inspired by [`jarrodwatts/claude-hud`](https://github.com/jarrodwatts/claude-hud) and purpose-engineered from the ground up in safe Rust (`#![forbid(unsafe_code)]`) for the Antigravity CLI plugin & `statusLine` ecosystem.

---

## 📸 Live Preview

### Standard Multi-Row Expanded Layout (`76–179` cols)

```text
╭─ 🔧 TOOL │ [AUTO] │ [Gemini 4 Argon · med] Pro · 🚗 ~118 tok/s (64~142) │ agy-hud git:(feat/rust-hud* ↑2) │ ⏱ 12m
├─ Ctx ████▓··· 59.3% (152k/256k · ↻2) · ⚡ 75% Cache │ ~$0.38 · agents 1/2 · tasks 1 · art 2 · 🛡 Sandbox ON
├─ Quota(weekly) ▓···· 16% (4d 18h)
├─ ◐ run_command: Cargo test execution · ✓ edit_file ×2: render.rs · ✓ read_file ×5: SKILL.md
├─ Skills: hud, agy-customizations │ MCP: context7
├─ ◐ CLI Docs Researcher [research] (1m 15s) │ ◷ Waiter [self] (20s) │ ✓ Security Auditor [self] (1m)
╰─ ▸ Verify compile-time arch_guard boundaries (2/4 · 50%)
```

### Ultra-Wide Single-Line Idle (`>= 180` cols) & Narrow Auto-Compact (`< 76` cols)

```text
# Ultra-wide terminal (>= 180 cols, idle) — collapses cleanly into one crisp line:
✓ IDLE │ [AUTO] │ [Gemini 4 Argon · med] Pro · 🚗 ~118 tok/s (64~142) │ agy-hud git:(main) │ ⏱ 5m │ Ctx ███████····· 59.3% (152k/256k) · ⚡ 75% Cache │ Quota(weekly) ▓···· 16% (4d 18h)

# Narrow split pane (< 76 cols, high context) — progressive degradation without wrapping:
⚡ WORK │ [AUTO] │ [Argon · M] │ agy-hud git:(main*)
Ctx ███████░ 89.6% !200k │ Quota(weekly) 92% (1h 30m)
◐ run_command: Cargo test execution
```

### HUD Anatomy

| Row / Segment | What It Shows | Key Config Toggle |
| :--- | :--- | :--- |
| **State & Mode** | Live `IDLE` / `WORK` / `TOOL` / `CONFIRM` state, `[PLAN]` / `[AUTO]` / `[ACCEPT]` cycle mode (plus `/plan` detection & `VIM` mode badge) | `display.showState`, `display.showMode` |
| **Model, Tier & Speed** | Compact model name, reasoning effort (`low`/`med`/`high`), normalized plan tier (`Pro`/`Ultra`), and TTFT-stripped `~tok/s` decode rate with speed-tier emoji (`🐢`/`🛵`/`🚗`/`🚅`/`✈️`/`🚀`/`🛰️`/`🛸`/`👽`, hidden in `ascii` mode) and session `(min~max)` range | `display.showModel`, `display.showPlanTier`, `display.showSpeed`, `display.glyphMode` |
| **Workspace & VCS** | Clickable `file://` OSC 8 project path (plus optional `project › subdir` breadcrumb), clickable GitHub branch OSC 8 links, `git` (worktrees, dirty, unpushed `↑N` thresholds, file stats), `jj` (bookmarks, `@change_id`, conflicts), or `hg` | `display.showProject`, `display.showLaunchDir`, `vcs.pushWarningThreshold`, `vcs.showFileStats` |
| **Context & Meta** | Compaction-watermark-scaled (`256k`) 8th-block Unicode context bar + `↻N` auto-compaction counter badge, `200k+` alert, prompt `Cache` hit %, session/daily/weekly cost (`Today` / `Week`), subagent/task/artifact counts, Sandbox badge | `display.contextScale`, `display.showContextBar`, `display.showDailyCost`, `display.showWeeklyCost` |
| **Quota Pace** | Model-routed quota buckets (`5h` / `weekly`), burn-rate projection (`▲` pace alert when projected > 90% / > 100% before reset), optional low-usage threshold hiding, and reset countdown | `display.showQuota`, `display.quotaPace`, `display.quotaThreshold`, `quota.backgroundRefresh` |
| **Activity & Composable Rows** | Folded tool invocations (`×N` deduplication), active `Skills` & `MCP` servers (`!` error alerts), `Subagents` (`◐`/`◷`/`✗`/`✓` with 60s completed retirement), `task.md` progress, and customizable row merging / right-alignment | `display.mergeGroups`, `display.rightAlign`, `display.showTools`, `display.showSubagents`, `display.showTodos` |

---

## ✨ Highlights

- **⚡ Native ~1–3 ms Warm Execution + Incremental `O(Δ)` Cursor**: Single LTO-optimized binary (`~930 KB`), zero VM startup overhead, `std::thread::scope` parallel probes, and an incremental `transcript.jsonl` byte-offset cursor (`transcript-cursors/<fnv64>.json`) that preserves early skills/MCP/tool counts even in multi-megabyte sessions.
- **🎯 Hybrid TTFT-Stripped `~tok/s` & Quota Burn-Rate (`▲`)**: Separates `thinking`/prefill latency (`TTFT`) and tool wait time from pure streaming decode throughput, displaying a speed-tier emoji (`🐢` `<50`, `🛵` `50–99`, `🚗` `100–149`, `🚅` `150–199`, `✈️` `200–249`, `🚀` `250–299`, `🛰️` `300–399`, `🛸` `400–499`, `👽` `500+`; hidden when `display.glyphMode=ascii`) and session `(min~max)` range from completed steps / native `stdin` rates (preserved across model switches, shown once two distinct rounded speeds exist; toggled via `display.showSpeed`). Meanwhile `display.quotaPace` projects `5h`/`weekly` quota burn velocity (`▲` amber/red warning before you hit 0%) and `display.showDailyCost` / `showWeeklyCost` tracks cross-session spend without double counting.
- **🎨 Theme-Safe, Grapheme-Cluster-Accurate & Composable Layout**: Zero hardcoded white (`37m`/`97m`) or gray (`90m`) foreground codes. Full Unicode extended grapheme cluster width engine (ZWJ emoji sequences, skin-tone modifiers, regional flag pairs, CJK 2-col alignment), `display.mergeGroups` + `display.rightAlign` row composition, and 3 glyph sets (`unicode`, `nerd`, `ascii`).
- **🌿 Hardened `git`, `jj` & `hg` Support**: Windows `CreateProcessW` `PATH`-hijack protection, scope-aware Git `[include]` filter neutralization (preserving global `git-lfs` while disarming untrusted repo filters, with zero-subprocess `.git/HEAD` fallback), clickable GitHub branch OSC 8 links, unpushed `↑N` thresholds, and FNV-1a sharded `mtime` caches.
- **🔄 Built-in Verified Self-Update**: Non-blocking 6-hour release check surfaces a subtle header badge (`↑ vX.Y.Z · /agy-hud:hud-update`) and upgrades in one SHA-256-verified step while preserving `--stack` and user `config.json`.
- **🌏 Native Bilingual UI**: Switch effortlessly between English (`en`) and Simplified Chinese (`zh-Hans`).

---

## 📦 Installation

### Requirements

- **Antigravity CLI (`agy`)** `v1.4.5+` (fully aligned with `v1.5.4+` schemas)
- **Zero Rust / Node.js / npm required** for the one-line installer, GitHub plugin, or prebuilt installations (`rustc 1.85+` only if compiling from source)

> **Marketplace status**: Submitted to the Antigravity Marketplace and currently under review. Once approved, you'll also be able to add **AGY HUD** directly from **Customizations → Marketplace** in Antigravity Desktop and click the **"Set up the HUD status line in Antigravity CLI"** starter prompt chip.

### Method 1: One-Line SHA-256 Verified Installer (Recommended — macOS & Linux)

Detects your OS/CPU (`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`), verifies the archive against `SHA256SUMS`, registers `/agy-hud:hud` & `/agy-hud:hud-update`, deploys `~/.gemini/antigravity-cli/agy-hud/bin/agy-hud`, and atomically updates `settings.json`:

```bash
curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/install.sh | sh
```

Want to stack **below** Antigravity's built-in status bar instead of replacing it?

```bash
curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/install.sh | sh -s -- --stack
```

### Method 2: Install via `agy` CLI from GitHub (macOS & Linux)

Install the plugin directly from the GitHub repository (no Marketplace listing required):

```bash
agy plugin install https://github.com/maxlee/agy-hud
```

Then launch `agy` and run `/agy-hud:hud` (or click the **"Set up the HUD status line in Antigravity CLI"** starter prompt chip if installed via Antigravity Desktop) to activate the status line in `~/.gemini/antigravity-cli/settings.json` *(if `agy` was already open, run `/skills reload` first)*.

### Method 3: Prebuilt Release Archives (macOS / Linux / Windows)

Download prebuilt binaries and `SHA256SUMS` from [**GitHub Releases**](https://github.com/maxlee/agy-hud/releases/latest):

| Platform | Archive | Rust Target Triple | Notes |
| :--- | :--- | :--- | :--- |
| **macOS (Apple Silicon)** | `agy-hud-darwin-arm64.tar.gz` | `aarch64-apple-darwin` | Native M1 / M2 / M3 / M4 |
| **macOS (Intel)** | `agy-hud-darwin-x64.tar.gz` | `x86_64-apple-darwin` | Native x86_64 |
| **Linux (x64)** | `agy-hud-linux-x64.tar.gz` | `x86_64-unknown-linux-musl` | Fully static `musl` (zero `glibc` issues) |
| **Linux (ARM64)** | `agy-hud-linux-arm64.tar.gz` | `aarch64-unknown-linux-musl` | Fully static `musl` |
| **Windows (x64)** | `agy-hud-windows-x64.zip` | `x86_64-pc-windows-msvc` | Native `agy-hud.exe` (quote-safe path handling) |

### Method 4: Build from Source

```bash
git clone https://github.com/maxlee/agy-hud.git
cd agy-hud
cargo build --release
agy plugin install .
./target/release/agy-hud install
```

---

## 🛠️ CLI & Configuration

### In-Session Slash Commands (inside `agy`)

| Command | Description |
| :--- | :--- |
| `/agy-hud:hud` | Interactively install, diagnose, preview, or customize layout, language, and badges in natural language |
| `/agy-hud:hud-update` | Check GitHub Releases and perform a SHA-256-verified binary + plugin self-upgrade in one step |
| `/statusline` | Built-in `agy` command to toggle the status bar on or off |

### Native CLI Subcommands

Invoke via `~/.gemini/antigravity-cli/agy-hud/bin/agy-hud` (or `./scripts/agy-hud` inside a checkout):

| Subcommand | Purpose |
| :--- | :--- |
| `install [--stack] [--replace]` | Deploy native binary and configure `statusLine` in `settings.json` (creates timestamped backup) |
| `uninstall [--plugin] [--keep-config]` | Remove `statusLine`, clean backups & HUD directory, and restore official `agy` default |
| `doctor [--pretty]` | Inspect runtime, plugin enablement, `statusLine` wiring, config validity, and glyph alignment |
| `preview [--state <name>] [--last] [--set k=v]` | Render `showcase`, `idle`, `tool_use`, `confirm`, `high-context`, inline JSON, or last live snapshot |
| `schema [query]` | Browse all **53** validated configuration keys, types, defaults, and descriptions |
| `set <key>=<value> ...` / `set --undo` | Atomically update `~/.gemini/antigravity-cli/agy-hud/config.json` or revert the previous change |
| `update [--check] [--force]` | Check for or apply the latest SHA-256-verified release from GitHub |
| `quota refresh` | Run an isolated one-shot `agy -p /usage` sync to reconcile local quota cache |
| `report [--note "..."]` | Export a width-preserving privacy-scrubbed diagnostic JSON bundle + replayable `.case` regression fixture |

### Common Customization Recipes

```bash
# Preview all 5 responsive layout & language presets
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud preview --state showcase

# Switch UI to Simplified Chinese and enable +/- VCS file stats
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set language=zh-Hans vcs.showFileStats=true

# Enable cross-session Today/Week cost ledger, launch-dir breadcrumb & Git push alerts
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set display.showDailyCost=true display.showWeeklyCost=true display.showLaunchDir=true vcs.pushWarningThreshold=5 vcs.pushCriticalThreshold=15

# Merge Context + Quota onto one row (with automatic narrow-terminal CJK column stacking) and right-align Quota
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set 'display.mergeGroups=[["context","quota"]]' 'display.rightAlign=["quota"]'

# Hide quota bar until 25% used (automatically surfaces earlier if burn pace `▲` is fast)
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set display.quotaThreshold=25 display.quotaPace=true

# Switch context bar denominator between 256k auto-compaction watermark (default) and raw 1.0M model ceiling
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set display.contextScale=compaction

# Switch glyph mode: unicode (default) | nerd (Nerd Fonts) | ascii (strict 7-bit)
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set display.glyphMode=nerd

# Customize header badges (conversation title, plan tier, ~tok/s speed tier & min~max range, project folder)
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set display.showTitle=true display.showSpeed=true
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set display.showProject=false vcs.enabled=true

# Undo the most recent `set` command
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set --undo
```

### Environment Variables

| Variable | Description |
| :--- | :--- |
| `AGY_HUD_DISABLE=1` | Suppress HUD output for the current terminal session without modifying `settings.json` (`AGY_HUD_DISABLE=1 agy`) |
| `AGY_CLI_DIR` | Override Antigravity CLI config directory (default: `~/.gemini/antigravity-cli`) |
| `AGY_HUD_HOME` | Override HUD data & config directory (default: `~/.gemini/antigravity-cli/agy-hud`) |

---

## 🧹 Clean Uninstall (Zero Residue)

Uninstall at any time with a single command — removes `"statusLine"` from `settings.json` (restoring Antigravity CLI's official default status bar while keeping all your other settings intact), cleans tracked backups, unregisters the plugin, and deletes `~/.gemini/antigravity-cli/agy-hud`:

```bash
curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/uninstall.sh | sh
```

Offline alternative:

```bash
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud uninstall --plugin
```

*(Append `--keep-config` if you want to preserve `config.json` for future reinstalls.)*

---

## 🔒 Architecture & Security (`THE LINE`)

`agy-hud` enforces capability isolation at compile time across a two-crate Rust workspace (continuously verified by [`arch_guard.rs`](crates/hud-core/tests/arch_guard.rs)). See [**ARCHITECTURE.md**](ARCHITECTURE.md) for full details:

```text
agy-hud/
├── crates/
│   ├── hud-core/   # Pure deterministic logic & layout crate (#![forbid(unsafe_code)], ZERO I/O)
│   └── hud-cli/    # Thin capability shell: bounded file seeks, hardened VCS probes, atomic cache I/O
├── skills/         # Antigravity slash-command skills (/agy-hud:hud & /agy-hud:hud-update)
├── scripts/        # POSIX platform router & SHA-256 Marketplace binary sync tooling
└── plugin.json     # Strict Antigravity v1 plugin manifest
```

1. **Zero-I/O Pure Core (`hud-core`)**: Depends **only** on `serde` and `serde_json` (11 total transitive crates) and is statically forbidden from referencing `std::fs`, `std::process`, `std::net`, `std::env`, or `std::os`.
2. **Command-Injection-Safe VCS Probes (`hud-cli`)**:
   - **Git**: Enforces `-c protocol.allow=never`, `-c core.fsmonitor=false`, `--no-optional-locks`, `GIT_NO_LAZY_FETCH=1`, and dynamically parses `.git/config` in memory to neutralize repo-local `[filter "<name>"]` `clean`/`smudge`/`process` drivers before invoking `git`.
   - **Jujutsu (`jj`) & Mercurial (`hg`)**: Enforces `--ignore-working-copy --at-operation=@ --no-pager` and `HGPLAIN=1 --config extensions.fsmonitor=!` under a strict `1200 ms` timeout.
3. **Terminal Escape Firewall (`terminal_filter`)**: Every line written to `stdout` is scrubbed against OSC 52 clipboard exfiltration, window title hijacking, cursor-movement CSI escapes, and Unicode Bidi Trojans (`U+202A..U+202E`, `U+2066..U+2069`), allowing only SGR styling and local `file://` OSC 8 links.

---

## 🧪 Development & Verification

```bash
cargo fmt --all -- --check                             # Verify Rustfmt formatting
cargo clippy --workspace --all-targets -- -D warnings  # Enforce zero Clippy warnings
cargo test --workspace                                 # Run unit, CLI e2e, Git filter canary & arch_guard suites
cargo build --release                                  # Build stripped LTO native binary
agy plugin validate --strict .                         # Validate Antigravity plugin manifest & skills
```

---

## 📄 License

Distributed under the [**MIT License**](LICENSE). Copyright © Max.

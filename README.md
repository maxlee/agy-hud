# AGY HUD (`agy-hud`)

Real-time heads-up display (HUD) plugin for **Google Antigravity CLI (`agy`)**, inspired by [`jarrodwatts/claude-hud`](https://github.com/jarrodwatts/claude-hud).

Displays live agent state, execution mode (`PLAN` / `AUTO` / `ACCEPT` + Vim mode), model & reasoning effort, workspace + Git status, fine-grained context window bar, prompt cache hit rate, quota buckets & reset timers, active tools, running subagents, and `task.md` checklist progress right below your CLI prompt.

> 🌐 English | [简体中文文档](README.zh-CN.md) | [Architecture](ARCHITECTURE.md)

## Preview

```text
╭─ 🔧 TOOL │ [AUTO] │ [Gemini 4 Argon · med] │ agy-hud git:(feat/agy-hud* ↑2) │ ⏱ 12m
├─ Ctx ███░········ 28.4% (152k/1.0M) · ⚡75% Cache │ Quota(weekly) █······ 16% (4d 18h) │ ~$0.38 · agents 1/2
├─ ◐ run_command: Unit test execution · ✓ edit_file ×2: render.ts · ✓ read_file ×5 │ Skills: hud
├─ ◐ CLI Docs Researcher [research] (1m 15s) │ ✓ Security Auditor [self] (1m)
╰─ ▸ Verify node --permission escape matrix (2/4 · 50%)
```

## Requirements

- **Antigravity CLI (`agy`)** v1.4.5+
- **Node.js** 22.18.0 or later (zero external npm dependencies)

## Quick Install

### Option A: Install as an Antigravity Plugin + Enable Status Line

```bash
# 1. Validate & install the plugin into Antigravity
agy plugin validate --strict /path/to/agy-hud
agy plugin install /path/to/agy-hud

# 2. Configure ~/.gemini/antigravity-cli/settings.json safely (creates automatic backup)
node /path/to/agy-hud/scripts/hud.mjs install
```

Inside `agy` CLI, you can now run `/hud` anytime to customize layouts, language, or elements interactively, or `/statusline` to toggle the bar on/off.

### Option B: Direct `/statusline` Command Inside `agy` TUI

```text
/statusline node /path/to/agy-hud/scripts/hud.mjs
```

## CLI Subcommands

```bash
node scripts/hud.mjs doctor                                    # Check setup, runtime, heartbeat, and config
node scripts/hud.mjs preview --state showcase                  # Preview all 5 layout/language scenarios
node scripts/hud.mjs preview --set language=zh-Hans            # Preview Simplified Chinese UI
node scripts/hud.mjs schema                                    # List all configurable keys and types
node scripts/hud.mjs set language=zh-Hans vcs.showFileStats=true
node scripts/hud.mjs set --undo                                # Revert the previous config change
node scripts/hud.mjs report --note "Describe issue"            # Generate privacy-scrubbed bug report JSON
node scripts/hud.mjs uninstall                                 # Remove statusLine (or restore previous script)
```

To disable the HUD for a single terminal session without changing `settings.json`:

```bash
AGY_HUD_DISABLE=1 agy
```

## Security

Following `claude-hud`'s security model, `agy-hud` runs all TypeScript parsing and rendering logic inside a locked-down Node permission cage (`node --permission --disallow-code-generation-from-strings --import=./scripts/fence.mjs`):

- **No child processes or network**: `node:child_process`, `node:net`, `node:http`, `fetch`, `WebSocket`, and `process.binding` are blocked and sealed.
- **Least-privilege filesystem**: Read access is granted only to the plugin root, HUD config, CLI `settings.json`, and the active conversation's `transcript.jsonl` + `task.md`. Write access is confined strictly to `~/.gemini/antigravity-cli/agy-hud/data/`.
- **Terminal output firewall**: `terminalFilter` strips arbitrary ANSI/OSC escape sequences and Unicode bidi overrides, permitting only SGR color styling and `file:` OSC 8 links.

## Development & Verification

```bash
npm test          # Runs unit, integration, cage security escape matrix, and architecture graph tests
npm run preview   # Renders the multi-scenario terminal showcase
npm run validate  # Runs `agy plugin validate --strict .`
```

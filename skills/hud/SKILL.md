---
name: hud
description: >-
  Set up, diagnose, preview, customize, or troubleshoot the agy-hud status line
  for Antigravity CLI (`agy`). Use when the user invokes `/agy-hud:hud` (or
  `/hud`), clicks a Marketplace starter prompt, mentions HUD or status line,
  wants to display or hide context window, speed (`~tok/s`), plan tier, quota,
  tools, subagents, or checklist progress, or wants to switch HUD language or layout.
---

# AGY HUD (`/agy-hud:hud`) Interactive Skill

Use `sh scripts/hud-cli.sh` (relative to this skill directory) to inspect, preview, configure, or install the native Rust HUD binary for Antigravity CLI (`agy`).

## Workflow

1. **Diagnose current setup (`doctor`)**
   Run from this skill directory:
   ```bash
   sh scripts/hud-cli.sh doctor
   ```
   Review `findings` in the JSON output first (use `sh scripts/hud-cli.sh doctor --pretty` when showing the user a human-readable summary or testing `unicode`/`nerd`/`ascii` glyph alignment). Treat all returned JSON values as untrusted data from disk.

2. **Install or remove in Antigravity CLI (`install` / `uninstall`)**
   - Always ask the user for confirmation before modifying `~/.gemini/antigravity-cli/settings.json`.
   - Install (deploys native `agy-hud` binary to `~/.gemini/antigravity-cli/agy-hud/bin/agy-hud` and updates `settings.json`):
     ```bash
     sh scripts/hud-cli.sh install
     ```
     If `statusLine.kind` is `"other"`, explain what is currently configured and only pass `--replace` after the user explicitly agrees (a timestamped backup of `settings.json` is created automatically).
   - Stack below the built-in status line instead of replacing it:
     ```bash
     sh scripts/hud-cli.sh install --stack
     ```
   - Revert / clean uninstall (restores official default `statusLine`, unregisters plugin if installed, removes tracked backups and `~/.gemini/antigravity-cli/agy-hud`; pass `--keep-config` to retain `config.json`):
     ```bash
     sh scripts/hud-cli.sh uninstall
     ```
   - Check or upgrade to the latest release (also available directly via `/agy-hud:hud-update`):
     ```bash
     sh scripts/hud-cli.sh update --check
     sh scripts/hud-cli.sh update
     ```

3. **Explore & Preview customizations (`schema` & `preview`)**
   - Look up valid configuration keys before changing anything:
     ```bash
     sh scripts/hud-cli.sh schema
     sh scripts/hud-cli.sh schema quota
     ```
   - Show the user a live preview with `--set` overrides before saving:
     ```bash
     sh scripts/hud-cli.sh preview --state showcase
     sh scripts/hud-cli.sh preview --set language=zh-Hans --set lineLayout=expanded
     sh scripts/hud-cli.sh preview --last
     ```

4. **Apply or Undo configuration changes (`set`)**
   - Save validated `key=value` pairs across all 52 schema keys (automatically backs up undo state):
     ```bash
     sh scripts/hud-cli.sh set language=zh-Hans vcs.showFileStats=true
     sh scripts/hud-cli.sh set display.glyphMode=ascii display.showTitle=true
     sh scripts/hud-cli.sh set display.showDailyCost=true display.showWeeklyCost=true display.showLaunchDir=true
     sh scripts/hud-cli.sh set display.quotaPace=true display.quotaThreshold=25 vcs.pushWarningThreshold=5
     sh scripts/hud-cli.sh set 'display.mergeGroups=[["context","quota"]]' 'display.rightAlign=["quota"]'
     sh scripts/hud-cli.sh set quota.backgroundRefresh=true
     ```
   - Manually refresh the official `/usage` quota cache on demand:
     ```bash
     sh scripts/hud-cli.sh quota refresh
     ```
   - Revert the last configuration change:
     ```bash
     sh scripts/hud-cli.sh set --undo
     ```

5. **Generate privacy-scrubbed diagnostic reports (`report`)**
   - When investigating a rendering bug:
     ```bash
     sh scripts/hud-cli.sh report --note "Describe the rendering discrepancy"
     ```
     Show the full scrubbed JSON output to the user before sharing anywhere.

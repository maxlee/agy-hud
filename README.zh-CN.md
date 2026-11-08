# AGY HUD (`agy-hud`) v1.0.1 — 100% 纯 Rust 极速版指南

[![CI](https://github.com/maxlee/agy-hud/actions/workflows/ci.yml/badge.svg)](https://github.com/maxlee/agy-hud/actions/workflows/ci.yml)
[![Release](https://github.com/maxlee/agy-hud/actions/workflows/release.yml/badge.svg)](https://github.com/maxlee/agy-hud/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

参考 [`jarrodwatts/claude-hud`](https://github.com/jarrodwatts/claude-hud) 的核心设计理念，采用 **100% 纯 Rust 原生单内核架构** 为 **Google Antigravity CLI (`agy`)** 打造的实时终端状态栏插件（Heads-Up Display）。

> 🌐 [English](README.md) | 简体中文文档 | [架构设计 (ARCHITECTURE.md)](ARCHITECTURE.md) | [预编译下载 (Releases)](https://github.com/maxlee/agy-hud/releases)

在 `agy` 输入框正下方实时展示：Agent 运行状态、执行模式（`PLAN` / `AUTO` / `ACCEPT` 与 Vim 模式）、当前模型与推理强度（Effort）、项目路径与多版本控制状态（原生支持 `Git` 含 Worktree、`Jujutsu (jj)` 书签/变更 ID/冲突计数、`Mercurial (hg)`）、高精度 Unicode 上下文窗口进度条、Prompt Cache 命中率、配额剩余与重置倒计时、正在执行的工具调用、后台 Subagents 以及 `task.md` 任务清单进度。

- **极致低延迟**：移除全部 Node.js 代码，编译后单文件体积仅 `~929 KB`；热缓存内部计算耗时仅 **`~3 ms`**（含 `sh -c` 进程拉起全链路仅 `~13–14 ms`，较 Node 版本提速 **5.3 倍**；冷启动实时探测 Git 仅 `~38 ms`）。

## 界面预览

```text
╭─ 🔧 调用工具 │ [自动] │ [Gemini 4 Argon · med] │ agy-hud git:(feat/rust-hud* ↑2) │ ⏱ 12m
├─ 上下文 ██░····· 28.4% (152k/1.0M) · ⚡75% 缓存 │ 配额(weekly) ▓···· 16% (4d 18h) │ ~$0.38 · 子代理 1/2
├─ ◐ run_command: Cargo test execution · ✓ edit_file ×2: render.rs · ✓ read_file ×5: SKILL.md │ 技能: hud
├─ ◐ CLI Docs Researcher [research] (1m 15s) │ ✓ Security Auditor [self] (1m)
╰─ ▸ Verify compile-time arch_guard boundaries (2/4 · 50%)
```

## 环境要求

- **Antigravity CLI (`agy`)** `1.4.5` 及以上版本
- **零 Rust / 零 Node.js 依赖**（通过一键安装脚本直接下载校验预编译二进制；仅从源码编译时才需 `rustc` 1.85+）

## 快速安装与启用

### 方式一：一行命令免编译极速安装（推荐）

自动识别系统与架构（macOS Apple Silicon / Intel、Linux x64 / ARM64 静态编译版），下载预编译包并校验 `SHA256SUMS`，注册 `/hud` 插件技能，将原生二进制部署至 `~/.gemini/antigravity-cli/agy-hud/bin/agy-hud` 并原子更新 `settings.json`（自动备份原配置）：

```bash
curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/install.sh | sh
```

如果希望保留官方内置状态栏（叠放在其下方），或覆盖已有的第三方 `statusLine`，可通过 `sh -s --` 传参：

```bash
curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/install.sh | sh -s -- --stack
```

启用后，在 `agy` 会话中可随时输入 `/hud` 用自然语言调整显示项（例如 `/hud 切换成简体中文并显示文件增删统计`），或使用内置 `/statusline` 开关状态栏。

### 方式二：手动下载预编译包（支持 macOS / Linux / Windows）

前往 [**GitHub Releases**](https://github.com/maxlee/agy-hud/releases/latest) 下载对应平台的发行包：

- `agy-hud-darwin-arm64.tar.gz`（macOS Apple Silicon）
- `agy-hud-darwin-x64.tar.gz`（macOS Intel）
- `agy-hud-linux-x64.tar.gz`（Linux x86_64 静态 `musl`，零 glibc 兼容问题）
- `agy-hud-linux-arm64.tar.gz`（Linux ARM64 静态 `musl`）
- `agy-hud-windows-x64.zip`（Windows x64 原生 `agy-hud.exe`）

### 方式三：从源码克隆编译安装

```bash
git clone https://github.com/maxlee/agy-hud.git
cd agy-hud
cargo build --release
agy plugin install .
./target/release/agy-hud install
```

如需彻底卸载并恢复官方默认状态栏，可随时执行一行卸载命令（自动移除 `settings.json` 中的 `statusLine` 以恢复 `agy` 官方默认配置、保留其余所有用户设置、清理安装备份文件、注销 `agy-hud` 插件并删除 `~/.gemini/antigravity-cli/agy-hud` 目录，零残留）：

```bash
curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/uninstall.sh | sh
```

（离线环境也可直接执行 `~/.gemini/antigravity-cli/agy-hud/bin/agy-hud uninstall --plugin`；若希望仅卸载但保留个性化 `config.json`，追加 `--keep-config` 即可。）

## 常用管理命令

```bash
./scripts/agy-hud doctor                                      # 诊断原生二进制、settings.json 与最近一次渲染耗时
./scripts/agy-hud preview --state showcase                    # 预览 5 种典型宽度、状态与中英文布局
./scripts/agy-hud schema                                      # 查看全部 38 个可配置项、类型及说明
./scripts/agy-hud set language=zh-Hans vcs.showFileStats=true # 修改配置（自动保存上一步快照）
./scripts/agy-hud set --undo                                  # 一键撤销上一次配置修改
./scripts/agy-hud report --note "问题描述"                     # 生成自动抹除敏感信息的诊断 JSON
```

单次会话临时关闭 HUD（不修改配置文件）：

```bash
AGY_HUD_DISABLE=1 agy
```

## 架构与安全防线

详见 [ARCHITECTURE.md](ARCHITECTURE.md)：

1. **编译期双 Crate 权限硬隔离（`THE LINE`）**：
   - `crates/hud-core` 开启 `#![forbid(unsafe_code)]`，仅依赖 `serde` 与 `serde_json`，经 `arch_guard.rs` 静态守卫测试强制保证**零 `std::fs`、`std::process`、`std::net`、`std::env` 调用**；
   - `crates/hud-cli` 负责有界读取 `stdin`（`<= 256 KB`）、`transcript.jsonl` 首行 `8 KB` + 尾部 `256 KB` 切片，并通过 `std::thread::scope` 并行拉起只读探针。
2. **防恶意仓库命令注入**：
   - Git 探针强制注入 `-c protocol.allow=never -c core.fsmonitor=false --no-optional-locks`，并自动扫描 `.git/config` 动态清空所有本地 `[filter "<name>"]` 驱动（`clean=` / `smudge=` / `process=` / `required=false`）；
   - Jujutsu (`jj`) 探针强制携带 `--ignore-working-copy --at-operation=@ --no-pager`，绝不后台改写工作区快照。
3. **分片无锁 VCS 缓存**：按工作区真实路径 FNV-1a 64-bit 哈希独立存储于 `<hudHome>/probe-cache/<hash>.json`，消除多终端并发读写竞争。
4. **终端转义序列防火墙（`terminal_filter`）**：仅放行标准 SGR 颜色码与本地 `file://` OSC 8 链接，彻底剥离 OSC 52 剪贴板读取、窗口标题劫持与双向字符（Bidi Trojan）控制码。

## 开源协议

基于 [MIT License](LICENSE) 开源发布。

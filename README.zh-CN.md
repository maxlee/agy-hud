<div align="center">

<img src="assets/logo.svg" alt="AGY HUD Logo" width="88" height="88" />

# AGY HUD (`agy-hud`)

**专为 Google Antigravity CLI (`agy`) 打造的超高速、零外部依赖、100% 纯 Rust 实时终端状态栏（HUD）插件**

[![CI](https://github.com/maxlee/agy-hud/actions/workflows/ci.yml/badge.svg)](https://github.com/maxlee/agy-hud/actions/workflows/ci.yml)
[![Release](https://github.com/maxlee/agy-hud/actions/workflows/release.yml/badge.svg)](https://github.com/maxlee/agy-hud/releases/latest)
[![Rust 2024](https://img.shields.io/badge/Rust-2024_Edition-orange.svg)](ARCHITECTURE.md)
[![Unsafe Forbidden](https://img.shields.io/badge/unsafe-forbidden-brightgreen.svg)](ARCHITECTURE.md)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

[**English**](README.md) · [**简体中文**](README.zh-CN.md) · [**快速安装**](#-安装指南) · [**核心特性**](#-核心特性) · [**命令与配置**](#️-命令行与个性化配置) · [**架构设计**](ARCHITECTURE.md) · [**版本发布**](https://github.com/maxlee/agy-hud/releases)

</div>

---

**AGY HUD** 在 **Antigravity CLI (`agy`)** 输入框正下方渲染实时、自适应深浅色主题的状态栏面板——一站式展示 Agent 运行状态、执行模式、当前模型与推理强度、剥离首字延迟（`TTFT`）的真实输出速率（`~tok/s`）、多版本控制状态（`git` / `jj` / `hg`）、上下文窗口进度条、Prompt Cache 命中率、配额剩余与重置倒计时、活跃工具调用、子智能体（Subagents）状态以及 `task.md` 任务清单进度。热缓存内部耗时仅 **~1–3 ms**，**零 Node.js 或任何运行时虚拟机依赖**。

> 设计灵感源自 [`jarrodwatts/claude-hud`](https://github.com/jarrodwatts/claude-hud)，基于安全 Rust（`#![forbid(unsafe_code)]`）面向 Antigravity CLI 插件与 `statusLine` 生态原生构建。

---

## 📸 界面预览

### 标准多行展开布局（`76–179` 列）

```text
╭─ 🔧 调用工具 │ [自动] │ [Gemini 4 Argon · med] Pro · ~118 tok/s │ agy-hud git:(feat/rust-hud* ↑2) │ ⏱ 12m
├─ 上下文 ██░····· 28.4% (152k/1.0M) · ⚡ 75% 缓存 │ ~$0.38 · 子代理 1/2 · 后台 1 · 产物 2 · 🛡 沙箱开启
├─ 配额(weekly) ▓···· 16% (4d 18h)
├─ ◐ run_command: Cargo test execution · ✓ edit_file ×2: render.rs · ✓ read_file ×5: SKILL.md
├─ 技能: hud, agy-customizations │ MCP: context7
├─ ◐ CLI Docs Researcher [research] (1m 15s) │ ◷ Waiter [self] (20s) │ ✓ Security Auditor [self] (1m)
╰─ ▸ Verify compile-time arch_guard boundaries (2/4 · 50%)
```

### 超宽屏单行空闲模式（`>= 180` 列）与窄分屏紧凑模式（`< 76` 列）

```text
# 超宽屏（>= 180 列，空闲状态）—— 自动折叠为清爽单行：
✓ 空闲 │ [自动] │ [Gemini 4 Argon · med] Pro · ~118 tok/s │ agy-hud git:(main) │ ⏱ 5m │ 上下文 ██░····· 28.4% (152k/1.0M) · ⚡ 75% 缓存 │ 配额(weekly) ▓···· 16% (4d 18h)

# 窄终端分屏（< 76 列，高上下文预警）—— 渐进式优雅降级，绝不折行错乱：
⚡ 思考中 │ [自动] │ [Argon · M] │ agy-hud git:(main*)
上下文 ███████░ 89.6% !200k │ 配额(weekly) 92% (1h 30m)
◐ run_command: Cargo test execution
```

### 状态栏模块拆解

| 模块区域 | 展示内容 | 常用配置开关 |
| :--- | :--- | :--- |
| **运行状态与模式** | 实时 `空闲` / `思考中` / `调用工具` / `等待确认` 状态，`[计划]` / `[自动]` / `[接受]` 模式（自动识别 `/plan` 与 `VIM` 模式徽章） | `display.showState`, `display.showMode` |
| **模型、档位与速率** | 模型简称、推理强度（`low`/`med`/`high`）、订阅档位（`Pro`/`Ultra`）以及剥离 `TTFT` 的真实生成速率 `~tok/s` | `display.showModel`, `display.showPlanTier`, `display.showSpeed` |
| **项目路径与 VCS** | 支持点击跳转的 `file://` OSC 8 目录链接 + `git`（含 Worktree、脏标记、领先/落后、文件增删）、`jj`（书签、`@change_id`、冲突计数）与 `hg` | `display.showProject`, `vcs.enabled`, `vcs.showFileStats` |
| **上下文与会话指标** | 1/8 精细块级进度条、`!200k` 阈值告警、Prompt `Cache` 命中率、估算开销、子代理/后台任务/产物计数、沙箱徽章 | `display.showContextBar`, `display.showCacheHitRate`, `display.showCost` |
| **配额进度与倒计时** | 按当前模型智能路由配额桶（`5h` / `weekly`），支持已用/剩余百分比进度条与重置倒计时 | `display.showQuota`, `display.quotaValue`, `quota.backgroundRefresh` |
| **实时活动明细行** | 同名工具调用折叠计数（`×N`）、已激活 `Skills` 与 `MCP` 服务（异常自动标红 `!`）、四态 `Subagents`（`◐`/`◷`/`✗`/`✓`）与 `task.md` 进度 | `display.showTools`, `display.showSkillsAndMcp`, `display.showSubagents`, `display.showTodos` |

---

## ✨ 核心特性

- **⚡ 原生 ~1–3 ms 极致性能**：LTO 裁剪后单二进制体积仅 `~930 KB`，无任何虚拟机冷启动开销；`std::thread::scope` 多线程并行采集，`transcript.jsonl` 采用有界 `O(1)` 首部（`32 KB`）+ 尾部（`512 KB`）定长切片读取。
- **🎯 混合毫秒级 `~tok/s` 测速引擎（`v1.4.0` 全新升级）**：借助 `agy` 每 `300 ms` 的状态栏心跳，精确剥离 `thinking`/Prefill 首字延迟（`TTFT`）与工具执行等待时间，结合两点斜率校准与按模型隔离的 Token-Weighted EWMA 平滑算法，彻底告别短工具回合导致的速率跳水。
- **🎨 深浅色主题自适应与中日韩宽度对齐**：绝不使用硬编码白色（`37m`/`97m`）或浅灰（`90m`）前景色，在 Dark 与 Solarized Light 浅色终端下均清晰可读；精确计算 CJK 双列字符、ZWJ 组合 Emoji 与 OSC 8 闭合标签，内置 `unicode`、`nerd`、`ascii` 三套图标模式。
- **🌿 原生多版本控制（`git` / `jj` / `hg` & CitC）**：自动识别共置 Jujutsu（`jj`）、嵌套 Git Worktree（超时自动降级至零子进程 `.git/HEAD` 解析）、Mercurial（`hg`）与 CitC 客户端工作区，配合按工作区 FNV-1a 哈希分片的 `O(1)` `mtime` 缓存。
- **🔄 内置 SHA-256 校验自更新**：每 6 小时后台静默检查新版本并在顶栏温和提示（`↑ vX.Y.Z · /agy-hud:hud-update`），一键校验升级并完整保留 `--stack` 与个性化配置。

---

## 📦 安装指南

### 环境要求

- **Antigravity CLI (`agy`)** `v1.4.5+`（已完整对齐 `v1.5.4+` 最新内部接口）
- **零 Rust / Node.js / npm 依赖**（使用预编译包或一键脚本安装时无需编译环境；仅从源码构建时需 `rustc 1.85+`）

### 方式一：Antigravity Marketplace 或 `agy plugin install`（推荐）

1. **Antigravity 桌面端**：打开 **Customizations → Marketplace**，搜索 **AGY HUD** 点击 **Add**，然后点击插件卡片上的 **"Set up the HUD status line in Antigravity CLI"** 快捷引导胶囊即可启用。
2. **或通过 `agy` 命令行安装**：
   ```bash
   agy plugin install agy-hud
   # 或直接从 GitHub 仓库安装最新版：
   agy plugin install https://github.com/maxlee/agy-hud
   ```
   启动 `agy` 后输入 `/agy-hud:hud` 即可自动完成 `settings.json` 挂载*（若安装前 `agy` 已处于运行状态，请先执行 `/skills reload` 刷新技能菜单）*。

### 方式二：一行脚本 SHA-256 校验安装（macOS & Linux）

自动识别平台架构（`darwin-arm64`、`darwin-x64`、`linux-x64`、`linux-arm64`），下载发行包并比对 `SHA256SUMS` 校验和，注册 `/agy-hud:hud` 与 `/agy-hud:hud-update` 技能，部署原生二进制至 `~/.gemini/antigravity-cli/agy-hud/bin/agy-hud` 并原子更新 `settings.json`：

```bash
curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/install.sh | sh
```

如果希望保留官方默认状态栏（将 `agy-hud` 叠放在其下方）：

```bash
curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/install.sh | sh -s -- --stack
```

### 方式三：手动下载预编译发行包（macOS / Linux / Windows）

前往 [**GitHub Releases**](https://github.com/maxlee/agy-hud/releases/latest) 下载对应平台的预编译包及 `SHA256SUMS`：

| 操作系统与架构 | 发行包文件名 | Rust Target Triple | 说明 |
| :--- | :--- | :--- | :--- |
| **macOS (Apple Silicon)** | `agy-hud-darwin-arm64.tar.gz` | `aarch64-apple-darwin` | 原生支持 M1 / M2 / M3 / M4 |
| **macOS (Intel)** | `agy-hud-darwin-x64.tar.gz` | `x86_64-apple-darwin` | 原生 x86_64 |
| **Linux (x64)** | `agy-hud-linux-x64.tar.gz` | `x86_64-unknown-linux-musl` | 纯静态 `musl` 链接，零 `glibc` 依赖 |
| **Linux (ARM64)** | `agy-hud-linux-arm64.tar.gz` | `aarch64-unknown-linux-musl` | 纯静态 `musl` 链接 |
| **Windows (x64)** | `agy-hud-windows-x64.zip` | `x86_64-pc-windows-msvc` | 原生 `agy-hud.exe`（兼容含空格路径防引号 Bug） |

### 方式四：从源码编译安装

```bash
git clone https://github.com/maxlee/agy-hud.git
cd agy-hud
cargo build --release
agy plugin install .
./target/release/agy-hud install
```

---

## 🛠️ 命令行与个性化配置

### `agy` 会话内交互斜杠命令

| 斜杠命令 | 功能说明 |
| :--- | :--- |
| `/agy-hud:hud` | 使用自然语言交互式安装、诊断、预览或调整布局、界面语言与各模块开关 |
| `/agy-hud:hud-update` | 检查 GitHub 最新发行版并一键完成 SHA-256 校验的二进制与插件同步升级 |
| `/statusline` | `agy` 官方内置命令，用于临时开启或隐藏底部状态栏 |

### 原生 CLI 子命令一览

直接调用 `~/.gemini/antigravity-cli/agy-hud/bin/agy-hud`（或在源码目录运行 `./scripts/agy-hud`）：

| 子命令 | 用途说明 |
| :--- | :--- |
| `install [--stack] [--replace]` | 部署原生二进制并原子写入 `settings.json`（自动创建带时间戳的备份） |
| `uninstall [--plugin] [--keep-config]` | 移除 `statusLine`、清理备份与数据目录，无损恢复 `agy` 官方默认状态栏 |
| `doctor [--pretty]` | 检查运行引擎、插件启用状态、`statusLine` 挂载、配置合法性及终端图标对齐情况 |
| `preview [--state <name>] [--last] [--set k=v]` | 预览 `showcase`（5 大场景）、`idle`、`tool_use`、`confirm`、`high-context` 或最近一次真实快照 |
| `schema [关键词]` | 检索全部 **43** 个受强类型校验的配置项、类型、默认值与说明 |
| `set <key>=<value> ...` / `set --undo` | 原子修改 `~/.gemini/antigravity-cli/agy-hud/config.json` 或一键回滚上一次修改 |
| `update [--check] [--force]` | 检查或执行经过 SHA-256 校验的自升级 |
| `quota refresh` | 在隔离临时目录执行一次 `agy -p /usage` 官方配额缓存校准 |
| `report [--note "..."]` | 导出隐私脱敏后的诊断 JSON（自动抹除本地路径、会话 ID、标题、邮箱与 VCS 客户端名） |

### 常用配置示例

```bash
# 一次预览全部 5 种终端宽度、状态与中英文布局效果
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud preview --state showcase

# 切换为简体中文界面并开启 VCS 文件增删统计
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set language=zh-Hans vcs.showFileStats=true

# 切换图标字符集：unicode（默认）| nerd（Nerd Fonts）| ascii（纯 7-bit 安全字符）
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set display.glyphMode=nerd

# 独立隐藏目录名但保留 Git 分支显示，或开启会话标题显示
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set display.showProject=false vcs.enabled=true
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set display.showTitle=true

# 一键撤销上一次配置变更
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud set --undo
```

### 环境变量

| 变量名 | 说明 |
| :--- | :--- |
| `AGY_HUD_DISABLE=1` | 仅在当前终端会话临时静默关闭 HUD，不改动任何配置文件（`AGY_HUD_DISABLE=1 agy`） |
| `AGY_CLI_DIR` | 自定义 Antigravity CLI 配置目录（默认：`~/.gemini/antigravity-cli`） |
| `AGY_HUD_HOME` | 自定义 HUD 配置与缓存目录（默认：`~/.gemini/antigravity-cli/agy-hud`） |

---

## 🧹 零残留干净卸载

随时可通过一条命令完整卸载——自动移除 `settings.json` 中的 `statusLine` 字段（恢复 `agy` 官方默认状态栏并完整保留你的其余所有配置）、清理备份、注销插件并删除 `~/.gemini/antigravity-cli/agy-hud`：

```bash
curl -fsSL https://raw.githubusercontent.com/maxlee/agy-hud/main/uninstall.sh | sh
```

离线环境也可直接执行：

```bash
~/.gemini/antigravity-cli/agy-hud/bin/agy-hud uninstall --plugin
```

*（如希望卸载但保留个性化 `config.json` 供日后重装使用，追加 `--keep-config` 即可。）*

---

## 🔒 架构与安全防线（`THE LINE`）

`agy-hud` 通过双 Crate 工作区在**编译期**实施严格的权限隔离（由 [`arch_guard.rs`](crates/hud-core/tests/arch_guard.rs) 每次测试静态审计）。完整设计请参阅 [**ARCHITECTURE.md**](ARCHITECTURE.md)：

```text
agy-hud/
├── crates/
│   ├── hud-core/   # 纯确定性解析与排版内核（#![forbid(unsafe_code)]，零系统 I/O 能力）
│   └── hud-cli/    # 极薄系统外壳：有界文件读取、只读加固 VCS 探针、原子缓存读写
├── skills/         # Antigravity 斜杠命令技能定义（/agy-hud:hud 与 /agy-hud:hud-update）
├── scripts/        # POSIX 多平台二进制路由与 Marketplace SHA-256 同步脚本
└── plugin.json     # 符合 Antigravity v1 严格规范的插件清单
```

1. **纯逻辑内核零 I/O（`hud-core`）**：仅依赖 `serde` 与 `serde_json`（全传递依赖仅 11 个包），静态禁止出现任何 `std::fs`、`std::process`、`std::net`、`std::env` 或 `std::os` 调用。
2. **防恶意仓库命令注入探针（`hud-cli`）**：
   - **Git**：强制注入 `-c protocol.allow=never`、`-c core.fsmonitor=false`、`--no-optional-locks`、`GIT_NO_LAZY_FETCH=1`，并在内存中扫描 `.git/config` 动态清空所有本地 `[filter "<name>"]` 驱动（`clean=` / `smudge=` / `process=` / `required=false`）；
   - **Jujutsu (`jj`) & Mercurial (`hg`)**：强制携带 `--ignore-working-copy --at-operation=@ --no-pager` 与 `HGPLAIN=1 --config extensions.fsmonitor=!`，所有子进程均受 `1200 ms` 硬超时保护。
3. **终端转义序列防火墙（`terminal_filter`）**：每一行输出在写入 `stdout` 前均经过转义过滤，彻底拦截 OSC 52 剪贴板窃取、窗口标题篡改、光标控制序列以及 Unicode 双向字符攻击（Bidi Trojan），仅放行标准 SGR 样式与本地 `file://` OSC 8 链接。

---

## 🧪 本地开发与校验

```bash
cargo fmt --all -- --check                             # 代码格式检查
cargo clippy --workspace --all-targets -- -D warnings  # 零告警 Clippy 静态检查
cargo test --workspace                                 # 运行单元、端到端集成、Git 注入金丝雀与架构守卫测试
cargo build --release                                  # 编译 LTO 裁剪版原生二进制
agy plugin validate --strict .                         # 执行 Antigravity 官方插件严格规范校验
```

---

## 📄 开源协议

基于 [**MIT License**](LICENSE) 协议开源发布。Copyright © Max.

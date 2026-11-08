# AGY HUD (`agy-hud`) v1.0.0 — 100% 纯 Rust 极速版指南

参考 [`jarrodwatts/claude-hud`](https://github.com/jarrodwatts/claude-hud) 的核心设计理念，采用 **100% 纯 Rust 原生单内核架构** 为 **Google Antigravity CLI (`agy`)** 打造的实时终端状态栏插件（Heads-Up Display）。

在 `agy` 输入框正下方实时展示：Agent 运行状态、执行模式（`PLAN` / `AUTO` / `ACCEPT` 与 Vim 模式）、当前模型与推理强度（Effort）、项目路径与多版本控制状态（原生支持 `Git` 含 Worktree、`Jujutsu (jj)` 书签/变更 ID/冲突计数、`Mercurial (hg)`）、高精度 Unicode 上下文窗口进度条、Prompt Cache 命中率、配额剩余与重置倒计时、正在执行的工具调用、后台 Subagents 以及 `task.md` 任务清单进度。

- **极致低延迟**：移除全部 Node.js 代码，编译后单文件体积仅 `~929 KB`；热缓存内部计算耗时仅 **`~3 ms`**（含 `sh -c` 进程拉起全链路仅 `~13–14 ms`，较 Node 版本提速 **5.3 倍**；冷启动实时探测 Git 仅 `~38 ms`）。

## 界面预览

```text
╭─ 🔧 调用工具 │ [自动] │ [Gemini 4 Argon · med] │ agy-hud git:(feat/rust-hud* ↑2) │ ⏱ 12m
├─ 上下文 ██░····· 28.4% (152k/1.0M) · ⚡75% 缓存 │ 配额(weekly) ▓···· 16% (4d 18h) │ ~\$0.38 · 子代理 1/2
├─ ◐ run_command: Cargo test execution · ✓ edit_file ×2: render.rs · ✓ read_file ×5: SKILL.md │ 技能: hud
├─ ◐ CLI Docs Researcher [research] (1m 15s) │ ✓ Security Auditor [self] (1m)
╰─ ▸ Verify compile-time arch_guard boundaries (2/4 · 50%)
```

## 环境要求

- **Antigravity CLI (`agy`)** `1.4.5` 及以上版本
- **零 Node.js 依赖**（仓库已内置 Release 原生二进制 `scripts/bin/agy-hud`；如需从源码重编需 `rustc` / `cargo`）

## 快速安装与启用

### 方式一：安装为 Antigravity 插件 + 部署原生二进制（推荐）

```bash
# 1. 严格校验并安装插件（同时激活 `/hud` 技能）
agy plugin validate --strict /Users/max/.gemini/antigravity/scratch/agy-hud
agy plugin install /Users/max/.gemini/antigravity/scratch/agy-hud

# 2. 部署原生二进制至 ~/.gemini/antigravity-cli/agy-hud/bin/agy-hud 并原子更新 settings.json（自动备份原配置）
/Users/max/.gemini/antigravity/scratch/agy-hud/scripts/bin/agy-hud install
```

启用后，在 `agy` 会话中可随时输入 `/hud` 用自然语言调整显示项（例如 `/hud 切换成简体中文并显示文件增删统计`），或使用内置 `/statusline` 开关状态栏。

### 方式二：在 `agy` 交互界面中直接绑定

```text
/statusline /Users/max/.gemini/antigravity/scratch/agy-hud/scripts/bin/agy-hud
```

如需恢复默认状态栏，输入 `/statusline delete` 或运行 `scripts/bin/agy-hud uninstall`。

## 常用管理命令

```bash
scripts/bin/agy-hud doctor                                      # 诊断原生二进制、settings.json 与最近一次渲染耗时
scripts/bin/agy-hud preview --state showcase                    # 预览 5 种典型宽度、状态与中英文布局
scripts/bin/agy-hud schema                                      # 查看全部 38 个可配置项、类型及说明
scripts/bin/agy-hud set language=zh-Hans vcs.showFileStats=true # 修改配置（自动保存上一步快照）
scripts/bin/agy-hud set --undo                                  # 一键撤销上一次配置修改
scripts/bin/agy-hud report --note "问题描述"                     # 生成自动抹除敏感信息的诊断 JSON
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

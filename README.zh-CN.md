# AGY HUD (`agy-hud`) — 简体中文指南

参考 [`jarrodwatts/claude-hud`](https://github.com/jarrodwatts/claude-hud) 的核心架构思想，专为 **Google Antigravity CLI (`agy`)** 打造的实时终端状态栏插件（Heads-Up Display）。

在 `agy` 输入框正下方实时展示：Agent 运行状态、执行模式（`PLAN` / `AUTO` / `ACCEPT` 与 Vim 模式）、当前模型与推理强度（Effort）、项目路径与多版本控制状态（原生支持 `Git` 含 Worktree、`Jujutsu (jj)` 书签/变更 ID/冲突计数、`Mercurial (hg)`）、高精度 Unicode 上下文窗口进度条、Prompt Cache 命中率、配额剩余与重置倒计时、正在执行的工具调用、后台 Subagents 以及 `task.md` 任务清单进度。

## 界面预览

```text
╭─ 🔧 调用工具 │ [自动] │ [Gemini 4 Argon · med] │ agy-hud git:(feat/agy-hud* ↑2) │ ⏱ 12m
├─ 上下文 ███░········ 28.4% (152k/1.0M) · ⚡75% 缓存 │ 配额(weekly) █······ 16% (4d 18h) │ ~\$0.38 · 子代理 1/2
├─ ◐ run_command: Unit test execution · ✓ edit_file ×2: render.ts · ✓ read_file ×5 │ 技能: hud
├─ ◐ CLI Docs Researcher [research] (1m 15s) │ ✓ Security Auditor [self] (1m)
╰─ ▸ Verify node --permission escape matrix (2/4 · 50%)
```

## 环境要求

- **Antigravity CLI (`agy`)** `1.4.5` 及以上版本
- **Node.js** `22.18.0` 及以上版本（零第三方 npm 依赖，开箱即用）

## 快速安装与启用

### 方式一：安装为 Antigravity 插件 + 自动配置 `statusLine`（推荐）

```bash
# 1. 严格校验并安装插件（同时激活 `/hud` 技能）
agy plugin validate --strict /Users/max/.gemini/antigravity/scratch/agy-hud
agy plugin install /Users/max/.gemini/antigravity/scratch/agy-hud

# 2. 安全写入 ~/.gemini/antigravity-cli/settings.json（自动备份原配置）
node /Users/max/.gemini/antigravity/scratch/agy-hud/scripts/hud.mjs install
```

启用后，在 `agy` 会话中可随时输入 `/hud` 用自然语言调整显示项（例如 `/hud 切换成简体中文并显示文件增删统计`），或使用内置 `/statusline` 开关状态栏。

### 方式二：在 `agy` 交互界面中直接绑定

启动 `agy` 后直接输入：

```text
/statusline node /Users/max/.gemini/antigravity/scratch/agy-hud/scripts/hud.mjs
```

如需恢复默认状态栏，输入 `/statusline delete` 或运行 `node scripts/hud.mjs uninstall`。

## 常用管理命令

```bash
node scripts/hud.mjs doctor                                      # 诊断运行环境、settings.json 与最近一次渲染耗时
node scripts/hud.mjs preview --state showcase                    # 预览 5 种典型宽度、状态与中英文布局
node scripts/hud.mjs schema                                      # 查看所有可配置项、类型及说明
node scripts/hud.mjs set language=zh-Hans vcs.showFileStats=true # 修改配置（自动保存上一步快照）
node scripts/hud.mjs set --undo                                  # 一键撤销上一次配置修改
node scripts/hud.mjs report --note "问题描述"                     # 生成自动脱敏的诊断 JSON
```

单次会话临时关闭 HUD（不修改配置文件）：

```bash
AGY_HUD_DISABLE=1 agy
```

## 架构与安全设计（对标 `claude-hud`）

详见 [ARCHITECTURE.md](ARCHITECTURE.md)：

1. **权限层与逻辑层硬隔离（THE LINE）**：`scripts/hud.mjs` 与 `scripts/collector.mjs` 持有外部能力；`src/*.ts` 全部运行在 `node --permission --disallow-code-generation-from-strings --import=./scripts/fence.mjs` 沙箱笼中。
2. **零外部命令与零网络逃逸**：`fence.mjs` 通过 `module.registerHooks` 拦截 `child_process`、`net`、`http` 等模块，并封锁 `fetch`、`WebSocket`、`process.binding` 与 `net.Socket`。
3. **终端转义序列防火墙（`terminalFilter`）**：严格过滤所有非 SGR 颜色控制码与非本地 `file:` 的 OSC 8 链接，杜绝剪贴板窃取（OSC 52）与双向字符欺骗（Bidi Trojan）。
4. **O(1) 尾部增量日志解析与多级热缓存**：
   - `stripped/manifest.json` 基于源文件 `(mtimeMs, size)` 跳过重复 TypeScript 类型剥离与哈希计算；
   - `probe-cache.json` 提供基于仓库元数据文件 `mtime` 指纹 + `vcs.cacheTtlMs`（默认 `1500ms`）的短周期缓存，并在缓存未命中时与受限子进程启动**并行预热**；
   - 仅读取 `transcript.jsonl` 首行与末尾 `256 KB` 窗口，单次热缓存状态栏渲染稳定在 `~70ms`。

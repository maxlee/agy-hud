import {
  BOLD,
  COLORS,
  DIM,
  ITALIC,
  RESET,
  fileLink,
  paint,
  renderBar,
  sanitizeText,
  textWidth,
  truncateAnsi,
} from './ansi.ts';
import type { HudConfig } from './config.ts';
import { getLabels, type Labels } from './i18n.ts';
import {
  extractContextMetrics,
  extractQuotaBuckets,
  formatCompactTokens,
  formatDurationSeconds,
  formatModelName,
  formatWorkspacePath,
} from './stdin.ts';
import type {
  AgyStdinData,
  MemoryProbeInfo,
  TranscriptSummary,
  VcsProbeInfo,
} from './types.ts';

export interface RenderInput {
  stdin: AgyStdinData | null;
  config: HudConfig;
  transcript: TranscriptSummary;
  vcs: VcsProbeInfo | null;
  memory: MemoryProbeInfo | null;
  columns: number | null;
  now: number;
}

const SEP = `${COLORS.gray} │ ${RESET}`;
const DOT = `${COLORS.gray} · ${RESET}`;

function renderStateBadge(stdin: AgyStdinData | null, labels: Labels): string {
  if (stdin?.tool_confirmation_pending) {
    return paint(`⚠ ${labels.state.confirm}`, COLORS.brightYellow, BOLD);
  }
  const state = (stdin?.agent_state || 'idle').toLowerCase();
  switch (state) {
    case 'idle':
      return paint(`● ${labels.state.idle}`, COLORS.brightGreen, BOLD);
    case 'thinking':
      return paint(`◆ ${labels.state.thinking}`, COLORS.brightYellow, BOLD);
    case 'working':
      return paint(`⚙ ${labels.state.working}`, COLORS.brightCyan, BOLD);
    case 'tool_use':
      return paint(`🔧 ${labels.state.tool_use}`, COLORS.brightMagenta, BOLD);
    case 'reviewing':
      return paint(`👁 ${labels.state.reviewing}`, COLORS.brightBlue, BOLD);
    case 'authenticating':
      return paint(`🔑 ${labels.state.authenticating}`, COLORS.yellow, BOLD);
    case 'initializing':
      return paint(`◌ ${labels.state.initializing}`, COLORS.cyan, BOLD);
    case 'error':
      return paint(`✖ ${labels.state.error}`, COLORS.brightRed, BOLD);
    default:
      return paint(`● ${sanitizeText(state).toUpperCase() || labels.state.idle}`, COLORS.white, BOLD);
  }
}

function renderModeBadges(stdin: AgyStdinData | null, labels: Labels): string {
  const parts: string[] = [];
  const cycle = sanitizeText(stdin?.cycle_mode || '').toLowerCase();
  if (cycle === 'plan') {
    parts.push(paint(`[${labels.mode.plan}]`, COLORS.brightCyan, BOLD));
  } else if (cycle === 'auto') {
    parts.push(paint(`[${labels.mode.auto}]`, COLORS.brightGreen, BOLD));
  } else if (cycle === 'accept-edits') {
    parts.push(paint(`[${labels.mode.acceptEdits}]`, COLORS.brightYellow));
  }

  const vimMode = sanitizeText(stdin?.vim?.mode || '');
  if (vimMode) {
    const vimColor =
      vimMode === 'INSERT'
        ? COLORS.brightGreen
        : vimMode.startsWith('VISUAL')
          ? COLORS.brightMagenta
          : COLORS.brightBlue;
    parts.push(paint(`[${vimMode}]`, vimColor, BOLD));
  }

  const customAgent = sanitizeText(stdin?.agent?.name || '');
  if (customAgent) {
    parts.push(paint(`@${customAgent}`, COLORS.cyan));
  }

  return parts.join(' ');
}

function renderVcsSegment(vcs: VcsProbeInfo | null, config: HudConfig): string {
  if (!config.vcs.enabled || !vcs || !vcs.branch) return '';
  const prefix = vcs.type === 'git' ? 'git' : vcs.type;
  const dirtyMark = config.vcs.showDirty && vcs.dirty ? paint('*', COLORS.brightYellow, BOLD) : '';
  const branchColor = vcs.dirty && config.vcs.showDirty ? COLORS.brightRed : COLORS.brightBlue;

  const extras: string[] = [];
  if (config.vcs.showAheadBehind) {
    const ab = `${vcs.ahead > 0 ? `↑${vcs.ahead}` : ''}${vcs.behind > 0 ? `↓${vcs.behind}` : ''}`;
    if (ab) extras.push(paint(ab, COLORS.cyan));
  }
  if (vcs.conflicts > 0) {
    extras.push(paint(`!${vcs.conflicts}`, COLORS.brightRed, BOLD));
  }
  if (config.vcs.showFileStats) {
    const stats: string[] = [];
    if (vcs.added > 0) stats.push(paint(`+${vcs.added}`, COLORS.green));
    if (vcs.modified > 0) stats.push(paint(`~${vcs.modified}`, COLORS.yellow));
    if (vcs.deleted > 0) stats.push(paint(`-${vcs.deleted}`, COLORS.red));
    if (stats.length > 0) extras.push(stats.join(''));
  }

  const extraText = extras.length > 0 ? ` ${extras.join(' ')}` : '';
  return `${ paint(`${prefix}:(`, COLORS.gray)}${paint(vcs.branch, branchColor)}${dirtyMark}${extraText}${paint(')', COLORS.gray)}`;
}

function renderContextSegment(
  stdin: AgyStdinData | null,
  config: HudConfig,
  labels: Labels,
  effectiveWidth: number | null,
): string {
  const metrics = extractContextMetrics(stdin);
  const pctStr = `${metrics.usedPercent.toFixed(1)}%`;
  const remStr = `${metrics.remainingPercent.toFixed(1)}% ${labels.metrics.remaining}`;
  const tokStr =
    metrics.usedTokens !== null && metrics.windowSize !== null
      ? `${formatCompactTokens(metrics.usedTokens)}/${formatCompactTokens(metrics.windowSize)}`
      : metrics.usedTokens !== null
        ? formatCompactTokens(metrics.usedTokens)
        : null;

  let valueText = pctStr;
  switch (config.display.contextValue) {
    case 'tokens':
      valueText = tokStr ?? pctStr;
      break;
    case 'remaining':
      valueText = remStr;
      break;
    case 'both':
      valueText = tokStr ? `${pctStr} ${paint(`(${tokStr})`, COLORS.gray)}` : pctStr;
      break;
    case 'percent':
    default:
      valueText = pctStr;
      break;
  }

  const barLen =
    config.lineLayout === 'adaptive' && effectiveWidth !== null && effectiveWidth < 120
      ? Math.min(config.display.barLength, 8)
      : config.display.barLength;

  const bar = config.display.showContextBar
    ? `${renderBar(
        metrics.usedPercent,
        barLen,
        config.display.contextWarningThreshold,
        config.display.contextCriticalThreshold,
        COLORS.brightGreen,
      )} `
    : '';

  const numColor =
    metrics.usedPercent >= config.display.contextCriticalThreshold
      ? COLORS.brightRed
      : metrics.usedPercent >= config.display.contextWarningThreshold
        ? COLORS.brightYellow
        : COLORS.brightWhite;

  const over200k = metrics.exceeds200k ? ` ${paint('200k+', COLORS.brightYellow)}` : '';
  const cachePart =
    config.display.showCacheHitRate && metrics.cacheHitPercent !== null
      ? `${DOT}${paint(`⚡${metrics.cacheHitPercent}% ${labels.metrics.cache}`, COLORS.cyan)}`
      : '';

  return `${paint(labels.metrics.context, COLORS.gray)} ${bar}${paint(valueText, numColor, BOLD)}${over200k}${cachePart}`;
}

function renderQuotaSegment(
  stdin: AgyStdinData | null,
  config: HudConfig,
  labels: Labels,
  effectiveWidth: number | null,
  maxBuckets = 2,
): string {
  if (!config.display.showQuota) return '';
  const buckets = extractQuotaBuckets(stdin).slice(0, maxBuckets);
  if (buckets.length === 0) return '';

  const miniLen =
    config.lineLayout === 'adaptive' && effectiveWidth !== null && effectiveWidth < 120
      ? 5
      : Math.max(5, Math.floor(config.display.barLength * 0.65));

  const rendered = buckets.map((b) => {
    const label = b.label ? `${labels.metrics.quota}(${b.label})` : labels.metrics.quota;
    if (b.usedPercent !== null && b.remainingPercent !== null) {
      const displayPct =
        config.display.quotaValue === 'remaining'
          ? `${b.remainingPercent.toFixed(0)}% ${labels.metrics.remaining}`
          : `${b.usedPercent.toFixed(0)}%`;
      const miniBar = config.display.quotaBarEnabled
        ? `${renderBar(b.usedPercent, miniLen, 70, 90, COLORS.brightCyan)} `
        : '';
      const reset =
        config.display.showQuotaReset && b.resetInSeconds !== null
          ? ` ${paint(`(${formatDurationSeconds(b.resetInSeconds)})`, COLORS.gray)}`
          : '';
      return `${paint(label, COLORS.gray)} ${miniBar}${paint(displayPct, COLORS.brightWhite, BOLD)}${reset}`;
    }
    return `${paint(label, COLORS.gray)} ${paint(String(b.remainingAmount ?? ''), COLORS.brightWhite, BOLD)}`;
  });

  return rendered.join(DOT);
}

function renderCountsAndMeta(
  stdin: AgyStdinData | null,
  memory: MemoryProbeInfo | null,
  config: HudConfig,
  labels: Labels,
  compact = false,
): string {
  const items: string[] = [];

  if (config.display.showCost && typeof stdin?.cost?.total_usd === 'number') {
    const prefix = stdin.cost.estimated ? '~' : '';
    items.push(paint(`${prefix}$${stdin.cost.total_usd.toFixed(2)}`, COLORS.yellow));
  }

  if (config.display.showCounts) {
    const runningAgents = Array.isArray(stdin?.subagents)
      ? stdin!.subagents!.filter((s) => s.status === 'running').length
      : 0;
    const totalAgents = Array.isArray(stdin?.subagents) ? stdin!.subagents!.length : 0;
    if (totalAgents > 0) {
      const agentLabel =
        runningAgents > 0 ? `${runningAgents}/${totalAgents}` : String(totalAgents);
      items.push(
        `${paint(labels.metrics.subagents, COLORS.gray)} ${paint(agentLabel, runningAgents > 0 ? COLORS.brightCyan : COLORS.brightWhite, BOLD)}`,
      );
    }
    if (typeof stdin?.task_count === 'number' && stdin.task_count > 0) {
      items.push(
        `${paint(labels.metrics.tasks, COLORS.gray)} ${paint(String(stdin.task_count), COLORS.brightCyan, BOLD)}`,
      );
    }
    if (!compact && typeof stdin?.artifact_count === 'number' && stdin.artifact_count > 0) {
      items.push(
        `${paint(labels.metrics.artifacts, COLORS.gray)} ${paint(String(stdin.artifact_count), COLORS.brightWhite, BOLD)}`,
      );
    }
    if (typeof stdin?.pending_input_count === 'number' && stdin.pending_input_count > 0) {
      items.push(
        paint(`+${stdin.pending_input_count} ${labels.metrics.queued}`, COLORS.brightYellow, BOLD),
      );
    }
  }

  if (config.display.showMemory && memory) {
    const memColor =
      memory.usedPercent >= 90
        ? COLORS.brightRed
        : memory.usedPercent >= 75
          ? COLORS.brightYellow
          : COLORS.brightWhite;
    items.push(`${paint(labels.metrics.memory, COLORS.gray)} ${paint(`${memory.usedPercent}%`, memColor)}`);
  }

  if (config.display.showSandbox && stdin?.sandbox && typeof stdin.sandbox.enabled === 'boolean') {
    if (stdin.sandbox.enabled) {
      items.push(paint(`🛡 ${labels.metrics.sandboxOn}`, COLORS.brightGreen, BOLD));
    } else if (!compact) {
      items.push(paint(labels.metrics.sandboxOff, COLORS.gray));
    }
  }

  return items.join(DOT);
}

function renderToolsActivityLine(
  transcript: TranscriptSummary,
  config: HudConfig,
  labels: Labels,
): string | null {
  const parts: string[] = [];

  if (config.display.showTools && transcript.tools.length > 0) {
    const visible = transcript.tools.slice(0, config.display.toolsMaxVisible);
    const formatted = visible.map((t) => {
      const icon =
        t.status === 'running'
          ? paint('◐', COLORS.brightCyan, BOLD)
          : t.status === 'error'
            ? paint('✗', COLORS.brightRed, BOLD)
            : paint('✓', COLORS.brightGreen);
      const nameColor =
        t.status === 'running'
          ? COLORS.brightWhite
          : t.status === 'error'
            ? COLORS.brightRed
            : COLORS.white;
      const countPart = t.count > 1 ? paint(` ×${t.count}`, COLORS.cyan) : '';
      const targetPart = t.target ? paint(`: ${t.target}`, COLORS.gray) : '';
      return `${icon} ${paint(t.name, nameColor)}${countPart}${targetPart}`;
    });
    parts.push(formatted.join(DOT));
  }

  if (config.display.showSkillsAndMcp) {
    if (transcript.skills.length > 0) {
      const shown = transcript.skills.slice(-3).join(', ');
      parts.push(`${paint(`${labels.metrics.skills}:`, COLORS.gray)} ${paint(shown, COLORS.magenta)}`);
    }
    if (transcript.mcpServers.length > 0) {
      const mcpItems = transcript.mcpServers.slice(-3).map((srv) => {
        const hasErr = transcript.mcpErrors.includes(srv);
        return hasErr ? paint(`${srv}!`, COLORS.brightRed, BOLD) : paint(srv, COLORS.cyan);
      });
      parts.push(`${paint(`${labels.metrics.mcp}:`, COLORS.gray)} ${mcpItems.join(', ')}`);
    }
  }

  return parts.length > 0 ? parts.join(SEP) : null;
}

function renderSubagentsLine(
  transcript: TranscriptSummary,
  config: HudConfig,
  now: number,
): string | null {
  if (!config.display.showSubagents || transcript.subagents.length === 0) return null;

  const formatted = transcript.subagents.slice(0, 4).map((ag) => {
    const running = ag.status === 'running';
    const icon = running ? paint('◐', COLORS.brightCyan, BOLD) : paint('✓', COLORS.brightGreen);
    const title = ag.role ? `${ag.role} ${paint(`[${ag.name}]`, COLORS.gray)}` : ag.name;
    let elapsed = '';
    if (ag.startedAt) {
      const end = ag.endedAt ?? now;
      if (end >= ag.startedAt) {
        elapsed = ` ${paint(`(${formatDurationSeconds((end - ag.startedAt) / 1000)})`, COLORS.gray)}`;
      }
    }
    return `${icon} ${running ? paint(title, COLORS.brightWhite) : title}${elapsed}`;
  });

  return formatted.join(SEP);
}

function renderTodosLine(
  transcript: TranscriptSummary,
  config: HudConfig,
  labels: Labels,
): string | null {
  if (!config.display.showTodos || transcript.todos.length === 0) return null;

  const total = transcript.todos.length;
  const completed = transcript.todos.filter((t) => t.status === 'completed').length;
  const active =
    transcript.todos.find((t) => t.status === 'in_progress') ??
    transcript.todos.find((t) => t.status === 'pending');

  if (!active) {
    return `${paint('✓', COLORS.brightGreen, BOLD)} ${paint(`${labels.metrics.todos}: ${completed}/${total} (100%)`, COLORS.brightGreen)}`;
  }

  const pct = Math.round((completed / total) * 100);
  const icon = active.status === 'in_progress' ? paint('▸', COLORS.brightCyan, BOLD) : paint('◦', COLORS.gray);
  return `${icon} ${paint(active.text, COLORS.brightWhite)} ${paint(`(${completed}/${total} · ${pct}%)`, COLORS.cyan)}`;
}

function joinWithRail(lines: string[], useBoxRail: boolean): string[] {
  if (!useBoxRail || lines.length <= 1) return lines;
  return lines.map((line, idx) => {
    if (idx === 0) return `${COLORS.gray}╭─${RESET} ${line}`;
    if (idx === lines.length - 1) return `${COLORS.gray}╰─${RESET} ${line}`;
    return `${COLORS.gray}├─${RESET} ${line}`;
  });
}

export function renderHudLines(input: RenderInput): string[] {
  const { stdin, config, transcript, vcs, memory, columns, now } = input;
  const labels = getLabels(config.language);

  const liveWidth = columns ?? stdin?.terminal_width ?? null;
  const effectiveWidth =
    config.maxWidth !== null
      ? liveWidth !== null
        ? Math.min(config.maxWidth, liveWidth)
        : config.maxWidth
      : liveWidth;

  const layoutMode =
    config.lineLayout === 'adaptive'
      ? effectiveWidth !== null && effectiveWidth < 76
        ? 'compact'
        : 'expanded'
      : config.lineLayout;

  // Build Header Segments
  const headerParts: string[] = [];
  if (config.display.showState) {
    headerParts.push(renderStateBadge(stdin, labels));
  }
  if (config.display.showMode) {
    const modeBadges = renderModeBadges(stdin, labels);
    if (modeBadges) headerParts.push(modeBadges);
  }
  if (config.display.showModel) {
    const modelStr = formatModelName(
      stdin?.model,
      layoutMode === 'compact' ? 'short' : config.display.modelFormat,
      config.display.effortFormat,
    );
    if (modelStr) {
      const tier =
        config.display.showPlanTier && stdin?.plan_tier
          ? ` ${paint(sanitizeText(stdin.plan_tier), COLORS.yellow)}`
          : '';
      headerParts.push(`${paint(`[${modelStr}]`, COLORS.brightMagenta, ITALIC)}${tier}`);
    }
  }
  if (config.display.showProject) {
    const rawDir = stdin?.workspace?.project_dir || stdin?.workspace?.current_dir || stdin?.cwd;
    const shortDir = formatWorkspacePath(rawDir, layoutMode === 'compact' ? 1 : config.pathLevels);
    const linkedDir = rawDir
      ? fileLink(rawDir, paint(shortDir, COLORS.brightWhite, BOLD), config.hyperlinks)
      : '';
    const vcsSeg = renderVcsSegment(vcs, config);
    const projVcs = [linkedDir, vcsSeg].filter(Boolean).join(' ');
    if (projVcs) headerParts.push(projVcs);
  }
  if (config.display.showDuration && transcript.sessionStartedAt !== null && now >= transcript.sessionStartedAt) {
    const elapsedSec = (now - transcript.sessionStartedAt) / 1000;
    if (elapsedSec >= 5) {
      headerParts.push(paint(`⏱ ${formatDurationSeconds(elapsedSec)}`, COLORS.gray));
    }
  }
  if (config.display.customTag) {
    headerParts.push(paint(sanitizeText(config.display.customTag), COLORS.cyan));
  }

  const line1 = headerParts.filter(Boolean).join(SEP);

  // Build Metrics Segments
  const metricsParts: string[] = [];
  metricsParts.push(renderContextSegment(stdin, config, labels, effectiveWidth));
  const quotaSeg = renderQuotaSegment(
    stdin,
    config,
    labels,
    effectiveWidth,
    layoutMode === 'compact' ? 1 : 2,
  );
  if (quotaSeg) metricsParts.push(quotaSeg);
  const metaSeg = renderCountsAndMeta(stdin, memory, config, labels, layoutMode === 'compact');
  if (metaSeg) metricsParts.push(metaSeg);

  const line2 = metricsParts.filter(Boolean).join(SEP);

  let rawLines: string[];

  if (layoutMode === 'compact') {
    // Compact layout: 1 line if wide enough, otherwise 2 dense lines + running tool/todo alerts
    const singleCandidate = [line1, line2].filter(Boolean).join(SEP);
    if (effectiveWidth !== null && textWidth(singleCandidate) <= effectiveWidth) {
      rawLines = [singleCandidate];
    } else {
      rawLines = [line1, line2].filter(Boolean);
    }
    const runningTool = transcript.tools.find((t) => t.status === 'running');
    if (config.display.showTools && runningTool) {
      const target = runningTool.target ? `: ${runningTool.target}` : '';
      rawLines.push(`${paint('◐', COLORS.brightCyan, BOLD)} ${runningTool.name}${paint(target, COLORS.gray)}`);
    }
  } else {
    // Expanded / Adaptive layout
    const toolsLine = renderToolsActivityLine(transcript, config, labels);
    const subagentsLine = renderSubagentsLine(transcript, config, now);
    const todosLine = renderTodosLine(transcript, config, labels);

    // On ultra-wide terminals (>= 132 cols) with no extra activity lines, join line1 + line2 on 1 sleek line
    const activityLines = [toolsLine, subagentsLine, todosLine].filter(
      (l): l is string => typeof l === 'string' && l.length > 0,
    );

    if (
      config.lineLayout === 'adaptive' &&
      activityLines.length === 0 &&
      effectiveWidth !== null &&
      effectiveWidth >= 132
    ) {
      const wideLine = [line1, line2].filter(Boolean).join(SEP);
      if (textWidth(wideLine) <= effectiveWidth) {
        rawLines = [wideLine];
      } else {
        rawLines = joinWithRail([line1, line2].filter(Boolean), true);
      }
    } else {
      rawLines = joinWithRail([line1, line2, ...activityLines].filter(Boolean), true);
    }
  }

  return rawLines.map((line) => {
    const clipped =
      effectiveWidth !== null && effectiveWidth > 0 ? truncateAnsi(line, effectiveWidth) : line;
    return `${RESET}${clipped}${RESET}`;
  });
}

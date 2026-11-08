import * as fs from 'node:fs';
import * as path from 'node:path';
import { sanitizeText } from './ansi.ts';
import type {
  AgyStdinData,
  SubagentEntry,
  TodoItem,
  ToolEntry,
  TranscriptSummary,
} from './types.ts';

const HEAD_READ_BYTES = 8 * 1024;
const TAIL_READ_BYTES = 256 * 1024;
const MAX_TASK_MD_BYTES = 64 * 1024;

interface RawToolCall {
  name?: string;
  args?: Record<string, unknown>;
}

interface RawStep {
  step_index?: number;
  source?: string;
  type?: string;
  status?: string;
  created_at?: string;
  content?: unknown;
  tool_calls?: RawToolCall[] | null;
}

/**
 * Antigravity `transcript.jsonl` often serializes tool argument values as JSON-encoded strings
 * (e.g. `"\"/path/to/file\""`). Unwrap safely so targets display cleanly.
 */
export function unwrapArgValue(raw: unknown): string {
  if (typeof raw === 'number' || typeof raw === 'boolean') return String(raw);
  if (typeof raw !== 'string') return '';
  const trimmed = raw.trim();
  if (trimmed.startsWith('"') && trimmed.endsWith('"')) {
    try {
      const decoded: unknown = JSON.parse(trimmed);
      if (typeof decoded === 'string') return sanitizeText(decoded);
    } catch {
      // keep raw
    }
  }
  return sanitizeText(trimmed);
}

function parseIsoMs(iso: unknown): number | null {
  if (typeof iso !== 'string' || !iso) return null;
  const ms = Date.parse(iso);
  return Number.isFinite(ms) ? ms : null;
}

function summarizeToolTarget(name: string, args: Record<string, unknown> | undefined): string {
  if (!args) return '';
  const fileArg = unwrapArgValue(args.Path ?? args.TargetFile ?? args.File ?? args.DirectoryPath);
  if (fileArg) {
    return path.basename(fileArg) || fileArg;
  }
  if (name === 'run_command') {
    const summary = unwrapArgValue(args.toolSummary);
    if (summary) return summary.slice(0, 28);
    const cmd = unwrapArgValue(args.CommandLine);
    return cmd ? cmd.split(/\s+/).slice(0, 2).join(' ').slice(0, 24) : '';
  }
  if (name === 'search_web') {
    return unwrapArgValue(args.query).slice(0, 24);
  }
  if (name === 'grep_search' || name === 'find_by_name') {
    return unwrapArgValue(args.Query ?? args.Pattern).slice(0, 24);
  }
  if (name === 'call_mcp_tool') {
    const server = unwrapArgValue(args.ServerName);
    const tool = unwrapArgValue(args.ToolName);
    return server && tool ? `${server}:${tool}` : server || tool;
  }
  if (name === 'invoke_subagent') {
    return unwrapArgValue(args.Role ?? args.TypeName).slice(0, 26);
  }
  return '';
}

function detectSkillFromPath(filePath: string): string | null {
  const normalized = filePath.replace(/\\/g, '/');
  const match = /\/skills\/([^/]+)\/SKILL\.md$/i.exec(normalized);
  return match ? sanitizeText(match[1]) : null;
}

function readTranscriptSteps(transcriptPath: string): {
  firstTimestamp: number | null;
  steps: RawStep[];
} {
  const fd = fs.openSync(transcriptPath, 'r');
  try {
    const stat = fs.fstatSync(fd);
    if (!stat.isFile() || stat.size === 0) {
      return { firstTimestamp: null, steps: [] };
    }

    // Read head chunk for accurate session start time even when transcript is multi-MB
    const headLen = Math.min(stat.size, HEAD_READ_BYTES);
    const headBuf = Buffer.alloc(headLen);
    fs.readSync(fd, headBuf, 0, headLen, 0);
    const firstNewline = headBuf.indexOf(0x0a);
    const firstLine = headBuf
      .subarray(0, firstNewline === -1 ? headLen : firstNewline)
      .toString('utf8')
      .trim();
    let firstTimestamp: number | null = null;
    if (firstLine) {
      try {
        const parsedFirst = JSON.parse(firstLine) as RawStep;
        firstTimestamp = parseIsoMs(parsedFirst.created_at);
      } catch {
        // ignore
      }
    }

    // Read tail chunk for recent activity
    const offset = Math.max(0, stat.size - TAIL_READ_BYTES);
    const tailLen = stat.size - offset;
    const tailBuf = Buffer.alloc(tailLen);
    fs.readSync(fd, tailBuf, 0, tailLen, offset);
    let tailText = tailBuf.toString('utf8');
    if (offset > 0) {
      const nl = tailText.indexOf('\n');
      tailText = nl === -1 ? '' : tailText.slice(nl + 1);
    }

    const steps: RawStep[] = [];
    for (const line of tailText.split('\n')) {
      const trimmed = line.trim();
      if (!trimmed) continue;
      try {
        const step = JSON.parse(trimmed) as RawStep;
        if (step && typeof step === 'object') steps.push(step);
      } catch {
        // skip truncated/partial JSON line being written concurrently
      }
    }
    if (firstTimestamp === null && steps.length > 0) {
      firstTimestamp = parseIsoMs(steps[0].created_at);
    }
    return { firstTimestamp, steps };
  } finally {
    fs.closeSync(fd);
  }
}

export function parseTaskMarkdown(taskMdPath: string | null): TodoItem[] {
  if (!taskMdPath) return [];
  try {
    if (!fs.existsSync(taskMdPath)) return [];
    const stat = fs.statSync(taskMdPath);
    if (!stat.isFile() || stat.size > MAX_TASK_MD_BYTES) return [];
    const content = fs.readFileSync(taskMdPath, 'utf8');
    const todos: TodoItem[] = [];
    for (const rawLine of content.split('\n')) {
      const match = /^\s*[-*]\s+\[([ xX/])\]\s+(.+)$/.exec(rawLine);
      if (!match) continue;
      const marker = match[1];
      const text = sanitizeText(match[2].replace(/`([^`]+)`/g, '$1'));
      if (!text) continue;
      const status: TodoItem['status'] =
        marker === '/' ? 'in_progress' : marker.toLowerCase() === 'x' ? 'completed' : 'pending';
      todos.push({ text, status });
    }
    return todos;
  } catch {
    return [];
  }
}

export function parseTranscriptAndTasks(
  transcriptPath: string | null,
  taskMdPath: string | null,
  stdin: AgyStdinData | null,
): TranscriptSummary {
  const todos = parseTaskMarkdown(taskMdPath);
  const empty: TranscriptSummary = {
    tools: [],
    skills: [],
    mcpServers: [],
    mcpErrors: [],
    subagents: [],
    todos,
    sessionStartedAt: null,
    lastStepAt: null,
    stepCount: 0,
  };

  let steps: RawStep[] = [];
  let sessionStartedAt: number | null = null;
  if (transcriptPath && fs.existsSync(transcriptPath)) {
    try {
      const read = readTranscriptSteps(transcriptPath);
      steps = read.steps;
      sessionStartedAt = read.firstTimestamp;
    } catch {
      // gracefully return stdin-derived subagents + todos if transcript isn't readable yet
    }
  }

  const skillsSet = new Set<string>();
  const mcpSet = new Set<string>();
  const mcpErrorSet = new Set<string>();
  const rawTools: Array<{
    id: string;
    name: string;
    target: string;
    status: 'running' | 'completed' | 'error';
    startedAt?: number;
    endedAt?: number;
  }> = [];
  const transcriptAgents: SubagentEntry[] = [];

  let lastStepAt: number | null = null;
  let maxStepIdx = 0;

  for (let i = 0; i < steps.length; i++) {
    const step = steps[i];
    const ts = parseIsoMs(step.created_at);
    if (ts !== null) lastStepAt = ts;
    if (typeof step.step_index === 'number' && step.step_index + 1 > maxStepIdx) {
      maxStepIdx = step.step_index + 1;
    }

    if (step.type === 'USER_INPUT' && typeof step.content === 'string') {
      const slashMatch = /(?:^|\s)\/([a-z0-9][a-z0-9_-]{1,32})\b/i.exec(step.content);
      if (slashMatch) {
        const cmd = slashMatch[1].toLowerCase();
        if (!['exit', 'quit', 'help', 'clear', 'config', 'model', 'statusline'].includes(cmd)) {
          skillsSet.add(cmd);
        }
      }
    }

    if (step.type === 'PLANNER_RESPONSE' && Array.isArray(step.tool_calls) && step.tool_calls.length > 0) {
      // In Antigravity transcripts, the next N GENERIC steps correspond sequentially to these tool_calls.
      const results: RawStep[] = [];
      for (let j = i + 1; j < steps.length && results.length < step.tool_calls.length; j++) {
        if (steps[j].type === 'PLANNER_RESPONSE' || steps[j].type === 'USER_INPUT') break;
        if (steps[j].type === 'GENERIC') results.push(steps[j]);
      }

      step.tool_calls.forEach((tc, idx) => {
        const rawName = sanitizeText(tc.name || 'tool');
        const args = tc.args && typeof tc.args === 'object' ? tc.args : undefined;
        const resultStep = results[idx];
        const endedAt = resultStep ? parseIsoMs(resultStep.created_at) ?? undefined : undefined;

        let status: 'running' | 'completed' | 'error' = 'running';
        if (resultStep) {
          const contentStr = typeof resultStep.content === 'string' ? resultStep.content : '';
          const isErr =
            resultStep.status === 'ERROR' ||
            /^Error:/i.test(contentStr.trim()) ||
            /Command exited with code [1-9]/.test(contentStr);
          status = isErr ? 'error' : 'completed';
        } else if (stdin?.agent_state === 'idle') {
          status = 'completed';
        }

        // Detect skill reads
        if (rawName === 'read_file' && args) {
          const skill = detectSkillFromPath(unwrapArgValue(args.Path));
          if (skill) skillsSet.add(skill);
        }

        // Detect MCP usage
        let mcpServer = '';
        if (rawName === 'call_mcp_tool' && args) {
          mcpServer = unwrapArgValue(args.ServerName);
        } else if (rawName.startsWith('mcp_')) {
          const parts = rawName.slice(4).split('_');
          if (parts[0]) mcpServer = parts[0];
        }
        if (mcpServer) {
          mcpSet.add(mcpServer);
          if (status === 'error') mcpErrorSet.add(mcpServer);
        }

        // Detect subagent launches
        if (rawName === 'invoke_subagent' && args) {
          transcriptAgents.push({
            name: unwrapArgValue(args.TypeName) || 'subagent',
            role: unwrapArgValue(args.Role) || undefined,
            model: unwrapArgValue(args.Model) || undefined,
            status: status === 'running' ? 'running' : 'completed',
            startedAt: ts ?? undefined,
            endedAt,
          });
        }

        const target = summarizeToolTarget(rawName, args);
        rawTools.push({
          id: `${step.step_index ?? i}:${idx}`,
          name: rawName,
          target,
          status,
          startedAt: ts ?? undefined,
          endedAt,
        });
      });
    }
  }

  // Fold consecutive/completed identical tool names while keeping running/error tools prominent
  const aggregated: ToolEntry[] = [];
  const completedByName = new Map<string, ToolEntry>();

  for (let i = rawTools.length - 1; i >= 0; i--) {
    const item = rawTools[i];
    if (item.status === 'running' || item.status === 'error') {
      aggregated.push({ ...item, count: 1 });
    } else {
      const existing = completedByName.get(item.name);
      if (existing) {
        existing.count += 1;
        if (existing.target && existing.target !== item.target) {
          existing.target = undefined;
        }
      } else {
        const entry: ToolEntry = { ...item, count: 1 };
        completedByName.set(item.name, entry);
        aggregated.push(entry);
      }
    }
  }

  // Sort: running first, then error, then most-recent completed
  const priority = { running: 0, error: 1, completed: 2 } as const;
  aggregated.sort((a, b) => priority[a.status] - priority[b.status]);

  // Merge stdin.subagents (authoritative live status from agy CLI) with transcript subagent metadata
  const mergedSubagents: SubagentEntry[] = [];
  if (Array.isArray(stdin?.subagents) && stdin.subagents.length > 0) {
    stdin.subagents.forEach((sa, idx) => {
      const match = transcriptAgents[idx];
      mergedSubagents.push({
        name: sanitizeText(sa.name || match?.name || 'subagent'),
        role: sanitizeText(sa.role || match?.role || '') || undefined,
        model: match?.model,
        status: sa.status === 'running' ? 'running' : 'completed',
        startedAt: match?.startedAt,
        endedAt: match?.endedAt,
      });
    });
  } else {
    mergedSubagents.push(...transcriptAgents);
  }

  return {
    tools: aggregated,
    skills: [...skillsSet],
    mcpServers: [...mcpSet],
    mcpErrors: [...mcpErrorSet],
    subagents: mergedSubagents,
    todos,
    sessionStartedAt: sessionStartedAt ?? empty.sessionStartedAt,
    lastStepAt,
    stepCount: maxStepIdx || steps.length,
  };
}

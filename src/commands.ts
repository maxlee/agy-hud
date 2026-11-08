import * as fs from 'node:fs';
import { BOLD, COLORS, RESET, paint, stripAnsi } from './ansi.ts';
import {
  CONFIG_SCHEMA,
  applyDotOverrides,
  loadConfigFile,
  validateSetting,
  type HudConfig,
} from './config.ts';
import { parseMemoryInfo, parseVcsInfo } from './probes.ts';
import { renderHudLines } from './render.ts';
import { parseTranscriptAndTasks } from './transcript.ts';
import type {
  AgyStdinData,
  CageOutput,
  DoctorFacts,
  LogicPaths,
  ProbeAnswers,
  TranscriptSummary,
} from './types.ts';

function parseCliArgs(args: string[]): { flags: Map<string, string[]>; words: string[] } {
  const flags = new Map<string, string[]>();
  const words: string[] = [];
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (!a.startsWith('--')) {
      words.push(a);
      continue;
    }
    const nextIsVal = i + 1 < args.length && !args[i + 1].startsWith('--');
    const val = nextIsVal ? args[++i] : 'true';
    flags.set(a, [...(flags.get(a) ?? []), val]);
  }
  return { flags, words };
}

function parseAssignmentValue(raw: string): unknown {
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
}

function redactCommand(cmd: string): string {
  return cmd
    .replace(/\b(Bearer)\s+["']?[^"'\s]+/gi, '$1 [REDACTED]')
    .replace(/\b(token|api[_-]?key|secret|password|auth)(=|:)\s*["']?[^"'\s]+/gi, '$1$2[REDACTED]')
    .replace(/\bsk-[A-Za-z0-9_-]{8,}\b/g, 'sk-[REDACTED]')
    .replace(/\bAIza[0-9A-Za-z_-]{20,}\b/g, 'AIza[REDACTED]')
    .trim()
    .slice(0, 180);
}

function readJsonSafe(filePath: string | null): Record<string, unknown> | null {
  if (!filePath) return null;
  try {
    if (!fs.existsSync(filePath)) return null;
    const raw: unknown = JSON.parse(fs.readFileSync(filePath, 'utf8').replace(/^\uFEFF/, ''));
    return raw && typeof raw === 'object' && !Array.isArray(raw)
      ? (raw as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

function runDoctor(facts: DoctorFacts, paths: LogicPaths): CageOutput {
  const loaded = loadConfigFile(paths.config);
  const cliSettings = readJsonSafe(paths.cliSettings);
  const sl =
    cliSettings && typeof cliSettings.statusLine === 'object' && cliSettings.statusLine !== null
      ? (cliSettings.statusLine as Record<string, unknown>)
      : null;

  const cmd = typeof sl?.command === 'string' ? sl.command : '';
  const enabled = sl?.enabled !== false;
  const kind = !cmd
    ? 'none'
    : cmd.includes('agy-hud') || cmd.includes('hud.mjs') || cmd.includes('statusline.mjs')
      ? 'agy-hud'
      : 'other';

  const findings: string[] = [];
  if (!facts.runtime.supported) {
    findings.push(
      `Node.js ${facts.runtime.version} is older than 22.18.0. Fix: install Node 22.18+ and run \`node scripts/hud.mjs install\`.`,
    );
  }
  if (kind === 'none') {
    findings.push(
      'No statusLine is configured in ~/.gemini/antigravity-cli/settings.json. Fix: run `node scripts/hud.mjs install` (or `/statusline` in agy CLI).',
    );
  } else if (kind === 'other') {
    findings.push(
      `statusLine currently runs another script (${redactCommand(cmd)}). Fix: ask the user first, then run \`node scripts/hud.mjs install --replace\`.`,
    );
  } else if (!enabled) {
    findings.push(
      'statusLine is configured for agy-hud but currently disabled (`enabled: false`). Fix: run `/statusline enable` inside agy CLI or `node scripts/hud.mjs install`.',
    );
  }

  if (kind === 'agy-hud' && enabled && !facts.heartbeat) {
    findings.push(
      'agy CLI has not executed agy-hud yet. Fix: start `agy` and send a prompt to trigger the first statusLine render.',
    );
  } else if (facts.heartbeat && facts.heartbeat.outcome !== 'ok') {
    findings.push(`Last render reported outcome "${facts.heartbeat.outcome}". Fix: run \`node scripts/hud.mjs preview --last\` to inspect.`);
  }

  for (const p of loaded.problems) {
    findings.push(`Config issue: ${p} Fix: run \`node scripts/hud.mjs set <key>=<value>\` or \`node scripts/hud.mjs schema\`.`);
  }

  const report = {
    runtime: facts.runtime,
    plugin: facts.plugin,
    statusLine: {
      settingsFile: paths.cliSettings,
      kind,
      enabled,
      command: cmd ? redactCommand(cmd) : null,
      stackWithDefault: Boolean(sl?.stack_with_default),
    },
    lastRender: facts.heartbeat
      ? {
          secondsAgo: Math.max(0, Math.round((Date.now() - facts.heartbeat.at) / 1000)),
          outcome: facts.heartbeat.outcome,
          durationMs: facts.heartbeat.durationMs ?? null,
          columns: facts.heartbeat.columns,
        }
      : null,
    config: {
      path: paths.config,
      problems: loaded.problems,
    },
    findings:
      findings.length > 0
        ? findings
        : ['None. agy-hud is healthy, configured, and ready to render.'],
  };

  return { lines: JSON.stringify(report, null, 2).split('\n') };
}

function getByDotPath(obj: unknown, dotKey: string): unknown {
  return dotKey.split('.').reduce<unknown>((acc, part) => {
    if (acc && typeof acc === 'object' && part in (acc as Record<string, unknown>)) {
      return (acc as Record<string, unknown>)[part];
    }
    return undefined;
  }, obj);
}

function runSchema(words: string[], paths: LogicPaths): CageOutput {
  const { config } = loadConfigFile(paths.config);
  const query = words.map((w) => w.toLowerCase());
  const matches = CONFIG_SCHEMA.filter((entry) => {
    if (query.length === 0) return true;
    const hay = `${entry.key} ${entry.type} ${entry.description}`.toLowerCase();
    return query.every((q) => hay.includes(q));
  });

  if (matches.length === 0) {
    return {
      lines: [`No config keys matched "${words.join(' ')}". Run \`hud schema\` with no arguments to list all ${CONFIG_SCHEMA.length} keys.`],
      failed: true,
    };
  }

  const lines = matches.map((m) => {
    const currentVal = JSON.stringify(getByDotPath(config, m.key));
    const range =
      m.allowed !== undefined
        ? `[${m.allowed.map((a) => JSON.stringify(a)).join(' | ')}]`
        : m.min !== undefined && m.max !== undefined
          ? `[${m.min}..${m.max}]`
          : m.type;
    return `${paint(m.key, COLORS.brightCyan, BOLD)} = ${paint(currentVal, COLORS.brightYellow)} ${paint(range, COLORS.gray)} — ${m.description}`;
  });
  return { lines };
}

function samplePresets(now: number): Record<
  string,
  { stdin: AgyStdinData; transcript: TranscriptSummary; probes: ProbeAnswers }
> {
  const baseStdin: AgyStdinData = {
    cwd: '/Users/dev/agy-hud',
    workspace: { current_dir: '/Users/dev/agy-hud', project_dir: '/Users/dev/agy-hud' },
    model: {
      id: 'Gemini 4 Argon (Medium)',
      display_name: 'Gemini 4 Argon (Medium)',
      effort: 'medium',
    },
    version: '1.5.3',
    product: 'antigravity',
    context_window: {
      total_input_tokens: 142_800,
      total_output_tokens: 18_400,
      context_window_size: 1_048_576,
      used_percentage: 28.4,
      remaining_percentage: 71.6,
      current_usage: {
        input_tokens: 34_000,
        output_tokens: 1_850,
        cache_creation_input_tokens: 4_000,
        cache_read_input_tokens: 112_000,
      },
    },
    exceeds_200k_tokens: false,
    quota: {
      'gemini-weekly': {
        remaining_fraction: 0.84,
        reset_in_seconds: 412_200,
      },
    },
    agent_state: 'idle',
    cycle_mode: 'auto',
    sandbox: { enabled: true },
    artifact_count: 2,
    plan_tier: 'Pro',
    task_count: 1,
    terminal_width: 108,
    cost: { total_usd: 0.38, subagent_usd: 0.09, estimated: true },
  };

  const richTranscript: TranscriptSummary = {
    tools: [
      { id: '4:0', name: 'run_command', target: 'Unit test execution', status: 'running', count: 1, startedAt: now - 4000 },
      { id: '3:0', name: 'edit_file', target: 'render.ts', status: 'completed', count: 2 },
      { id: '2:0', name: 'read_file', target: 'SKILL.md', status: 'completed', count: 5 },
    ],
    skills: ['hud', 'agy-customizations'],
    mcpServers: ['context7'],
    mcpErrors: [],
    subagents: [
      {
        name: 'research',
        role: 'CLI Docs Researcher',
        status: 'running',
        startedAt: now - 75_000,
      },
      {
        name: 'self',
        role: 'Security Auditor',
        status: 'completed',
        startedAt: now - 140_000,
        endedAt: now - 80_000,
      },
    ],
    todos: [
      { text: 'Design capability vs logic cage', status: 'completed' },
      { text: 'Implement Antigravity stdin & transcript parser', status: 'completed' },
      { text: 'Verify node --permission escape matrix', status: 'in_progress' },
      { text: 'Validate plugin manifest with agy plugin validate --strict', status: 'pending' },
    ],
    sessionStartedAt: now - 740_000,
    lastStepAt: now - 2_000,
    stepCount: 24,
  };

  const richProbes: ProbeAnswers = {
    vcs: JSON.stringify({
      type: 'git',
      branch: 'feat/agy-hud',
      dirty: true,
      ahead: 2,
      behind: 0,
      modified: 3,
      added: 2,
      deleted: 0,
      untracked: 1,
      conflicts: 0,
    }),
    memory: JSON.stringify({
      totalBytes: 34_359_738_368,
      usedBytes: 21_990_232_555,
      usedPercent: 64,
    }),
  };

  return {
    idle: {
      stdin: { ...baseStdin, artifact_count: 0, task_count: 0 },
      transcript: {
        tools: [],
        skills: [],
        mcpServers: [],
        mcpErrors: [],
        subagents: [],
        todos: [],
        sessionStartedAt: now - 320_000,
        lastStepAt: now - 10_000,
        stepCount: 8,
      },
      probes: richProbes,
    },
    tool_use: {
      stdin: {
        ...baseStdin,
        agent_state: 'tool_use',
        subagents: [
          { name: 'research', role: 'CLI Docs Researcher', status: 'running' },
          { name: 'self', role: 'Security Auditor', status: 'completed' },
        ],
      },
      transcript: richTranscript,
      probes: richProbes,
    },
    confirm: {
      stdin: {
        ...baseStdin,
        agent_state: 'tool_use',
        cycle_mode: 'plan',
        tool_confirmation_pending: true,
        pending_input_count: 1,
        vim: { mode: 'NORMAL' },
      },
      transcript: richTranscript,
      probes: richProbes,
    },
    'high-context': {
      stdin: {
        ...baseStdin,
        agent_state: 'working',
        exceeds_200k_tokens: true,
        context_window: {
          ...baseStdin.context_window,
          used_percentage: 89.6,
          remaining_percentage: 10.4,
          current_usage: {
            input_tokens: 820_000,
            output_tokens: 19_500,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 100_000,
          },
        },
        quota: {
          'gemini-weekly': {
            remaining_fraction: 0.08,
            reset_in_seconds: 5_400,
          },
        },
      },
      transcript: richTranscript,
      probes: richProbes,
    },
  };
}

function parseSetPairs(
  pairs: string[],
  baseConfig: HudConfig,
): { config: HudConfig; assignments: Array<[string, unknown]>; problems: string[] } {
  const patch: Record<string, unknown> = {};
  const assignments: Array<[string, unknown]> = [];
  const problems: string[] = [];

  for (const pair of pairs) {
    const eq = pair.indexOf('=');
    if (eq <= 0) {
      problems.push(`Invalid assignment "${pair}" (expected key=value).`);
      continue;
    }
    const key = pair.slice(0, eq).trim();
    const val = parseAssignmentValue(pair.slice(eq + 1).trim());
    const err = validateSetting(key, val);
    if (err) {
      problems.push(err);
      continue;
    }
    assignments.push([key, val]);
    const parts = key.split('.');
    let cursor = patch;
    for (const p of parts.slice(0, -1)) {
      if (!cursor[p] || typeof cursor[p] !== 'object') cursor[p] = {};
      cursor = cursor[p] as Record<string, unknown>;
    }
    cursor[parts[parts.length - 1]] = val;
  }

  const merged = applyDotOverrides(baseConfig, patch);
  return {
    config: merged.config,
    assignments,
    problems: [...problems, ...merged.problems],
  };
}

function runPreview(args: string[], paths: LogicPaths, now: number): CageOutput {
  const { flags } = parseCliArgs(args);
  const loaded = loadConfigFile(paths.config);
  const setPairs = flags.get('--set') ?? [];
  const overridden = parseSetPairs(setPairs, loaded.config);
  if (overridden.problems.length > 0) {
    return { lines: overridden.problems, failed: true };
  }
  const config = overridden.config;

  const widthRaw = flags.get('--width')?.[0];
  const explicitWidth = widthRaw ? Number.parseInt(widthRaw, 10) : null;

  if (flags.has('--last')) {
    const snap = readJsonSafe(paths.lastSnapshot);
    if (!snap) {
      return {
        lines: ['No previous live render snapshot found yet. Run a prompt inside `agy` first, or run `hud preview` without `--last`.'],
        failed: true,
      };
    }
    const stdin = (snap.stdin as AgyStdinData) ?? null;
    const probes = (snap.probes as ProbeAnswers) ?? {};
    const transcript = parseTranscriptAndTasks(paths.transcript, paths.taskMd, stdin);
    const lines = renderHudLines({
      stdin,
      config,
      transcript,
      vcs: parseVcsInfo(stdin, probes),
      memory: parseMemoryInfo(probes),
      columns: explicitWidth ?? stdin?.terminal_width ?? 100,
      now,
    });
    return { lines };
  }

  const presets = samplePresets(now);
  const stateArg = flags.get('--state')?.[0] ?? 'tool_use';

  if (stateArg === 'showcase') {
    const out: string[] = [];
    const scenarios: Array<{ title: string; presetKey: string; width: number; extraConfig?: Partial<HudConfig> }> = [
      { title: '1) Standard Expanded / Active Multi-Agent + Tools + Checklist (108 cols)', presetKey: 'tool_use', width: 108 },
      { title: '2) Pending Tool Confirmation + Plan Mode + Vim Badge (108 cols)', presetKey: 'confirm', width: 108 },
      { title: '3) Ultra-Wide Adaptive Single-Line Idle (196 cols)', presetKey: 'idle', width: 196 },
      { title: '4) Narrow Terminal Auto-Compact + Critical Context Alert (68 cols)', presetKey: 'high-context', width: 68 },
      {
        title: '5) 简体中文界面模式 (`language=zh-Hans`, 108 cols)',
        presetKey: 'tool_use',
        width: 108,
        extraConfig: { language: 'zh-Hans' },
      },
    ];

    for (const sc of scenarios) {
      const p = presets[sc.presetKey];
      const cfg = sc.extraConfig ? { ...config, ...sc.extraConfig } : config;
      out.push(paint(`── ${sc.title} ──`, COLORS.gray, BOLD));
      const rendered = renderHudLines({
        stdin: p.stdin,
        config: cfg,
        transcript: p.transcript,
        vcs: parseVcsInfo(p.stdin, p.probes),
        memory: parseMemoryInfo(p.probes),
        columns: explicitWidth ?? sc.width,
        now,
      });
      out.push(...rendered, '');
    }
    return { lines: out.slice(0, -1) };
  }

  if (stateArg in presets) {
    const p = presets[stateArg];
    const lines = renderHudLines({
      stdin: p.stdin,
      config,
      transcript: p.transcript,
      vcs: parseVcsInfo(p.stdin, p.probes),
      memory: parseMemoryInfo(p.probes),
      columns: explicitWidth ?? p.stdin.terminal_width ?? 108,
      now,
    });
    return { lines };
  }

  // Custom inline JSON state
  try {
    const customStdin = JSON.parse(stateArg) as AgyStdinData;
    const lines = renderHudLines({
      stdin: customStdin,
      config,
      transcript: {
        tools: [],
        skills: [],
        mcpServers: [],
        mcpErrors: [],
        subagents: [],
        todos: [],
        sessionStartedAt: null,
        lastStepAt: null,
        stepCount: 0,
      },
      vcs: parseVcsInfo(customStdin, {}),
      memory: null,
      columns: explicitWidth ?? customStdin.terminal_width ?? 100,
      now,
    });
    return { lines };
  } catch {
    return {
      lines: [
        `Unknown --state "${stateArg}". Use one of: showcase, idle, tool_use, confirm, high-context, or a valid JSON object.`,
      ],
      failed: true,
    };
  }
}

function runSet(args: string[], paths: LogicPaths): CageOutput {
  const { words } = parseCliArgs(args);
  if (words.length === 0) {
    return {
      lines: ['Usage: `hud set <key>=<value> ...` or `hud set --undo`. Run `hud schema` to inspect available keys.'],
      failed: true,
    };
  }

  const loaded = loadConfigFile(paths.config);
  const parsed = parseSetPairs(words, loaded.config);
  if (parsed.problems.length > 0) {
    return { lines: parsed.problems, failed: true };
  }

  const nextRaw = JSON.parse(JSON.stringify(loaded.raw)) as Record<string, unknown>;
  for (const [key, val] of parsed.assignments) {
    const parts = key.split('.');
    let cursor = nextRaw;
    for (const p of parts.slice(0, -1)) {
      if (!cursor[p] || typeof cursor[p] !== 'object' || Array.isArray(cursor[p])) {
        cursor[p] = {};
      }
      cursor = cursor[p] as Record<string, unknown>;
    }
    cursor[parts[parts.length - 1]] = val;
  }

  return {
    lines: [
      `Updated ${parsed.assignments.length} setting(s) in ${paths.config}:`,
      ...parsed.assignments.map(([k, v]) => `  ${k} = ${JSON.stringify(v)}`),
    ],
    configToWrite: nextRaw,
  };
}

function scrubStdin(stdin: AgyStdinData | null): AgyStdinData | null {
  if (!stdin) return null;
  return {
    ...stdin,
    cwd: stdin.cwd ? '/redacted/project' : undefined,
    session_id: stdin.session_id ? '00000000-0000-0000-0000-000000000000' : undefined,
    conversation_id: stdin.conversation_id ? '00000000-0000-0000-0000-000000000000' : undefined,
    conversation_title: stdin.conversation_title ? '<redacted-title>' : undefined,
    transcript_path: stdin.transcript_path ? '/redacted/transcript.jsonl' : undefined,
    email: stdin.email ? 'user@example.com' : undefined,
    workspace: stdin.workspace
      ? { current_dir: '/redacted/project', project_dir: '/redacted/project' }
      : stdin.workspace,
  };
}

function runReport(args: string[], facts: DoctorFacts, paths: LogicPaths, now: number): CageOutput {
  const { flags } = parseCliArgs(args);
  const note = flags.get('--note')?.[0] ?? '';
  const loaded = loadConfigFile(paths.config);
  const snap = readJsonSafe(paths.lastSnapshot);
  const scrubbedStdin = scrubStdin((snap?.stdin as AgyStdinData | undefined) ?? null);

  const rendered = renderHudLines({
    stdin: scrubbedStdin,
    config: loaded.config,
    transcript: {
      tools: [],
      skills: [],
      mcpServers: [],
      mcpErrors: [],
      subagents: [],
      todos: [],
      sessionStartedAt: null,
      lastStepAt: null,
      stepCount: 0,
    },
    vcs: parseVcsInfo(scrubbedStdin, {}),
    memory: null,
    columns: scrubbedStdin?.terminal_width ?? 100,
    now,
  }).map((l) => stripAnsi(l));

  const payload = {
    note,
    pluginVersion: facts.plugin.version,
    nodeVersion: facts.runtime.version,
    config: loaded.raw,
    scrubbedStdin,
    renderedPlainLines: rendered,
  };

  return { lines: JSON.stringify(payload, null, 2).split('\n') };
}

export function executeSubcommand(
  name: string,
  args: string[],
  facts: DoctorFacts,
  paths: LogicPaths,
  now: number,
): CageOutput {
  switch (name) {
    case 'doctor':
      return runDoctor(facts, paths);
    case 'schema':
      return runSchema(args, paths);
    case 'preview':
      return runPreview(args, paths, now);
    case 'set':
      return runSet(args, paths);
    case 'report':
      return runReport(args, facts, paths, now);
    default:
      return {
        lines: [
          `Unknown subcommand "${name}".`,
          'Available commands: doctor, install, uninstall, preview, schema, set, report',
        ],
        failed: true,
      };
  }
}

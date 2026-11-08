import * as fs from 'node:fs';
import type {
  ContextValueMode,
  EffortFormatMode,
  Language,
  LineLayout,
  ModelFormatMode,
  PathLevels,
  QuotaValueMode,
} from './types.ts';

const MAX_CONFIG_BYTES = 64 * 1024;
const MAX_DEPTH = 6;
const UNSAFE_KEYS = new Set(['__proto__', 'prototype', 'constructor']);

export interface HudConfig {
  language: Language;
  lineLayout: LineLayout;
  pathLevels: PathLevels;
  maxWidth: number | null;
  hyperlinks: boolean;
  vcs: {
    enabled: boolean;
    showDirty: boolean;
    showAheadBehind: boolean;
    showFileStats: boolean;
  };
  display: {
    showState: boolean;
    showMode: boolean;
    showModel: boolean;
    modelFormat: ModelFormatMode;
    effortFormat: EffortFormatMode;
    showProject: boolean;
    showContextBar: boolean;
    barLength: number;
    contextValue: ContextValueMode;
    contextWarningThreshold: number;
    contextCriticalThreshold: number;
    showCacheHitRate: boolean;
    showQuota: boolean;
    quotaValue: QuotaValueMode;
    quotaBarEnabled: boolean;
    showQuotaReset: boolean;
    showCost: boolean;
    showSandbox: boolean;
    showCounts: boolean;
    showPlanTier: boolean;
    showMemory: boolean;
    showDuration: boolean;
    showTools: boolean;
    toolsMaxVisible: number;
    showSkillsAndMcp: boolean;
    showSubagents: boolean;
    showTodos: boolean;
    customTag: string;
  };
}

export const DEFAULT_CONFIG: HudConfig = {
  language: 'en',
  lineLayout: 'adaptive',
  pathLevels: 1,
  maxWidth: null,
  hyperlinks: true,
  vcs: {
    enabled: true,
    showDirty: true,
    showAheadBehind: true,
    showFileStats: false,
  },
  display: {
    showState: true,
    showMode: true,
    showModel: true,
    modelFormat: 'compact',
    effortFormat: 'text',
    showProject: true,
    showContextBar: true,
    barLength: 12,
    contextValue: 'both',
    contextWarningThreshold: 60,
    contextCriticalThreshold: 85,
    showCacheHitRate: true,
    showQuota: true,
    quotaValue: 'used',
    quotaBarEnabled: true,
    showQuotaReset: true,
    showCost: true,
    showSandbox: true,
    showCounts: true,
    showPlanTier: false,
    showMemory: false,
    showDuration: true,
    showTools: true,
    toolsMaxVisible: 4,
    showSkillsAndMcp: true,
    showSubagents: true,
    showTodos: true,
    customTag: '',
  },
};

export interface SchemaEntry {
  key: string;
  type: string;
  allowed?: readonly (string | number | boolean | null)[];
  min?: number;
  max?: number;
  description: string;
}

export const CONFIG_SCHEMA: readonly SchemaEntry[] = [
  {
    key: 'language',
    type: 'enum',
    allowed: ['en', 'zh-Hans'],
    description: 'HUD display language ("en" or "zh-Hans").',
  },
  {
    key: 'lineLayout',
    type: 'enum',
    allowed: ['adaptive', 'expanded', 'compact'],
    description: 'Layout mode: adaptive (auto by terminal_width), expanded (multi-line), or compact (dense).',
  },
  {
    key: 'pathLevels',
    type: 'enum',
    allowed: [1, 2, 3, 'full'],
    description: 'Number of trailing directory segments to show for the workspace path.',
  },
  {
    key: 'maxWidth',
    type: 'number|null',
    min: 40,
    max: 400,
    description: 'Optional hard column cap (null uses live stdin.terminal_width).',
  },
  {
    key: 'hyperlinks',
    type: 'boolean',
    description: 'Emit clickable OSC 8 file:// links for workspace and tool targets.',
  },
  {
    key: 'vcs.enabled',
    type: 'boolean',
    description: 'Show git/jj/hg branch and repository state.',
  },
  {
    key: 'vcs.showDirty',
    type: 'boolean',
    description: 'Show dirty indicator (*) when working tree has uncommitted changes.',
  },
  {
    key: 'vcs.showAheadBehind',
    type: 'boolean',
    description: 'Show ahead/behind commit arrows (↑N↓M) from VCS probe.',
  },
  {
    key: 'vcs.showFileStats',
    type: 'boolean',
    description: 'Show modified/added/deleted file counts (+A ~M -D).',
  },
  {
    key: 'display.showState',
    type: 'boolean',
    description: 'Show real-time agent_state badge (READY, WORKING, TOOL, CONFIRM?, etc.).',
  },
  {
    key: 'display.showMode',
    type: 'boolean',
    description: 'Show Shift+Tab cycle_mode (PLAN/AUTO/ACCEPT) and Vim mode.',
  },
  {
    key: 'display.showModel',
    type: 'boolean',
    description: 'Show active model name and reasoning effort.',
  },
  {
    key: 'display.modelFormat',
    type: 'enum',
    allowed: ['full', 'compact', 'short'],
    description: 'Model name verbosity: full ("Gemini 4 Argon (Medium)"), compact ("Gemini 4 Argon"), short ("Argon").',
  },
  {
    key: 'display.effortFormat',
    type: 'enum',
    allowed: ['text', 'symbol', 'hidden'],
    description: 'Effort display style: text ("med"), symbol ("⚡⚡"), or hidden.',
  },
  {
    key: 'display.showProject',
    type: 'boolean',
    description: 'Show workspace folder path.',
  },
  {
    key: 'display.showContextBar',
    type: 'boolean',
    description: 'Show Unicode context window progress bar.',
  },
  {
    key: 'display.barLength',
    type: 'number',
    min: 5,
    max: 30,
    description: 'Character width of progress bars (5..30).',
  },
  {
    key: 'display.contextValue',
    type: 'enum',
    allowed: ['percent', 'tokens', 'remaining', 'both'],
    description: 'Context metric format: percent ("14.2%"), tokens ("149k/1.0M"), remaining ("85.8% left"), or both.',
  },
  {
    key: 'display.contextWarningThreshold',
    type: 'number',
    min: 10,
    max: 99,
    description: 'Percentage at which context bar turns yellow.',
  },
  {
    key: 'display.contextCriticalThreshold',
    type: 'number',
    min: 20,
    max: 100,
    description: 'Percentage at which context bar turns red.',
  },
  {
    key: 'display.showCacheHitRate',
    type: 'boolean',
    description: 'Show prompt cache read hit percentage when cache tokens are observed.',
  },
  {
    key: 'display.showQuota',
    type: 'boolean',
    description: 'Show Antigravity quota buckets from stdin.quota.',
  },
  {
    key: 'display.quotaValue',
    type: 'enum',
    allowed: ['used', 'remaining'],
    description: 'Show quota as percentage used or remaining.',
  },
  {
    key: 'display.quotaBarEnabled',
    type: 'boolean',
    description: 'Render mini progress bar next to quota percentage.',
  },
  {
    key: 'display.showQuotaReset',
    type: 'boolean',
    description: 'Show countdown until quota bucket resets (e.g., "6d 11h").',
  },
  {
    key: 'display.showCost',
    type: 'boolean',
    description: 'Show estimated session USD spend when reported.',
  },
  {
    key: 'display.showSandbox',
    type: 'boolean',
    description: 'Show terminal sandbox status badge.',
  },
  {
    key: 'display.showCounts',
    type: 'boolean',
    description: 'Show counts of artifacts, running subagents, background tasks, and queued inputs.',
  },
  {
    key: 'display.showPlanTier',
    type: 'boolean',
    description: 'Show subscription tier badge (e.g. Pro).',
  },
  {
    key: 'display.showMemory',
    type: 'boolean',
    description: 'Show host RAM usage from system probe.',
  },
  {
    key: 'display.showDuration',
    type: 'boolean',
    description: 'Show elapsed conversation duration derived from transcript.',
  },
  {
    key: 'display.showTools',
    type: 'boolean',
    description: 'Show active and recent tool invocations line parsed from transcript.jsonl.',
  },
  {
    key: 'display.toolsMaxVisible',
    type: 'number',
    min: 1,
    max: 10,
    description: 'Maximum distinct tool entries displayed on the tools activity line.',
  },
  {
    key: 'display.showSkillsAndMcp',
    type: 'boolean',
    description: 'Show activated skills and MCP servers alongside tool activity.',
  },
  {
    key: 'display.showSubagents',
    type: 'boolean',
    description: 'Show detailed subagent activity line when subagents are active or completed.',
  },
  {
    key: 'display.showTodos',
    type: 'boolean',
    description: 'Show checklist task progress parsed from conversation task.md artifact.',
  },
  {
    key: 'display.customTag',
    type: 'string',
    description: 'Optional custom text label pinned to the header line.',
  },
];

const SCHEMA_MAP = new Map<string, SchemaEntry>(CONFIG_SCHEMA.map((s) => [s.key, s]));

export function validateSetting(key: string, value: unknown): string | null {
  const entry = SCHEMA_MAP.get(key);
  if (!entry) return `Unknown config key "${key}". Run \`hud schema\` to list valid keys.`;

  if (entry.type === 'boolean') {
    return typeof value === 'boolean' ? null : `${key} expects a boolean (true or false).`;
  }
  if (entry.type === 'string') {
    return typeof value === 'string' && value.length <= 80
      ? null
      : `${key} expects a string up to 80 characters.`;
  }
  if (entry.type === 'enum') {
    return entry.allowed?.includes(value as never)
      ? null
      : `${key} must be one of: ${entry.allowed?.map((v) => JSON.stringify(v)).join(', ')}.`;
  }
  if (entry.type === 'number') {
    if (typeof value !== 'number' || !Number.isFinite(value) || !Number.isInteger(value)) {
      return `${key} expects an integer.`;
    }
    if (entry.min !== undefined && value < entry.min) return `${key} must be >= ${entry.min}.`;
    if (entry.max !== undefined && value > entry.max) return `${key} must be <= ${entry.max}.`;
    return null;
  }
  if (entry.type === 'number|null') {
    if (value === null) return null;
    if (typeof value !== 'number' || !Number.isFinite(value) || !Number.isInteger(value)) {
      return `${key} expects an integer or null.`;
    }
    if (entry.min !== undefined && value < entry.min) return `${key} must be >= ${entry.min}.`;
    if (entry.max !== undefined && value > entry.max) return `${key} must be <= ${entry.max}.`;
    return null;
  }
  return null;
}

function isPlainObject(val: unknown): val is Record<string, unknown> {
  return typeof val === 'object' && val !== null && !Array.isArray(val);
}

function sanitizeObject(raw: unknown, depth = 0): Record<string, unknown> {
  if (!isPlainObject(raw) || depth > MAX_DEPTH) return {};
  const clean: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(raw)) {
    if (UNSAFE_KEYS.has(k)) continue;
    clean[k] = isPlainObject(v) ? sanitizeObject(v, depth + 1) : v;
  }
  return clean;
}

export function cloneDefaultConfig(): HudConfig {
  return JSON.parse(JSON.stringify(DEFAULT_CONFIG)) as HudConfig;
}

export function applyDotOverrides(
  base: HudConfig,
  rawObj: Record<string, unknown>,
): { config: HudConfig; problems: string[] } {
  const next = JSON.parse(JSON.stringify(base)) as Record<string, unknown>;
  const problems: string[] = [];

  const walk = (node: Record<string, unknown>, prefix: string) => {
    for (const [k, v] of Object.entries(node)) {
      if (UNSAFE_KEYS.has(k)) continue;
      const fullKey = prefix ? `${prefix}.${k}` : k;
      if (isPlainObject(v) && !SCHEMA_MAP.has(fullKey)) {
        walk(v, fullKey);
      } else {
        const err = validateSetting(fullKey, v);
        if (err) {
          problems.push(err);
        } else {
          const parts = fullKey.split('.');
          let cursor = next;
          for (const part of parts.slice(0, -1)) {
            if (!isPlainObject(cursor[part])) cursor[part] = {};
            cursor = cursor[part] as Record<string, unknown>;
          }
          cursor[parts[parts.length - 1]] = v;
        }
      }
    }
  };

  walk(sanitizeObject(rawObj), '');
  return { config: next as unknown as HudConfig, problems };
}

export function loadConfigFile(filePath: string | null): {
  config: HudConfig;
  raw: Record<string, unknown>;
  problems: string[];
} {
  const defaults = cloneDefaultConfig();
  if (!filePath) return { config: defaults, raw: {}, problems: [] };

  try {
    if (!fs.existsSync(filePath)) return { config: defaults, raw: {}, problems: [] };
    const stat = fs.statSync(filePath);
    if (!stat.isFile() || stat.size > MAX_CONFIG_BYTES) {
      return {
        config: defaults,
        raw: {},
        problems: [`Config file ${filePath} exceeds ${MAX_CONFIG_BYTES} bytes or is not a regular file.`],
      };
    }
    const text = fs.readFileSync(filePath, 'utf8').replace(/^\uFEFF/, '');
    if (!text.trim()) return { config: defaults, raw: {}, problems: [] };
    const parsed: unknown = JSON.parse(text);
    if (!isPlainObject(parsed)) {
      return { config: defaults, raw: {}, problems: ['Config root must be a JSON object.'] };
    }
    const sanitized = sanitizeObject(parsed);
    const { config, problems } = applyDotOverrides(defaults, sanitized);
    return { config, raw: sanitized, problems };
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    return { config: defaults, raw: {}, problems: [`Failed to parse config: ${msg}`] };
  }
}

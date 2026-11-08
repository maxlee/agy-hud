import * as path from 'node:path';
import { sanitizeText } from './ansi.ts';
import type {
  AgyStdinData,
  EffortFormatMode,
  ModelFormatMode,
  PathLevels,
  QuotaBucketView,
} from './types.ts';

export interface ContextMetrics {
  hasData: boolean;
  usedPercent: number;
  remainingPercent: number;
  usedTokens: number | null;
  windowSize: number | null;
  exceeds200k: boolean;
  cacheHitPercent: number | null;
}

const EFFORT_SYMBOLS: Record<string, string> = {
  low: '⚡',
  medium: '⚡⚡',
  med: '⚡⚡',
  high: '⚡⚡⚡',
  xhigh: '⚡⚡⚡⚡',
  max: '⚡⚡⚡⚡',
};

const EFFORT_SHORT: Record<string, string> = {
  low: 'low',
  medium: 'med',
  high: 'high',
  xhigh: 'xhigh',
  max: 'max',
};

export function formatModelName(
  model: AgyStdinData['model'],
  modelFormat: ModelFormatMode,
  effortFormat: EffortFormatMode,
): string {
  if (!model) return '';
  const rawName = sanitizeText(model.display_name || model.id || '');
  if (!rawName) return '';

  let base = rawName;
  // Extract parenthesized effort if model.effort wasn't separately populated
  const parenMatch = /\((Low|Medium|High|XHigh|Max|Thinking)\)\s*$/i.exec(rawName);
  const inferredEffort = sanitizeText(model.effort || (parenMatch ? parenMatch[1] : '')).toLowerCase();

  if (modelFormat === 'compact' || modelFormat === 'short') {
    base = base.replace(/\s*\([^)]*\)\s*$/, '').trim();
  }
  if (modelFormat === 'short') {
    base = base
      .replace(/^Google\s+/i, '')
      .replace(/^Gemini\s+\d+(\.\d+)?\s+/i, '')
      .replace(/^Claude\s+/i, '')
      .trim();
  }

  if (!inferredEffort || effortFormat === 'hidden') {
    return base;
  }
  // If full mode already includes "(Medium)", avoid duplicating effort text
  if (modelFormat === 'full' && parenMatch && effortFormat === 'text') {
    return base;
  }

  if (effortFormat === 'symbol') {
    const sym = EFFORT_SYMBOLS[inferredEffort] || inferredEffort;
    return `${base} ${sym}`;
  }
  const label = EFFORT_SHORT[inferredEffort] || inferredEffort;
  return `${base} · ${label}`;
}

export function extractContextMetrics(stdin: AgyStdinData | null): ContextMetrics {
  const cw = stdin?.context_window;
  if (!cw) {
    return {
      hasData: false,
      usedPercent: 0,
      remainingPercent: 100,
      usedTokens: null,
      windowSize: null,
      exceeds200k: Boolean(stdin?.exceeds_200k_tokens),
      cacheHitPercent: null,
    };
  }

  const windowSize =
    typeof cw.context_window_size === 'number' && cw.context_window_size > 0
      ? cw.context_window_size
      : null;

  const cu = cw.current_usage;
  let currentContextTokens: number | null = null;
  let cacheHitPercent: number | null = null;

  if (cu && typeof cu === 'object') {
    const inTok = typeof cu.input_tokens === 'number' ? cu.input_tokens : 0;
    const outTok = typeof cu.output_tokens === 'number' ? cu.output_tokens : 0;
    const cacheCreate =
      typeof cu.cache_creation_input_tokens === 'number' ? cu.cache_creation_input_tokens : 0;
    const cacheRead =
      typeof cu.cache_read_input_tokens === 'number' ? cu.cache_read_input_tokens : 0;
    const promptTotal = inTok + cacheCreate + cacheRead;
    if (promptTotal + outTok > 0) {
      currentContextTokens = promptTotal + outTok;
    }
    if (cacheRead > 0 && promptTotal > 0) {
      cacheHitPercent = Math.round((cacheRead / promptTotal) * 100);
    }
  }

  const totalTokensFallback =
    typeof cw.total_input_tokens === 'number' || typeof cw.total_output_tokens === 'number'
      ? (cw.total_input_tokens ?? 0) + (cw.total_output_tokens ?? 0)
      : null;

  const usedTokens = currentContextTokens ?? totalTokensFallback;

  let usedPercent =
    typeof cw.used_percentage === 'number' && Number.isFinite(cw.used_percentage)
      ? cw.used_percentage
      : windowSize && usedTokens !== null
        ? (usedTokens / windowSize) * 100
        : 0;
  usedPercent = Math.max(0, Math.min(100, usedPercent));

  const remainingPercent =
    typeof cw.remaining_percentage === 'number' && Number.isFinite(cw.remaining_percentage)
      ? Math.max(0, Math.min(100, cw.remaining_percentage))
      : Math.max(0, 100 - usedPercent);

  const hasData =
    typeof cw.used_percentage === 'number' || usedTokens !== null || windowSize !== null;

  return {
    hasData,
    usedPercent,
    remainingPercent,
    usedTokens,
    windowSize,
    exceeds200k: Boolean(stdin?.exceeds_200k_tokens || (usedTokens !== null && usedTokens > 200_000)),
    cacheHitPercent,
  };
}

function cleanQuotaLabel(bucketId: string): string {
  const clean = sanitizeText(bucketId)
    .replace(/^antigravity[-_]?/i, '')
    .replace(/^gemini[-_]?/i, '');
  return clean || sanitizeText(bucketId);
}

export function extractQuotaBuckets(stdin: AgyStdinData | null): QuotaBucketView[] {
  const quota = stdin?.quota;
  if (!quota || typeof quota !== 'object') return [];

  const buckets: QuotaBucketView[] = [];
  for (const [id, raw] of Object.entries(quota)) {
    if (!raw || typeof raw !== 'object' || raw.disabled) continue;
    let remainingPercent: number | null = null;
    let usedPercent: number | null = null;
    if (typeof raw.remaining_fraction === 'number' && Number.isFinite(raw.remaining_fraction)) {
      const frac = Math.max(0, Math.min(1, raw.remaining_fraction));
      remainingPercent = Math.round(frac * 1000) / 10;
      usedPercent = Math.round((1 - frac) * 1000) / 10;
    }
    const remainingAmount =
      typeof raw.remaining_amount === 'number' && Number.isFinite(raw.remaining_amount)
        ? raw.remaining_amount
        : null;
    const resetInSeconds =
      typeof raw.reset_in_seconds === 'number' && raw.reset_in_seconds >= 0
        ? raw.reset_in_seconds
        : null;

    if (usedPercent === null && remainingAmount === null) continue;
    buckets.push({
      id: sanitizeText(id),
      label: cleanQuotaLabel(id),
      usedPercent,
      remainingPercent,
      remainingAmount,
      resetInSeconds,
    });
  }
  return buckets;
}

export function formatCompactTokens(count: number): string {
  if (!Number.isFinite(count) || count < 0) return '0';
  if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M`;
  if (count >= 10_000) return `${Math.round(count / 1_000)}k`;
  if (count >= 1_000) return `${(count / 1_000).toFixed(1)}k`;
  return String(Math.round(count));
}

export function formatDurationSeconds(totalSeconds: number): string {
  if (!Number.isFinite(totalSeconds) || totalSeconds < 0) return '0s';
  const sec = Math.floor(totalSeconds);
  if (sec < 60) return `${sec}s`;
  const days = Math.floor(sec / 86400);
  const hours = Math.floor((sec % 86400) / 3600);
  const mins = Math.floor((sec % 3600) / 60);
  const remSec = sec % 60;

  if (days > 0) return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
  if (hours > 0) return mins > 0 ? `${hours}h ${mins}m` : `${hours}h`;
  return remSec > 0 && mins < 10 ? `${mins}m ${remSec}s` : `${mins}m`;
}

export function formatWorkspacePath(rawPath: string | undefined, levels: PathLevels): string {
  const clean = sanitizeText(rawPath || '');
  if (!clean) return '';
  if (levels === 'full') return clean;

  const normalized = clean.replace(/\\/g, '/').replace(/\/+$/, '');
  const parts = normalized.split('/').filter(Boolean);
  if (parts.length === 0) return '/';
  if (parts.length <= levels) return parts.join('/');
  return parts.slice(-levels).join(path.posix.sep);
}

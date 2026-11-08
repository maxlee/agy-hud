import { sanitizeText } from './ansi.ts';
import type { AgyStdinData, MemoryProbeInfo, ProbeAnswers, VcsProbeInfo } from './types.ts';

/**
 * Merges `stdin.vcs` metadata from `agy` CLI with optional rich VCS probe JSON from `collector.mjs`.
 */
export function parseVcsInfo(
  stdin: AgyStdinData | null,
  answers: ProbeAnswers,
): VcsProbeInfo | null {
  if (answers.vcs) {
    try {
      const parsed = JSON.parse(answers.vcs) as Partial<VcsProbeInfo>;
      if (parsed && typeof parsed === 'object' && parsed.branch) {
        return {
          type: (parsed.type as VcsProbeInfo['type']) || 'git',
          branch: sanitizeText(parsed.branch),
          dirty: Boolean(parsed.dirty),
          ahead: Number(parsed.ahead) || 0,
          behind: Number(parsed.behind) || 0,
          modified: Number(parsed.modified) || 0,
          added: Number(parsed.added) || 0,
          deleted: Number(parsed.deleted) || 0,
          untracked: Number(parsed.untracked) || 0,
          conflicts: Number(parsed.conflicts) || 0,
          repoUrl: typeof parsed.repoUrl === 'string' ? parsed.repoUrl : null,
        };
      }
    } catch {
      // Fall back to stdin.vcs
    }
  }

  const sv = stdin?.vcs;
  if (!sv || (!sv.branch && !sv.type)) return null;
  const branch = sanitizeText(sv.branch || sv.type || '');
  if (!branch) return null;

  return {
    type: (sv.type as VcsProbeInfo['type']) || 'git',
    branch,
    dirty: Boolean(sv.dirty),
    ahead: 0,
    behind: 0,
    modified: 0,
    added: 0,
    deleted: 0,
    untracked: 0,
    conflicts: 0,
    repoUrl: null,
  };
}

export function parseMemoryInfo(answers: ProbeAnswers): MemoryProbeInfo | null {
  if (!answers.memory) return null;
  try {
    const parsed = JSON.parse(answers.memory) as Partial<MemoryProbeInfo>;
    if (
      typeof parsed.totalBytes === 'number' &&
      parsed.totalBytes > 0 &&
      typeof parsed.usedBytes === 'number'
    ) {
      const usedPercent = Math.max(
        0,
        Math.min(100, Math.round((parsed.usedBytes / parsed.totalBytes) * 100)),
      );
      return {
        totalBytes: parsed.totalBytes,
        usedBytes: parsed.usedBytes,
        usedPercent,
      };
    }
  } catch {
    // ignore malformed probe
  }
  return null;
}

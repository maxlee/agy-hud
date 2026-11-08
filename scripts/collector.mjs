// Capability layer: owns all paths, safe fixed child_process probes (git, jj, hg, vm_stat),
// mtime+TTL VCS probe cache (`probe-cache.json`), incremental TypeScript stripping cache
// (`stripped/manifest.json`), and symlink-safe `data/` preparation.
import { execFile } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import nodeModule from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const PROBE_TIMEOUT_MS = 1200;
const PROBE_MAX_BUFFER = 512 * 1024;
const DEFAULT_VCS_CACHE_TTL_MS = 1500;
const MAX_CACHED_WORKSPACES = 16;

function homeDirOf(env = process.env) {
  return (process.platform === 'win32' ? env.USERPROFILE : env.HOME) || os.homedir();
}

function expandTilde(p, home) {
  if (p === '~') return home;
  if (p.startsWith('~/') || p.startsWith('~\\')) return path.join(home, p.slice(2));
  return p;
}

export function agyPaths(env = process.env) {
  const home = homeDirOf(env);
  const cliDir = env.AGY_CLI_DIR?.trim()
    ? path.resolve(expandTilde(env.AGY_CLI_DIR.trim(), home))
    : path.join(home, '.gemini', 'antigravity-cli');
  const hudHome = env.AGY_HUD_HOME?.trim()
    ? path.resolve(expandTilde(env.AGY_HUD_HOME.trim(), home))
    : path.join(cliDir, 'agy-hud');

  return {
    home,
    cliDir,
    hudHome,
    cliSettingsPath: path.join(cliDir, 'settings.json'),
    dataDir: path.join(hudHome, 'data'),
    strippedDir: path.join(hudHome, 'stripped'),
    configPath: path.join(hudHome, 'config.json'),
    undoPath: path.join(hudHome, 'config.undo.json'),
    heartbeatPath: path.join(hudHome, 'heartbeat.json'),
    probeCachePath: path.join(hudHome, 'probe-cache.json'),
  };
}

export function deriveTaskMdPath(transcriptPath) {
  if (typeof transcriptPath !== 'string' || !path.isAbsolute(transcriptPath)) return null;
  const logsDir = path.dirname(transcriptPath);
  const sysGenDir = path.dirname(logsDir);
  if (path.basename(logsDir) === 'logs' && path.basename(sysGenDir) === '.system_generated') {
    return path.join(path.dirname(sysGenDir), 'task.md');
  }
  return path.join(logsDir, 'task.md');
}

export function prepareDataDir(paths) {
  try {
    if (fs.lstatSync(paths.hudHome, { throwIfNoEntry: false })?.isSymbolicLink()) {
      return null;
    }
    fs.mkdirSync(paths.hudHome, { recursive: true, mode: 0o700 });
    if (fs.lstatSync(paths.dataDir, { throwIfNoEntry: false })?.isSymbolicLink()) {
      fs.rmSync(paths.dataDir, { force: true });
    }
    fs.mkdirSync(paths.dataDir, { recursive: true, mode: 0o700 });

    // Remove any symlinks inside dataDir so Node --allow-fs-write cannot follow a planted symlink
    const sweep = (dir) => {
      for (const ent of fs.readdirSync(dir, { withFileTypes: true })) {
        const full = path.join(dir, ent.name);
        if (ent.isSymbolicLink()) fs.rmSync(full, { force: true });
        else if (ent.isDirectory()) sweep(full);
      }
    };
    sweep(paths.dataDir);
    return fs.realpathSync(paths.dataDir);
  } catch {
    return null;
  }
}

function realpathOrSelf(p) {
  if (typeof p !== 'string' || !p) return null;
  try {
    if (fs.existsSync(p)) return fs.realpathSync(p);
    const parent = path.dirname(p);
    if (fs.existsSync(parent)) {
      return path.join(fs.realpathSync(parent), path.basename(p));
    }
    return p;
  } catch {
    return p;
  }
}

export function logicPaths(paths, stdin, dataRealDir) {
  const abs = (v) => (typeof v === 'string' && path.isAbsolute(v) ? realpathOrSelf(v) : null);
  const cwd =
    abs(stdin?.workspace?.project_dir) ||
    abs(stdin?.workspace?.current_dir) ||
    abs(stdin?.cwd);
  const transcript = abs(stdin?.transcript_path);
  const taskMd = realpathOrSelf(deriveTaskMdPath(transcript));

  return {
    data: dataRealDir,
    config: realpathOrSelf(paths.configPath),
    undo: realpathOrSelf(paths.undoPath),
    heartbeat: realpathOrSelf(paths.heartbeatPath),
    lastSnapshot: dataRealDir ? path.join(dataRealDir, 'last-snapshot.json') : null,
    cliSettings: realpathOrSelf(paths.cliSettingsPath),
    transcript,
    taskMd,
    cwd,
  };
}

export function buildReadGrants(pluginRoot, paths, named, strippedDir) {
  const candidates = [
    pluginRoot,
    paths.hudHome,
    strippedDir,
    named.data,
    named.config,
    named.heartbeat,
    named.lastSnapshot,
    named.cliSettings,
    named.transcript,
    named.taskMd,
  ].filter(Boolean);

  const resolved = new Set();
  for (const item of candidates) {
    try {
      if (fs.existsSync(item)) {
        resolved.add(fs.realpathSync(item));
      }
    } catch {
      // ignore missing path
    }
  }
  return [...resolved];
}

function readJsonSafeObject(filePath, maxBytes = 64 * 1024) {
  try {
    if (!filePath || !fs.existsSync(filePath)) return null;
    const st = fs.lstatSync(filePath);
    if (!st.isFile() || st.size > maxBytes) return null;
    const raw = JSON.parse(fs.readFileSync(filePath, 'utf8').replace(/^\uFEFF/, ''));
    return raw && typeof raw === 'object' && !Array.isArray(raw) ? raw : null;
  } catch {
    return null;
  }
}

function stripTsSource(source) {
  const origWarn = process.emitWarning;
  process.emitWarning = () => {};
  try {
    return nodeModule.stripTypeScriptTypes(source);
  } finally {
    process.emitWarning = origWarn;
  }
}

/**
 * Strips TypeScript types from `src/*.ts` into `strippedDir`.
 * Uses `stripped/manifest.json` keyed on `(nodeVersion, mtimeMs, size)` so warm renders skip
 * reading and SHA-256 hashing unchanged `.ts` source files.
 */
export function stripLogicFiles(strippedDir, pluginRoot) {
  try {
    if (fs.lstatSync(strippedDir, { throwIfNoEntry: false })?.isSymbolicLink()) {
      fs.rmSync(strippedDir, { force: true });
    }
    fs.mkdirSync(strippedDir, { recursive: true, mode: 0o700 });
    const rootReal = fs.realpathSync(pluginRoot);
    const rootUrl = pathToFileURL(path.join(rootReal, path.sep)).href;
    const srcDir = path.join(rootReal, 'src');
    const manifestPath = path.join(strippedDir, 'manifest.json');

    const prevManifest = readJsonSafeObject(manifestPath);
    const prevEntries =
      prevManifest?.nodeVersion === process.version &&
      prevManifest.entries &&
      typeof prevManifest.entries === 'object'
        ? prevManifest.entries
        : {};

    const nextEntries = {};
    const files = {};
    let anyChanged = prevManifest?.nodeVersion !== process.version;

    for (const rel of fs.readdirSync(srcDir, { recursive: true })) {
      if (typeof rel !== 'string' || !rel.endsWith('.ts')) continue;
      const fullPath = path.join(srcDir, rel);
      const st = fs.statSync(fullPath);
      const urlRel = pathToFileURL(fullPath).href.slice(rootUrl.length);
      const prev = prevEntries[urlRel];

      if (
        prev &&
        typeof prev.cacheName === 'string' &&
        prev.mtimeMs === st.mtimeMs &&
        prev.size === st.size &&
        fs.existsSync(path.join(strippedDir, prev.cacheName))
      ) {
        files[urlRel] = prev.cacheName;
        nextEntries[urlRel] = prev;
        continue;
      }

      anyChanged = true;
      const srcText = fs.readFileSync(fullPath, 'utf8');
      const cacheName = `${crypto.hash('sha256', `${process.version}\0${srcText}`)}.js`;
      files[urlRel] = cacheName;
      nextEntries[urlRel] = { mtimeMs: st.mtimeMs, size: st.size, cacheName };

      const dest = path.join(strippedDir, cacheName);
      if (!fs.existsSync(dest)) {
        const tmp = `${dest}.${process.pid}.tmp`;
        fs.writeFileSync(tmp, stripTsSource(srcText), { mode: 0o600 });
        fs.renameSync(tmp, dest);
      }
    }

    if (!anyChanged && Object.keys(prevEntries).length !== Object.keys(nextEntries).length) {
      anyChanged = true;
    }

    if (anyChanged) {
      const manifestTmp = `${manifestPath}.${process.pid}.tmp`;
      fs.writeFileSync(
        manifestTmp,
        JSON.stringify({ nodeVersion: process.version, entries: nextEntries }),
        { mode: 0o600 },
      );
      fs.renameSync(manifestTmp, manifestPath);

      const activeSet = new Set(Object.values(files));
      for (const existing of fs.readdirSync(strippedDir)) {
        if (existing === 'manifest.json' || existing.endsWith('.tmp')) continue;
        if (!activeSet.has(existing)) {
          fs.rmSync(path.join(strippedDir, existing), { force: true });
        }
      }
    }

    return { dir: fs.realpathSync(strippedDir), files, cacheHit: !anyChanged };
  } catch {
    return null;
  }
}

// ─── Multi-VCS Repository Detection & mtime Fingerprinting ────────────────────

function resolveGitWorktreeDirs(repoDir, dotGitFile) {
  try {
    const raw = fs.readFileSync(dotGitFile, 'utf8').trim();
    const match = /^gitdir:\s*(.+)$/i.exec(raw);
    if (!match) return null;
    const gitDir = realpathOrSelf(path.resolve(repoDir, match[1].trim()));
    let commonDir = gitDir;
    const commondirFile = path.join(gitDir, 'commondir');
    if (fs.existsSync(commondirFile)) {
      const relCommon = fs.readFileSync(commondirFile, 'utf8').trim();
      if (relCommon) commonDir = realpathOrSelf(path.resolve(gitDir, relCommon));
    }
    return { gitDir, commonDir };
  } catch {
    return null;
  }
}

/**
 * Walks upward from `cwd` (up to 64 levels) to locate the nearest enclosing VCS root.
 * Checks `.jj` before `.git` at the same level so colocated Jujutsu repos resolve as `jj`,
 * and stops at the first repo boundary so nested Git repos never resolve to an outer `.jj`/`.hg`.
 */
export function detectVcsRepo(cwd) {
  if (typeof cwd !== 'string' || !cwd) return null;
  let cur;
  try {
    const resolved = fs.realpathSync(cwd);
    if (!fs.lstatSync(resolved).isDirectory()) return null;
    cur = resolved;
  } catch {
    return null;
  }

  for (let depth = 0; cur && depth < 64; depth++) {
    const dotJj = path.join(cur, '.jj');
    const jjStat = fs.lstatSync(dotJj, { throwIfNoEntry: false });
    if (jjStat?.isDirectory()) {
      return { type: 'jj', root: cur, metaDir: dotJj };
    }

    const dotGit = path.join(cur, '.git');
    const gitStat = fs.lstatSync(dotGit, { throwIfNoEntry: false });
    if (gitStat?.isDirectory()) {
      return { type: 'git', root: cur, gitDir: dotGit, commonDir: dotGit };
    }
    if (gitStat?.isFile()) {
      const dirs = resolveGitWorktreeDirs(cur, dotGit);
      if (dirs) {
        return { type: 'git', root: cur, gitDir: dirs.gitDir, commonDir: dirs.commonDir };
      }
    }

    const dotHg = path.join(cur, '.hg');
    const hgStat = fs.lstatSync(dotHg, { throwIfNoEntry: false });
    if (hgStat?.isDirectory()) {
      return { type: 'hg', root: cur, metaDir: dotHg };
    }

    const parent = path.dirname(cur);
    cur = parent === cur ? null : parent;
  }
  return null;
}

function statToken(targetPath) {
  try {
    const st = fs.statSync(targetPath);
    return `${Math.trunc(st.mtimeMs)}:${st.size}`;
  } catch {
    return '-';
  }
}

/**
 * Computes a cheap O(1) stat fingerprint of VCS state files so commits, checkouts, staging,
 * jj operations, or hg updates immediately invalidate cached VCS status.
 */
export function computeVcsSignature(repo) {
  if (!repo) return '';
  if (repo.type === 'git') {
    const headPath = path.join(repo.gitDir, 'HEAD');
    let refToken = '-';
    try {
      const headText = fs.readFileSync(headPath, 'utf8').trim();
      if (headText.startsWith('ref: ')) {
        const refRel = headText.slice(5).trim();
        refToken = statToken(path.join(repo.commonDir, refRel));
      }
    } catch {
      // ignore missing HEAD
    }
    return [
      'git',
      statToken(headPath),
      statToken(path.join(repo.gitDir, 'index')),
      statToken(path.join(repo.commonDir, 'packed-refs')),
      refToken,
    ].join('|');
  }
  if (repo.type === 'jj') {
    return [
      'jj',
      statToken(path.join(repo.metaDir, 'working_copy', 'checkout')),
      statToken(path.join(repo.metaDir, 'repo', 'op_heads', 'heads')),
    ].join('|');
  }
  if (repo.type === 'hg') {
    return [
      'hg',
      statToken(path.join(repo.metaDir, 'dirstate')),
      statToken(path.join(repo.metaDir, 'branch')),
      statToken(path.join(repo.metaDir, 'bookmarks')),
    ].join('|');
  }
  return '';
}

// ─── Subprocess Execution & Hardened Git / Jujutsu / Mercurial Probes ─────────

export function execFileSafe(cmd, args, cwd, extraEnv = {}) {
  return new Promise((resolve) => {
    execFile(
      cmd,
      args,
      {
        cwd: cwd || undefined,
        timeout: PROBE_TIMEOUT_MS,
        maxBuffer: PROBE_MAX_BUFFER,
        windowsHide: true,
        shell: false,
        env: {
          PATH: process.env.PATH || '',
          HOME: process.env.HOME || '',
          GIT_OPTIONAL_LOCKS: '0',
          GIT_TERMINAL_PROMPT: '0',
          GCM_INTERACTIVE: 'Never',
          GIT_NO_LAZY_FETCH: '1',
          HGPLAIN: '1',
          LC_ALL: 'C',
          ...extraEnv,
        },
      },
      (err, stdout) => {
        if (err) resolve(null);
        else resolve(stdout);
      },
    );
  });
}

export const GIT_SAFE_PREFIX = [
  '-c',
  'core.quotePath=false',
  '--no-optional-locks',
  '-c',
  'core.fsmonitor=false',
  '-c',
  'protocol.allow=never',
];

export async function probeGit(cwd, runner = execFileSafe) {
  const out = await runner(
    'git',
    [...GIT_SAFE_PREFIX, 'status', '--porcelain=v2', '--branch', '--ignore-submodules=dirty'],
    cwd,
  );
  if (!out) return null;

  let branch = '';
  let ahead = 0;
  let behind = 0;
  let modified = 0;
  let added = 0;
  let deleted = 0;
  let untracked = 0;
  let conflicts = 0;

  for (const line of out.split('\n')) {
    if (!line) continue;
    if (line.startsWith('# branch.head ')) {
      branch = line.slice('# branch.head '.length).trim();
    } else if (line.startsWith('# branch.ab ')) {
      const m = /\+(\d+)\s+-(\d+)/.exec(line);
      if (m) {
        ahead = Number(m[1]) || 0;
        behind = Number(m[2]) || 0;
      }
    } else if (line.startsWith('u ')) {
      conflicts++;
    } else if (line.startsWith('? ')) {
      untracked++;
    } else if (line.startsWith('1 ') || line.startsWith('2 ')) {
      const xy = line.slice(2, 4);
      if (xy.includes('A')) added++;
      else if (xy.includes('D')) deleted++;
      else modified++;
    }
  }

  if (branch === '(detached)') {
    const rev = await runner('git', [...GIT_SAFE_PREFIX, 'rev-parse', '--short', 'HEAD'], cwd);
    branch = rev ? `@${rev.trim()}` : 'detached';
  }
  if (!branch) return null;

  const dirty = modified + added + deleted + untracked + conflicts > 0;
  return JSON.stringify({
    type: 'git',
    branch,
    dirty,
    ahead,
    behind,
    modified,
    added,
    deleted,
    untracked,
    conflicts,
  });
}

// Strictly read-only `jj log`: `--ignore-working-copy` guarantees jj never snapshots/mutates the repo.
export const JJ_PROBE_ARGS = [
  '--ignore-working-copy',
  '--at-operation=@',
  '--no-pager',
  'log',
  '-r',
  '@',
  '--no-graph',
  '--color',
  'never',
  '-T',
  'change_id.shortest(8) ++ "\\x1f" ++ self.local_bookmarks().map(|b| b.name()).join("\\x1e") ++ "\\x1f" ++ if(self.empty(), "0", "1") ++ "\\x1f" ++ if(self.conflict(), "1", "0")',
];

export function parseJjProbeOutput(stdout) {
  if (typeof stdout !== 'string') return null;
  const trimmed = stdout.replace(/\r?\n$/, '');
  if (!trimmed || /[\r\n]/.test(trimmed)) return null;

  const parts = trimmed.split('\x1f');
  if (parts.length !== 4) return null;
  const [rawChangeId, rawBookmarks, nonEmptyFlag, conflictFlag] = parts;
  const changeId = rawChangeId.trim();
  if (!changeId || !['0', '1'].includes(nonEmptyFlag) || !['0', '1'].includes(conflictFlag)) {
    return null;
  }

  const bookmarks = rawBookmarks ? rawBookmarks.split('\x1e').map((b) => b.trim()).filter(Boolean) : [];
  const branch =
    bookmarks.length > 1
      ? `${bookmarks[0]}+${bookmarks.length - 1}`
      : bookmarks.length === 1
        ? bookmarks[0]
        : `@${changeId}`;

  return {
    type: 'jj',
    branch,
    dirty: nonEmptyFlag === '1',
    ahead: 0,
    behind: 0,
    modified: 0,
    added: 0,
    deleted: 0,
    untracked: 0,
    conflicts: conflictFlag === '1' ? 1 : 0,
  };
}

export async function probeJj(cwd, runner = execFileSafe) {
  const out = await runner('jj', JJ_PROBE_ARGS, cwd);
  const parsed = parseJjProbeOutput(out);
  return parsed ? JSON.stringify(parsed) : null;
}

export const HG_SAFE_PREFIX = ['--config', 'ui.interactive=False', '--config', 'extensions.fsmonitor=!'];

export function parseHgProbeOutput(branchOut, statusOut) {
  if (typeof branchOut !== 'string') return null;
  const branch = branchOut.trim().split(/\r?\n/)[0]?.trim() || '';
  if (!branch) return null;

  let modified = 0;
  let added = 0;
  let deleted = 0;
  let untracked = 0;

  if (typeof statusOut === 'string') {
    for (const rawLine of statusOut.split(/\r?\n/)) {
      if (rawLine.length < 2 || rawLine[1] !== ' ') continue;
      const code = rawLine[0];
      if (code === 'M') modified++;
      else if (code === 'A') added++;
      else if (code === 'R' || code === '!') deleted++;
      else if (code === '?') untracked++;
    }
  }

  const dirty = modified + added + deleted + untracked > 0;
  return {
    type: 'hg',
    branch,
    dirty,
    ahead: 0,
    behind: 0,
    modified,
    added,
    deleted,
    untracked,
    conflicts: 0,
  };
}

export async function probeHg(cwd, runner = execFileSafe) {
  const [branchOut, statusOut] = await Promise.all([
    runner('hg', [...HG_SAFE_PREFIX, 'branch'], cwd),
    runner('hg', [...HG_SAFE_PREFIX, 'status', '-mardu'], cwd),
  ]);
  const parsed = parseHgProbeOutput(branchOut, statusOut);
  return parsed ? JSON.stringify(parsed) : null;
}

export async function probeVcsUncached(repo, runner = execFileSafe) {
  if (!repo) return null;
  if (repo.type === 'jj') return probeJj(repo.root, runner);
  if (repo.type === 'hg') return probeHg(repo.root, runner);
  return probeGit(repo.root, runner);
}

export async function probeVcsCached({
  cwd,
  paths,
  ttlMs = DEFAULT_VCS_CACHE_TTL_MS,
  stdinVcs = null,
  now = Date.now(),
  runner = execFileSafe,
}) {
  const repo = detectVcsRepo(cwd);
  if (!repo) return null;

  const sig = computeVcsSignature(repo);
  const cacheFile = paths?.probeCachePath ?? null;

  if (ttlMs > 0 && cacheFile) {
    const cacheDoc = readJsonSafeObject(cacheFile, 128 * 1024);
    const entry = cacheDoc?.entries?.[repo.root];
    if (
      entry &&
      entry.sig === sig &&
      typeof entry.expiresAt === 'number' &&
      now < entry.expiresAt &&
      typeof entry.value === 'string'
    ) {
      const branchMatches =
        !stdinVcs?.branch || !entry.branch || stdinVcs.branch === entry.branch;
      const dirtyMatches =
        typeof stdinVcs?.dirty !== 'boolean' ||
        typeof entry.dirty !== 'boolean' ||
        stdinVcs.dirty === entry.dirty;
      if (branchMatches && dirtyMatches) {
        return entry.value;
      }
    }
  }

  const fresh = await probeVcsUncached(repo, runner);
  if (fresh && ttlMs > 0 && cacheFile) {
    try {
      fs.mkdirSync(path.dirname(cacheFile), { recursive: true, mode: 0o700 });
      const prevDoc = readJsonSafeObject(cacheFile, 128 * 1024);
      const prevEntries =
        prevDoc?.entries && typeof prevDoc.entries === 'object' ? prevDoc.entries : {};
      const parsedFresh = JSON.parse(fresh);

      const merged = {
        ...prevEntries,
        [repo.root]: {
          sig,
          at: now,
          expiresAt: now + ttlMs,
          branch: parsedFresh.branch,
          dirty: parsedFresh.dirty,
          value: fresh,
        },
      };
      // Bound cache size to most recent MAX_CACHED_WORKSPACES workspaces
      const sortedKeys = Object.keys(merged).sort(
        (a, b) => (merged[b]?.at ?? 0) - (merged[a]?.at ?? 0),
      );
      const bounded = {};
      for (const k of sortedKeys.slice(0, MAX_CACHED_WORKSPACES)) {
        bounded[k] = merged[k];
      }

      const tmp = `${cacheFile}.${process.pid}.tmp`;
      fs.writeFileSync(tmp, JSON.stringify({ entries: bounded }), { mode: 0o600 });
      fs.renameSync(tmp, cacheFile);
    } catch {
      // non-fatal on read-only filesystem
    }
  }
  return fresh;
}

async function probeMemory(runner = execFileSafe) {
  const totalBytes = os.totalmem();
  if (process.platform === 'darwin') {
    const vmOut = await runner('vm_stat', [], undefined);
    if (vmOut) {
      const pageMatch = /page size of (\d+) bytes/i.exec(vmOut);
      const pageSize = pageMatch ? Number(pageMatch[1]) : 16384;
      const readPages = (label) => {
        const m = new RegExp(`${label}:\\s+(\\d+)`, 'i').exec(vmOut);
        return m ? Number(m[1]) : 0;
      };
      const usedPages =
        readPages('Pages active') +
        readPages('Pages wired down') +
        readPages('Pages occupied by compressor');
      if (usedPages > 0) {
        const usedBytes = Math.min(totalBytes, usedPages * pageSize);
        return JSON.stringify({
          totalBytes,
          usedBytes,
          usedPercent: Math.round((usedBytes / totalBytes) * 100),
        });
      }
    }
  }
  const freeBytes = os.freemem();
  const usedBytes = Math.max(0, totalBytes - freeBytes);
  return JSON.stringify({
    totalBytes,
    usedBytes,
    usedPercent: Math.round((usedBytes / totalBytes) * 100),
  });
}

function readFastProbePrefs(configPath) {
  const raw = readJsonSafeObject(configPath);
  const vcsObj = raw?.vcs && typeof raw.vcs === 'object' ? raw.vcs : {};
  const dispObj = raw?.display && typeof raw.display === 'object' ? raw.display : {};
  const vcsEnabled = vcsObj.enabled !== false;
  const cacheTtlMs =
    typeof vcsObj.cacheTtlMs === 'number' &&
    Number.isInteger(vcsObj.cacheTtlMs) &&
    vcsObj.cacheTtlMs >= 0 &&
    vcsObj.cacheTtlMs <= 60000
      ? vcsObj.cacheTtlMs
      : DEFAULT_VCS_CACHE_TTL_MS;
  const showMemory = dispObj.showMemory === true;
  return { vcsEnabled, cacheTtlMs, showMemory };
}

/**
 * Creates a deduplicated per-render probe session that can pre-warm `vcs` / `memory` concurrently
 * while the caged Node child process is booting.
 */
export function createProbeSession({
  paths,
  cwd,
  stdin = null,
  now = Date.now(),
  runner = execFileSafe,
}) {
  const prefs = readFastProbePrefs(paths?.configPath);
  const inflight = new Map();

  const runOne = (name) => {
    if (!inflight.has(name)) {
      if (name === 'vcs') {
        inflight.set(
          name,
          cwd && fs.existsSync(cwd)
            ? probeVcsCached({
                cwd,
                paths,
                ttlMs: prefs.cacheTtlMs,
                stdinVcs: stdin?.vcs ?? null,
                now,
                runner,
              })
            : Promise.resolve(null),
        );
      } else if (name === 'memory') {
        inflight.set(name, probeMemory(runner));
      } else {
        inflight.set(name, Promise.resolve(null));
      }
    }
    return inflight.get(name);
  };

  return {
    prewarm() {
      if (prefs.vcsEnabled && cwd) runOne('vcs');
      if (prefs.showMemory) runOne('memory');
    },
    async ask(requested) {
      const answers = {};
      const unique = [...new Set(Array.isArray(requested) ? requested : [])];
      await Promise.all(
        unique.map(async (name) => {
          answers[name] = await runOne(name);
        }),
      );
      return answers;
    },
  };
}

export async function executeProbes(requested, cwd, options = {}) {
  const session = createProbeSession({
    paths: options.paths ?? null,
    cwd,
    stdin: options.stdin ?? null,
    now: options.now ?? Date.now(),
    runner: options.runner ?? execFileSafe,
  });
  return session.ask(requested);
}

export function writeConfigWithUndo(paths, nextRawConfig) {
  fs.mkdirSync(paths.hudHome, { recursive: true, mode: 0o700 });
  if (fs.existsSync(paths.configPath)) {
    fs.copyFileSync(paths.configPath, paths.undoPath);
  } else {
    fs.writeFileSync(paths.undoPath, '{}\n', { mode: 0o600 });
  }
  const tmp = `${paths.configPath}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, `${JSON.stringify(nextRawConfig, null, 2)}\n`, { mode: 0o600 });
  fs.renameSync(tmp, paths.configPath);
}

export function undoConfig(paths) {
  if (!fs.existsSync(paths.undoPath)) {
    throw new Error('Nothing to undo (no previous config backup found).');
  }
  const tmp = `${paths.configPath}.${process.pid}.tmp`;
  fs.copyFileSync(paths.undoPath, tmp);
  fs.renameSync(tmp, paths.configPath);
  fs.rmSync(paths.undoPath, { force: true });
}

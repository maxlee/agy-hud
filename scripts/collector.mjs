// Capability layer: owns all paths, safe fixed child_process probes (git, jj, vm_stat),
// TypeScript stripping cache (`stripped/`), and symlink-safe `data/` preparation.
import { execFile } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import nodeModule from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const PROBE_TIMEOUT_MS = 1200;
const PROBE_MAX_BUFFER = 512 * 1024;

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
    return fs.existsSync(p) ? fs.realpathSync(p) : p;
  } catch {
    return p;
  }
}

export function logicPaths(paths, stdin, dataRealDir) {
  const abs = (v) => (typeof v === 'string' && path.isAbsolute(v) ? realpathOrSelf(v) : null);
  const cwd = abs(stdin?.workspace?.project_dir) || abs(stdin?.workspace?.current_dir) || abs(stdin?.cwd);
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

function stripTsSource(source) {
  const origWarn = process.emitWarning;
  process.emitWarning = () => {};
  try {
    return nodeModule.stripTypeScriptTypes(source);
  } finally {
    process.emitWarning = origWarn;
  }
}

export function stripLogicFiles(strippedDir, pluginRoot) {
  try {
    if (fs.lstatSync(strippedDir, { throwIfNoEntry: false })?.isSymbolicLink()) {
      fs.rmSync(strippedDir, { force: true });
    }
    fs.mkdirSync(strippedDir, { recursive: true, mode: 0o700 });
    const rootReal = fs.realpathSync(pluginRoot);
    const rootUrl = pathToFileURL(path.join(rootReal, path.sep)).href;
    const srcDir = path.join(rootReal, 'src');
    const files = {};

    for (const rel of fs.readdirSync(srcDir, { recursive: true })) {
      if (typeof rel !== 'string' || !rel.endsWith('.ts')) continue;
      const fullPath = path.join(srcDir, rel);
      const srcText = fs.readFileSync(fullPath, 'utf8');
      const cacheName = `${crypto.hash('sha256', `${process.version}\0${srcText}`)}.js`;
      const urlRel = pathToFileURL(fullPath).href.slice(rootUrl.length);
      files[urlRel] = cacheName;

      const dest = path.join(strippedDir, cacheName);
      if (!fs.existsSync(dest)) {
        const tmp = `${dest}.${process.pid}.tmp`;
        fs.writeFileSync(tmp, stripTsSource(srcText), { mode: 0o600 });
        fs.renameSync(tmp, dest);
      }
    }

    const activeSet = new Set(Object.values(files));
    for (const existing of fs.readdirSync(strippedDir)) {
      if (!activeSet.has(existing)) {
        fs.rmSync(path.join(strippedDir, existing), { force: true });
      }
    }

    return { dir: fs.realpathSync(strippedDir), files };
  } catch {
    return null;
  }
}

function execFileSafe(cmd, args, cwd) {
  return new Promise((resolve) => {
    execFile(
      cmd,
      args,
      {
        cwd: cwd || undefined,
        timeout: PROBE_TIMEOUT_MS,
        maxBuffer: PROBE_MAX_BUFFER,
        windowsHide: true,
        env: {
          PATH: process.env.PATH || '',
          HOME: process.env.HOME || '',
          GIT_OPTIONAL_LOCKS: '0',
          GIT_TERMINAL_PROMPT: '0',
          LC_ALL: 'C',
        },
      },
      (err, stdout) => {
        if (err) resolve(null);
        else resolve(stdout);
      },
    );
  });
}

async function probeGit(cwd) {
  const out = await execFileSafe(
    'git',
    ['--no-optional-locks', '-c', 'core.fsmonitor=false', 'status', '--porcelain=v2', '--branch'],
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
    const rev = await execFileSafe('git', ['--no-optional-locks', 'rev-parse', '--short', 'HEAD'], cwd);
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

async function probeMemory() {
  const totalBytes = os.totalmem();
  if (process.platform === 'darwin') {
    const vmOut = await execFileSafe('vm_stat', []);
    if (vmOut) {
      const pageMatch = /page size of (\d+) bytes/i.exec(vmOut);
      const pageSize = pageMatch ? Number(pageMatch[1]) : 16384;
      const readPages = (label) => {
        const m = new RegExp(`${label}:\\s+(\\d+)`, 'i').exec(vmOut);
        return m ? Number(m[1]) : 0;
      };
      const usedPages =
        readPages('Pages active') + readPages('Pages wired down') + readPages('Pages occupied by compressor');
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

export async function executeProbes(requested, cwd) {
  const answers = {};
  const tasks = [];
  if (requested.includes('vcs') && cwd && fs.existsSync(cwd)) {
    tasks.push(
      probeGit(cwd).then((res) => {
        answers.vcs = res;
      }),
    );
  }
  if (requested.includes('memory')) {
    tasks.push(
      probeMemory().then((res) => {
        answers.memory = res;
      }),
    );
  }
  await Promise.all(tasks);
  return answers;
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

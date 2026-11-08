#!/usr/bin/env node
// Main statusLine command & CLI entrypoint for agy-hud.
// Enforces THE LINE: reads stdin, runs caged TypeScript logic under `node --permission` + `fence.mjs`,
// answers named probe requests via `collector.mjs`, and filters all terminal output via `terminalFilter`.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import * as collector from './collector.mjs';
import { installStatusLine, uninstallStatusLine } from './install.mjs';

const SELF = fileURLToPath(import.meta.url);
const PLUGIN_ROOT = path.resolve(path.dirname(SELF), '..');
const DEFAULT_LOGIC_ENTRY = path.join(PLUGIN_ROOT, 'src', 'main.ts');
const FENCE_HREF = pathToFileURL(path.join(PLUGIN_ROOT, 'scripts', 'fence.mjs')).href;

const MAX_STDIN_BYTES = 256 * 1024;
const MAX_OUTPUT_BYTES = 64 * 1024;
const STDIN_TIMEOUT_MS = 220;
const CAGE_TIMEOUT_MS = 4200; // Fits safely inside agy CLI's 5000ms statusLine timeout

export function isHudDisabled(env = process.env) {
  const raw = env.AGY_HUD_DISABLE?.trim().toLowerCase();
  return Boolean(raw) && !['0', 'false', 'off', 'no'].includes(raw);
}

export function isNodeSupported(version = process.versions.node) {
  const [maj = 0, min = 0] = String(version)
    .split('.')
    .map((n) => Number.parseInt(n, 10));
  return maj > 22 || (maj === 22 && min >= 18);
}

// Terminal output allowlist: keeps SGR color sequences and safe `file:` OSC 8 links; strips everything else.
const ESCAPE_OR_CTRL =
  /\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)?|\x1b[ -/]*[0-~]?|[\x00-\x1f\x7f-\x9f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g;
const SGR_SEQ = /^\x1b\[[0-9;]*m$/;
const OSC8_SEQ = /^\x1b\]8;;(.*)\x1b\\$/s;

export function terminalFilter() {
  const isAllowedLink = (target) => {
    if (target === '') return true;
    try {
      const parsed = new URL(target);
      return parsed.protocol === 'file:';
    } catch {
      return false;
    }
  };
  return (line) =>
    String(line).replace(ESCAPE_OR_CTRL, (seq) => {
      if (SGR_SEQ.test(seq)) return seq;
      const m = OSC8_SEQ.exec(seq);
      if (m && isAllowedLink(m[1])) return seq;
      return '';
    });
}

export function readStdinPayload(stream = process.stdin) {
  if (stream.isTTY) return Promise.resolve(null);
  return new Promise((resolve) => {
    let raw = '';
    let settled = false;
    let idleTimer = null;

    const finish = (val) => {
      if (settled) return;
      settled = true;
      clearTimeout(noDataTimer);
      if (idleTimer) clearTimeout(idleTimer);
      try {
        stream.pause();
      } catch {
        // ignore
      }
      resolve(val);
    };

    const tryParse = () => {
      try {
        const obj = JSON.parse(raw);
        return obj && typeof obj === 'object' && !Array.isArray(obj) ? obj : null;
      } catch {
        return null;
      }
    };

    const noDataTimer = setTimeout(() => {
      if (!raw) finish(null);
    }, STDIN_TIMEOUT_MS);

    stream.setEncoding('utf8');
    stream.on('data', (chunk) => {
      raw += chunk;
      if (Buffer.byteLength(raw, 'utf8') > MAX_STDIN_BYTES) {
        finish(null);
        return;
      }
      const parsed = tryParse();
      if (parsed) {
        finish(parsed);
        return;
      }
      if (idleTimer) clearTimeout(idleTimer);
      idleTimer = setTimeout(() => finish(tryParse()), STDIN_TIMEOUT_MS);
    });
    stream.on('end', () => finish(tryParse()));
    stream.on('error', () => finish(null));
  });
}

const SAFE_ENV_KEYS = ['LANG', 'LC_ALL', 'LC_CTYPE', 'LC_TIME', 'TZ', 'NO_COLOR', 'FORCE_COLOR'];

function buildCageEnv(env, strippedInfo) {
  const out = { PATH: '' };
  for (const k of SAFE_ENV_KEYS) {
    if (typeof env[k] === 'string') out[k] = env[k];
  }
  if (strippedInfo) {
    out.AGY_HUD_STRIPPED = JSON.stringify({
      dir: path.join(strippedInfo.dir, path.sep),
      files: strippedInfo.files,
    });
  }
  return out;
}

function readPluginVersion() {
  try {
    const manifest = JSON.parse(fs.readFileSync(path.join(PLUGIN_ROOT, 'plugin.json'), 'utf8'));
    return typeof manifest.version === 'string' ? manifest.version : '0.1.0';
  } catch {
    return '0.1.0';
  }
}

function readHeartbeat(heartbeatPath) {
  try {
    if (!fs.existsSync(heartbeatPath)) return null;
    const hb = JSON.parse(fs.readFileSync(heartbeatPath, 'utf8'));
    return hb && typeof hb === 'object' ? hb : null;
  } catch {
    return null;
  }
}

function writeHeartbeat(heartbeatPath, info) {
  try {
    fs.mkdirSync(path.dirname(heartbeatPath), { recursive: true, mode: 0o700 });
    const tmp = `${heartbeatPath}.${process.pid}.tmp`;
    fs.writeFileSync(tmp, JSON.stringify(info), { mode: 0o600 });
    fs.renameSync(tmp, heartbeatPath);
  } catch {
    // non-fatal
  }
}

export async function runCage({
  mode = 'render',
  stdin = null,
  now = Date.now(),
  columns = null,
  command = undefined,
  env = process.env,
  entry = DEFAULT_LOGIC_ENTRY,
  extraReadGrants = [],
  probesRunner = null,
}) {
  const paths = collector.agyPaths(env);
  let dataRealDir = collector.prepareDataDir(paths);
  const rootReal = fs.realpathSync(PLUGIN_ROOT);
  // Never grant write inside plugin root
  if (dataRealDir) {
    const rel = path.relative(rootReal, dataRealDir);
    if (!rel.startsWith('..') && !path.isAbsolute(rel)) {
      dataRealDir = null;
    }
  }

  const stripped = collector.stripLogicFiles(paths.strippedDir, PLUGIN_ROOT);
  const named = collector.logicPaths(paths, stdin, dataRealDir);
  const grants = [
    ...new Set([
      ...collector.buildReadGrants(rootReal, paths, named, stripped?.dir),
      ...extraReadGrants.filter((g) => fs.existsSync(g)).map((g) => fs.realpathSync(g)),
    ]),
  ];

  // Start VCS/memory probes concurrently with caged Node startup when rendering live statusLine
  const probeSession =
    probesRunner === null
      ? collector.createProbeSession({ paths, cwd: named.cwd, stdin, now })
      : null;
  if (mode === 'render' && probeSession) {
    probeSession.prewarm();
  }
  const runProbes =
    probesRunner ?? ((requested) => probeSession.ask(requested));

  const nodeArgs = [
    '--no-warnings',
    '--permission',
    ...grants.map((g) => `--allow-fs-read=${g}`),
    ...(dataRealDir ? [`--allow-fs-write=${dataRealDir}`] : []),
    '--disallow-code-generation-from-strings',
    `--import=${FENCE_HREF}`,
    entry,
  ];

  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, nodeArgs, {
      env: buildCageEnv(env, stripped),
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    });

    let settled = false;
    let stderrBuf = '';
    let stdoutBuf = '';
    let outBytes = 0;

    const settle = (err, val) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try {
        child.stdin.end();
      } catch {
        // ignore
      }
      if (err) {
        child.kill('SIGKILL');
        reject(err);
      } else {
        resolve({ output: val, paths });
      }
    };

    const timer = setTimeout(() => settle(new Error('agy-hud cage timed out')), CAGE_TIMEOUT_MS);

    const sendLine = (msg) => {
      if (!settled) {
        child.stdin.write(`${JSON.stringify(msg)}\n`);
      }
    };

    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (chunk) => {
      if (stderrBuf.length < 8192) stderrBuf += chunk;
    });

    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      outBytes += Buffer.byteLength(chunk, 'utf8');
      if (outBytes > MAX_OUTPUT_BYTES) {
        settle(new Error('agy-hud cage exceeded max output bytes'));
        return;
      }
      stdoutBuf += chunk;
      let nl = stdoutBuf.indexOf('\n');
      while (nl !== -1) {
        const line = stdoutBuf.slice(0, nl).trim();
        stdoutBuf = stdoutBuf.slice(nl + 1);
        if (line) {
          let msg;
          try {
            msg = JSON.parse(line);
          } catch {
            settle(new Error('Invalid NDJSON from caged logic'));
            return;
          }
          if (Array.isArray(msg.ask) && typeof msg.id === 'number') {
            Promise.resolve(runProbes(msg.ask, named.cwd))
              .then((answers) => sendLine({ id: msg.id, answers }))
              .catch(() => sendLine({ id: msg.id, answers: {} }));
          } else if (msg.result) {
            settle(null, msg.result);
            return;
          }
        }
        nl = stdoutBuf.indexOf('\n');
      }
    });

    child.on('error', (err) => settle(err));
    child.on('close', (code) => {
      if (!settled) {
        settle(
          new Error(
            `Caged logic exited prematurely (code ${code})${stderrBuf ? `: ${stderrBuf.trim()}` : ''}`,
          ),
        );
      }
    });

    sendLine({
      mode,
      stdin,
      now,
      columns,
      paths: named,
      command,
    });
  });
}

function printFiltered(lines) {
  if (!Array.isArray(lines) || lines.length === 0) return;
  const filter = terminalFilter();
  process.stdout.write(`${lines.map((l) => filter(l)).join('\n')}\n`);
}

const HELP_TEXT = [
  'agy-hud — Real-time statusline HUD for Google Antigravity CLI (agy)',
  '',
  'Usage:',
  '  node scripts/hud.mjs                       (statusLine mode: reads JSON from stdin)',
  '  node scripts/hud.mjs doctor                Check setup, runtime, settings.json, and health',
  '  node scripts/hud.mjs install [--replace]   Configure statusLine in ~/.gemini/antigravity-cli/settings.json',
  '  node scripts/hud.mjs uninstall             Remove agy-hud statusLine (or restore previous script)',
  '  node scripts/hud.mjs preview [options]     Render live previews (--state showcase|idle|tool_use|confirm|high-context, --width N, --set k=v, --last)',
  '  node scripts/hud.mjs schema [query]        List configurable HUD keys and descriptions',
  '  node scripts/hud.mjs set <k=v...>|--undo   Update or undo HUD configuration',
  '  node scripts/hud.mjs report [--note text]  Generate a privacy-scrubbed diagnostic report',
];

export async function main(argv = process.argv.slice(2), env = process.env) {
  const paths = collector.agyPaths(env);
  const sub = argv[0];

  if (sub === 'help' || sub === '--help' || sub === '-h') {
    printFiltered(HELP_TEXT);
    return 0;
  }

  if (!isNodeSupported()) {
    const msg = `agy-hud requires Node.js 22.18+ (current: ${process.versions.node}). Run /hud to fix.`;
    writeHeartbeat(paths.heartbeatPath, {
      at: Date.now(),
      runtime: process.versions.node,
      path: process.execPath,
      outcome: 'upgrade',
      columns: null,
    });
    printFiltered([msg]);
    return sub ? 1 : 0;
  }

  if (sub === 'install') {
    try {
      const report = installStatusLine({
        paths,
        pluginRoot: PLUGIN_ROOT,
        argv: argv.slice(1),
        env,
      });
      printFiltered(JSON.stringify(report, null, 2).split('\n'));
      return 0;
    } catch (err) {
      printFiltered([`install failed: ${err instanceof Error ? err.message : String(err)}`]);
      return 1;
    }
  }

  if (sub === 'uninstall') {
    try {
      const report = uninstallStatusLine({ paths });
      printFiltered(JSON.stringify(report, null, 2).split('\n'));
      return 0;
    } catch (err) {
      printFiltered([`uninstall failed: ${err instanceof Error ? err.message : String(err)}`]);
      return 1;
    }
  }

  if (sub === 'set' && argv.includes('--undo')) {
    try {
      collector.undoConfig(paths);
      printFiltered([`Reverted ${paths.configPath} to previous configuration.`]);
      return 0;
    } catch (err) {
      printFiltered([err instanceof Error ? err.message : String(err)]);
      return 1;
    }
  }

  if (sub) {
    const facts = {
      cwd: process.cwd(),
      runtime: {
        version: process.versions.node,
        execPath: process.execPath,
        supported: true,
      },
      plugin: {
        root: PLUGIN_ROOT,
        version: readPluginVersion(),
      },
      configPath: paths.configPath,
      heartbeat: readHeartbeat(paths.heartbeatPath),
    };
    const { output } = await runCage({
      mode: 'command',
      now: Date.now(),
      columns: process.stdout.columns || 100,
      command: { name: sub, args: argv.slice(1), facts },
      env,
    });
    if (output.configToWrite) {
      collector.writeConfigWithUndo(paths, output.configToWrite);
    }
    printFiltered(output.lines);
    return output.failed ? 1 : 0;
  }

  // Status line render mode
  if (isHudDisabled(env)) {
    return 0;
  }

  const startedAt = Date.now();
  const stdinPayload = await readStdinPayload();
  if (!stdinPayload) {
    // Manual bare invocation in a TTY without stdin: show helpful one-liner
    if (process.stdin.isTTY) {
      printFiltered(['agy-hud: pipe Antigravity CLI statusLine JSON on stdin, or run `node scripts/hud.mjs preview --state showcase`.']);
    }
    return 0;
  }

  try {
    const columns =
      typeof stdinPayload.terminal_width === 'number' && stdinPayload.terminal_width > 0
        ? stdinPayload.terminal_width
        : null;
    const { output } = await runCage({
      mode: 'render',
      stdin: stdinPayload,
      now: startedAt,
      columns,
      env,
    });
    writeHeartbeat(paths.heartbeatPath, {
      at: startedAt,
      runtime: process.versions.node,
      path: process.execPath,
      outcome: output.failed ? 'error' : 'ok',
      columns,
      durationMs: Date.now() - startedAt,
    });
    printFiltered(output.lines);
    return 0;
  } catch (err) {
    writeHeartbeat(paths.heartbeatPath, {
      at: startedAt,
      runtime: process.versions.node,
      path: process.execPath,
      outcome: err instanceof Error ? err.message : 'crash',
      columns: null,
      durationMs: Date.now() - startedAt,
    });
    return 0;
  }
}

const isMain = (() => {
  if (!process.argv[1]) return false;
  try {
    return fs.realpathSync(process.argv[1]) === fs.realpathSync(SELF);
  } catch {
    return path.resolve(process.argv[1]) === SELF;
  }
})();

if (isMain) {
  main().then((code) => {
    process.exitCode = code;
  });
}

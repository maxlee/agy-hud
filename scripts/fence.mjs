// Loaded via `--import` into the caged logic process BEFORE any `src/*.ts` code executes.
// Enforces an allowlist on Node builtins + seals network/process/socket escape hatches.
import { Console } from 'node:console';
import fs from 'node:fs';
import { registerHooks } from 'node:module';

const table = (entries) => Object.freeze(Object.assign(Object.create(null), entries));

const ALLOWED_BUILTINS = table({
  'node:path': 1,
  'node:fs': 1,
  'node:fs/promises': 1,
  'node:os': 1,
  'node:url': 1,
  'node:crypto': 1,
});

// `fence.mjs` lives in `<pluginRoot>/scripts/fence.mjs`, so plugin root is `../`
const PLUGIN_ROOT_URL = new URL('../', import.meta.url).href;
const Err = Error;
const slice = Function.prototype.call.bind(String.prototype.slice);
const startsWith = (s, prefix) =>
  typeof s === 'string' && s.length >= prefix.length && slice(s, 0, prefix.length) === prefix;

const isAllowedBuiltin = (id) =>
  ALLOWED_BUILTINS[startsWith(id, 'node:') ? id : `node:${id}`] === 1;

const strippedCache = (() => {
  try {
    const { dir, files } = JSON.parse(process.env.AGY_HUD_STRIPPED || '{}');
    return {
      dir: typeof dir === 'string' ? dir : '',
      files: files && typeof files === 'object' ? table(files) : table({}),
    };
  } catch {
    return { dir: '', files: table({}) };
  }
})();

const { readFileSync } = fs;

registerHooks({
  resolve(specifier, context, nextResolve) {
    const resolved = nextResolve(specifier, context);
    if (startsWith(resolved.url, 'node:')) {
      if (!isAllowedBuiltin(resolved.url)) {
        throw new Err(`fence: builtin ${resolved.url} is blocked inside agy-hud logic cage`);
      }
      return resolved;
    }
    if (!startsWith(resolved.url, PLUGIN_ROOT_URL)) {
      throw new Err(`fence: external module ${resolved.url} is blocked inside agy-hud logic cage`);
    }
    return resolved;
  },
  load(url, context, nextLoad) {
    if (strippedCache.dir && startsWith(url, PLUGIN_ROOT_URL)) {
      const rel = slice(url, PLUGIN_ROOT_URL.length);
      const cachedFile = strippedCache.files[rel];
      if (typeof cachedFile === 'string') {
        try {
          return {
            format: 'module',
            source: readFileSync(`${strippedCache.dir}${cachedFile}`, 'utf8'),
            shortCircuit: true,
          };
        } catch {
          // Fall back to Node's built-in loader if cache entry disappeared concurrently
        }
      }
    }
    return nextLoad(url, context);
  },
});

const seal = (obj, key, value) => {
  try {
    delete obj[key];
  } catch {
    // ignore non-configurable
  }
  try {
    Object.defineProperty(obj, key, {
      value,
      writable: false,
      configurable: false,
      enumerable: false,
    });
  } catch {
    // already sealed
  }
};

const origGetBuiltin = process.getBuiltinModule;
if (typeof origGetBuiltin === 'function') {
  seal(process, 'getBuiltinModule', (id) => {
    if (!isAllowedBuiltin(id)) {
      throw new Err(`fence: builtin ${id} is blocked inside agy-hud logic cage`);
    }
    return origGetBuiltin(id);
  });
}

for (const globalKey of ['fetch', 'WebSocket', 'EventSource', 'Navigator', 'navigator']) {
  seal(globalThis, globalKey, undefined);
}

for (const procKey of [
  'binding',
  '_linkedBinding',
  'dlopen',
  'kill',
  '_kill',
  '_debugProcess',
  'execve',
  'setuid',
  'setgid',
  'seteuid',
  'setegid',
  'initgroups',
  'setgroups',
  'chdir',
  'loadEnvFile',
  'openStdin',
  'channel',
]) {
  seal(process, procKey, undefined);
}

// Replace stdio net.Socket instances with minimal descriptor writers so net.Socket is unreachable
const fdWriter = (fd) =>
  Object.freeze({
    write(chunk) {
      try {
        fs.writeSync(fd, String(chunk));
      } catch {
        // reader closed
      }
      return true;
    },
    isTTY: false,
  });

const safeStdout = fdWriter(1);
const safeStderr = fdWriter(2);
seal(process, 'stdout', safeStdout);
seal(process, 'stderr', safeStderr);
seal(process, 'stdin', undefined);
seal(globalThis, 'console', new Console({ stdout: safeStdout, stderr: safeStderr, ignoreErrors: false }));

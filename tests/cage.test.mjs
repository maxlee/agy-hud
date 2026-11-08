import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { runCage } from '../scripts/hud.mjs';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

test('cage blocks child_process, net, http, fetch, eval, ungranted reads, and writes outside data/', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-hud-cage-'));
  // Place temporary escape probe inside plugin's tests/ folder so module fence permits loading the probe itself,
  // then verify every outward capability inside the probe is denied!
  const probeFile = path.join(ROOT, 'tests', `.tmp-escape-probe-${process.pid}.mjs`);
  const secretOutsideFile = path.join(tmp, 'secret-outside.txt');
  fs.writeFileSync(secretOutsideFile, 'TOP_SECRET', 'utf8');

  const env = {
    ...process.env,
    AGY_CLI_DIR: path.join(tmp, 'cli'),
    AGY_HUD_HOME: path.join(tmp, 'hud'),
  };

  fs.writeFileSync(
    probeFile,
    `
import fs from 'node:fs';
import path from 'node:path';

const checks = {};

// 1. Dynamic import of child_process
try {
  await import('node:child_process');
  checks.childProcess = 'ESCAPED';
} catch {
  checks.childProcess = 'BLOCKED';
}

// 2. Dynamic import of net
try {
  await import('node:net');
  checks.net = 'ESCAPED';
} catch {
  checks.net = 'BLOCKED';
}

// 3. process.getBuiltinModule('node:http')
try {
  process.getBuiltinModule('node:http');
  checks.builtinHttp = 'ESCAPED';
} catch {
  checks.builtinHttp = 'BLOCKED';
}

// 4. Sealed globals & process bindings
checks.fetchUndefined = globalThis.fetch === undefined;
checks.webSocketUndefined = globalThis.WebSocket === undefined;
checks.bindingUndefined = process.binding === undefined;
checks.dlopenUndefined = process.dlopen === undefined;
checks.stdinSealed = process.stdin === undefined;

// 5. Dynamic code generation from strings (new Function / eval)
try {
  const fn = new Function('return 42');
  fn();
  checks.evalCodegen = 'ESCAPED';
} catch {
  checks.evalCodegen = 'BLOCKED';
}

// 6. Ungranted file read
try {
  fs.readFileSync(${JSON.stringify(secretOutsideFile)}, 'utf8');
  checks.ungrantedRead = 'ESCAPED';
} catch {
  checks.ungrantedRead = 'BLOCKED';
}

// 7. Write outside data/ (into plugin root)
try {
  fs.writeFileSync(${JSON.stringify(path.join(ROOT, 'pwned.txt'))}, 'nope');
  checks.writePluginRoot = 'ESCAPED';
} catch {
  checks.writePluginRoot = 'BLOCKED';
}

// Read initial message from fd 0 so we also test that writing inside granted paths.data succeeds
const buf = Buffer.alloc(16384);
fs.read(0, buf, 0, buf.length, null, (_err, n) => {
  const msg = JSON.parse(buf.subarray(0, n).toString('utf8').split('\\n')[0]);
  try {
    fs.writeFileSync(path.join(msg.paths.data, 'allowed.txt'), 'ok');
    checks.writeDataDir = 'ALLOWED';
  } catch (e) {
    checks.writeDataDir = 'FAILED:' + e.message;
  }
  const out = Buffer.from(JSON.stringify({ result: { lines: [JSON.stringify(checks)] } }) + '\\n');
  fs.writeSync(1, out);
});
`,
    'utf8',
  );

  try {
    const { output } = await runCage({
      mode: 'render',
      stdin: { cwd: ROOT },
      entry: probeFile,
      env,
    });
    const checks = JSON.parse(output.lines[0]);
    assert.equal(checks.childProcess, 'BLOCKED');
    assert.equal(checks.net, 'BLOCKED');
    assert.equal(checks.builtinHttp, 'BLOCKED');
    assert.equal(checks.fetchUndefined, true);
    assert.equal(checks.webSocketUndefined, true);
    assert.equal(checks.bindingUndefined, true);
    assert.equal(checks.dlopenUndefined, true);
    assert.equal(checks.stdinSealed, true);
    assert.equal(checks.evalCodegen, 'BLOCKED');
    assert.equal(checks.ungrantedRead, 'BLOCKED');
    assert.equal(checks.writePluginRoot, 'BLOCKED');
    assert.equal(checks.writeDataDir, 'ALLOWED');
  } finally {
    fs.rmSync(probeFile, { force: true });
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

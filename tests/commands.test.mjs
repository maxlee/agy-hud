import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const HUD_BIN = path.join(ROOT, 'scripts', 'hud.mjs');

function runHudCli(args, env, input = undefined) {
  return execFileSync(process.execPath, ['--no-warnings', HUD_BIN, ...args], {
    env,
    input,
    encoding: 'utf8',
  });
}

test('install, --replace guard, uninstall restore, doctor, set/--undo, preview --last, and scrubbed report', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-hud-cmd-'));
  const cliDir = path.join(tmp, 'antigravity-cli');
  const hudHome = path.join(cliDir, 'agy-hud');
  const settingsPath = path.join(cliDir, 'settings.json');
  const env = {
    ...process.env,
    AGY_CLI_DIR: cliDir,
    AGY_HUD_HOME: hudHome,
  };

  try {
    fs.mkdirSync(cliDir, { recursive: true });
    // Seed foreign statusLine first to test --replace protection
    fs.writeFileSync(
      settingsPath,
      JSON.stringify({
        colorScheme: 'dark',
        statusLine: { type: 'command', command: '/usr/local/bin/custom-bar.sh --token=secret123' },
      }),
      'utf8',
    );

    // 1. doctor detects foreign statusLine and redacts secret token
    const doc1 = JSON.parse(runHudCli(['doctor'], env));
    assert.equal(doc1.statusLine.kind, 'other');
    assert.ok(!doc1.statusLine.command.includes('secret123'));
    assert.ok(doc1.statusLine.command.includes('[REDACTED]'));

    // 2. install without --replace refuses to overwrite foreign statusLine
    assert.throws(() => runHudCli(['install'], env));

    // 3. install --replace backs up settings.json and installs agy-hud
    const inst = JSON.parse(runHudCli(['install', '--replace', '--stack'], env));
    assert.equal(inst.installed, true);
    assert.equal(inst.replaced, 'other');
    assert.ok(fs.existsSync(inst.backupPath));

    const updatedSettings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
    assert.equal(updatedSettings.colorScheme, 'dark');
    assert.equal(updatedSettings.statusLine.stack_with_default, true);
    assert.ok(updatedSettings.statusLine.command.includes('statusline.mjs'));

    // 4. Pipe a live render to populate heartbeat.json and last-snapshot.json
    const liveOut = runHudCli(
      [],
      env,
      JSON.stringify({
        cwd: '/Users/alice/secret-repo',
        conversation_id: 'abcd-1234',
        conversation_title: 'Top secret algorithm',
        email: 'alice@corp.internal',
        model: { id: 'Gemini 4 Argon', display_name: 'Gemini 4 Argon', effort: 'high' },
        context_window: { used_percentage: 33.3, context_window_size: 1_000_000 },
        agent_state: 'working',
        terminal_width: 100,
      }),
    );
    assert.ok(liveOut.includes('WORKING'));

    // 5. preview --last replays the recorded snapshot
    const replay = runHudCli(['preview', '--last'], env);
    assert.ok(replay.includes('WORKING'));

    // 6. report scrubs PII (email, cwd, conversation_id, conversation_title)
    const report = JSON.parse(runHudCli(['report', '--note', 'test case'], env));
    const reportStr = JSON.stringify(report);
    assert.ok(!reportStr.includes('alice@corp.internal'));
    assert.ok(!reportStr.includes('secret-repo'));
    assert.ok(!reportStr.includes('Top secret algorithm'));
    assert.ok(!reportStr.includes('abcd-1234'));
    assert.equal(report.scrubbedStdin.email, 'user@example.com');

    // 7. set & set --undo
    const setOut = runHudCli(['set', 'language=zh-Hans', 'display.barLength=16'], env);
    assert.ok(setOut.includes('language = "zh-Hans"'));
    const cfgAfterSet = JSON.parse(fs.readFileSync(path.join(hudHome, 'config.json'), 'utf8'));
    assert.equal(cfgAfterSet.language, 'zh-Hans');
    assert.equal(cfgAfterSet.display.barLength, 16);

    runHudCli(['set', '--undo'], env);
    const cfgAfterUndo = JSON.parse(fs.readFileSync(path.join(hudHome, 'config.json'), 'utf8'));
    assert.equal(cfgAfterUndo.language, undefined);

    // 8. uninstall restores previous custom-bar.sh statusLine cleanly
    const uninst = JSON.parse(runHudCli(['uninstall'], env));
    assert.equal(uninst.restoredPrevious, true);
    const restoredSettings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
    assert.ok(restoredSettings.statusLine.command.includes('custom-bar.sh'));
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

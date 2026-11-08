import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { isHudDisabled, runCage } from '../scripts/hud.mjs';
import { stripAnsi, textWidth } from '../src/ansi.ts';

function makeTempEnv() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-hud-render-'));
  return {
    dir,
    env: {
      ...process.env,
      AGY_CLI_DIR: path.join(dir, 'cli'),
      AGY_HUD_HOME: path.join(dir, 'hud'),
    },
  };
}

test('caged render adapts across ultra-wide, standard, and narrow widths without overflowing columns', async () => {
  const { dir, env } = makeTempEnv();
  try {
    const sampleStdin = {
      cwd: '/Users/dev/demo-app',
      workspace: { current_dir: '/Users/dev/demo-app', project_dir: '/Users/dev/demo-app' },
      model: { id: 'Gemini 4 Argon (Medium)', display_name: 'Gemini 4 Argon (Medium)', effort: 'medium' },
      context_window: {
        context_window_size: 1_048_576,
        used_percentage: 42.5,
        remaining_percentage: 57.5,
        current_usage: {
          input_tokens: 400_000,
          output_tokens: 5_000,
          cache_creation_input_tokens: 0,
          cache_read_input_tokens: 100_000,
        },
      },
      exceeds_200k_tokens: true,
      quota: {
        'gemini-weekly': { remaining_fraction: 0.75, reset_in_seconds: 86_400 },
      },
      agent_state: 'idle',
      cycle_mode: 'plan',
      vim: { mode: 'INSERT' },
      sandbox: { enabled: true },
      cost: { total_usd: 1.25, estimated: false },
    };

    const fakeProbes = async () => ({
      vcs: JSON.stringify({
        type: 'git',
        branch: 'main',
        dirty: true,
        ahead: 1,
        behind: 0,
        modified: 2,
        added: 0,
        deleted: 0,
        untracked: 0,
        conflicts: 0,
      }),
    });

    // 1. Ultra-wide 200 cols -> collapses to 1 line when idle without active tools/todos
    const wide = await runCage({
      mode: 'render',
      stdin: { ...sampleStdin, terminal_width: 200 },
      columns: 200,
      env,
      probesRunner: fakeProbes,
    });
    assert.equal(wide.output.lines.length, 1);
    const widePlain = stripAnsi(wide.output.lines[0]);
    assert.ok(widePlain.includes('READY'));
    assert.ok(widePlain.includes('[PLAN]'));
    assert.ok(widePlain.includes('[INSERT]'));
    assert.ok(widePlain.includes('200k+'));
    assert.ok(widePlain.includes('Quota(weekly)'));

    // 2. Narrow 62 cols -> never exceeds 62 display columns on any line
    const narrow = await runCage({
      mode: 'render',
      stdin: { ...sampleStdin, terminal_width: 62, tool_confirmation_pending: true },
      columns: 62,
      env,
      probesRunner: fakeProbes,
    });
    assert.ok(narrow.output.lines.length >= 2);
    assert.ok(stripAnsi(narrow.output.lines[0]).includes('CONFIRM?'));
    for (const line of narrow.output.lines) {
      assert.ok(textWidth(line) <= 62, `Line exceeded 62 cols (${textWidth(line)}): ${stripAnsi(line)}`);
    }

    // 3. Simplified Chinese rendering via config.json
    fs.writeFileSync(
      path.join(dir, 'hud', 'config.json'),
      JSON.stringify({ language: 'zh-Hans' }),
      'utf8',
    );
    const zh = await runCage({
      mode: 'render',
      stdin: { ...sampleStdin, terminal_width: 110 },
      columns: 110,
      env,
      probesRunner: fakeProbes,
    });
    const zhJoined = zh.output.lines.map(stripAnsi).join('\n');
    assert.ok(zhJoined.includes('就绪'));
    assert.ok(zhJoined.includes('[规划]'));
    assert.ok(zhJoined.includes('上下文'));
    assert.ok(zhJoined.includes('配额(weekly)'));
    assert.ok(zhJoined.includes('沙箱开启'));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('AGY_HUD_DISABLE environment flag toggles correctly', () => {
  assert.equal(isHudDisabled({}), false);
  assert.equal(isHudDisabled({ AGY_HUD_DISABLE: '0' }), false);
  assert.equal(isHudDisabled({ AGY_HUD_DISABLE: 'false' }), false);
  assert.equal(isHudDisabled({ AGY_HUD_DISABLE: '1' }), true);
  assert.equal(isHudDisabled({ AGY_HUD_DISABLE: 'true' }), true);
});

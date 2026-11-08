import assert from 'node:assert/strict';
import test from 'node:test';
import { terminalFilter } from '../scripts/hud.mjs';
import {
  COLORS,
  fileLink,
  paint,
  renderBar,
  sanitizeText,
  stripAnsi,
  textWidth,
  truncateAnsi,
} from '../src/ansi.ts';

test('textWidth handles ASCII, CJK, Emoji, and ANSI escapes accurately', () => {
  assert.equal(textWidth('hello'), 5);
  assert.equal(textWidth('简体中文'), 8);
  assert.equal(textWidth('🔧 TOOL'), 7);
  assert.equal(textWidth(paint('上下文 100%', COLORS.brightGreen)), 11);
  const linked = fileLink('/Users/dev/agy-hud', paint('agy-hud', COLORS.brightWhite));
  assert.equal(textWidth(linked), 7);
});

test('truncateAnsi respects column budget on CJK boundaries and closes OSC 8 links', () => {
  const cjk = paint('简体中文状态栏测试', COLORS.brightCyan);
  for (const width of [5, 6, 8, 11]) {
    const truncated = truncateAnsi(cjk, width);
    assert.ok(
      textWidth(truncated) <= width,
      `Expected width <= ${width}, got ${textWidth(truncated)} for ${JSON.stringify(stripAnsi(truncated))}`,
    );
  }

  const linked = fileLink('/Users/dev/very-long-workspace-folder-name', 'very-long-workspace-folder-name');
  const clipped = truncateAnsi(linked, 12);
  assert.ok(textWidth(clipped) <= 12);
  // Ensure open OSC 8 sequence was closed cleanly
  assert.ok(clipped.includes('\x1b]8;;\x1b\\'));
});

test('renderBar produces partial block gradients of exact visual width', () => {
  for (const pct of [0, 6.2, 28.4, 65, 92.5, 100]) {
    const bar = renderBar(pct, 12);
    assert.equal(textWidth(bar), 12);
  }
});

test('terminalFilter strips dangerous escapes while preserving SGR and file:// links', () => {
  const filter = terminalFilter();

  // Preserves SGR colors and file:// links
  const safe = `${COLORS.brightGreen}READY\x1b[0m ${fileLink('/tmp/demo', 'demo')}`;
  assert.equal(filter(safe), safe);

  // Strips OSC 52 clipboard exfiltration, OSC 2 title hijack, CSI cursor clear, remote OSC 8, and Bidi override
  const hostile =
    'ok\x1b]52;c;cHduZWQ=\x07\x1b]2;spoofed-title\x07\x1b[2J\x1b[H' +
    '\x1b]8;;https://evil.example/phish\x1b\\click\x1b]8;;\x1b\\\u202ehidden';
  const cleaned = filter(hostile);
  assert.equal(stripAnsi(cleaned), 'okclickhidden');
  assert.ok(!cleaned.includes('52;c;'));
  assert.ok(!cleaned.includes('spoofed-title'));
  assert.ok(!cleaned.includes('evil.example'));
  assert.ok(!cleaned.includes('\u202e'));
});

test('sanitizeText removes newlines, ANSI codes, and bidi controls', () => {
  assert.equal(sanitizeText('  Gemini\n4\t\x1b[31mArgon\u202e  '), 'Gemini 4 Argon');
});

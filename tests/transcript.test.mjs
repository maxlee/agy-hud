import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { deriveTaskMdPath } from '../scripts/collector.mjs';
import { parseTranscriptAndTasks, unwrapArgValue } from '../src/transcript.ts';

test('unwrapArgValue decodes double-serialized Antigravity transcript strings', () => {
  assert.equal(
    unwrapArgValue('"/Users/max/.gemini/config/skills/hud/SKILL.md"'),
    '/Users/max/.gemini/config/skills/hud/SKILL.md',
  );
  assert.equal(unwrapArgValue('plain-value'), 'plain-value');
  assert.equal(unwrapArgValue(5000), '5000');
});

test('deriveTaskMdPath resolves conversation root task.md from .system_generated/logs/transcript.jsonl', () => {
  const tp = '/Users/dev/.gemini/antigravity-cli/brain/1234-5678/.system_generated/logs/transcript.jsonl';
  assert.equal(deriveTaskMdPath(tp), '/Users/dev/.gemini/antigravity-cli/brain/1234-5678/task.md');
});

test('parseTranscriptAndTasks extracts tools, skills, MCP servers, subagents, and task.md todos', () => {
  const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-hud-transcript-'));
  try {
    const convDir = path.join(tmpRoot, 'conv-1');
    const logsDir = path.join(convDir, '.system_generated', 'logs');
    fs.mkdirSync(logsDir, { recursive: true });

    const transcriptPath = path.join(logsDir, 'transcript.jsonl');
    const taskMdPath = path.join(convDir, 'task.md');

    const steps = [
      {
        step_index: 0,
        source: 'USER_EXPLICIT',
        type: 'USER_INPUT',
        status: 'DONE',
        created_at: '2026-11-08T13:00:00Z',
        content: '/boost Build the CLI HUD',
      },
      {
        step_index: 1,
        source: 'MODEL',
        type: 'PLANNER_RESPONSE',
        status: 'DONE',
        created_at: '2026-11-08T13:00:02Z',
        tool_calls: [
          { name: 'read_file', args: { Path: '"/Users/dev/.gemini/skills/hud/SKILL.md"' } },
          { name: 'read_file', args: { Path: '"/Users/dev/project/README.md"' } },
          { name: 'call_mcp_tool', args: { ServerName: '"context7"', ToolName: '"query-docs"' } },
          { name: 'invoke_subagent', args: { TypeName: '"research"', Role: '"Spec Reader"' } },
        ],
      },
      { step_index: 2, source: 'MODEL', type: 'GENERIC', status: 'DONE', created_at: '2026-11-08T13:00:03Z', content: 'ok' },
      { step_index: 3, source: 'MODEL', type: 'GENERIC', status: 'DONE', created_at: '2026-11-08T13:00:03Z', content: 'ok' },
      { step_index: 4, source: 'MODEL', type: 'GENERIC', status: 'ERROR', created_at: '2026-11-08T13:00:04Z', content: 'Error: timeout' },
      { step_index: 5, source: 'MODEL', type: 'GENERIC', status: 'DONE', created_at: '2026-11-08T13:00:05Z', content: 'spawned' },
      {
        step_index: 6,
        source: 'MODEL',
        type: 'PLANNER_RESPONSE',
        status: 'DONE',
        created_at: '2026-11-08T13:01:00Z',
        tool_calls: [
          { name: 'edit_file', args: { Path: '"/Users/dev/project/render.ts"' } },
          { name: 'run_command', args: { CommandLine: '"npm test"', toolSummary: '"Run unit tests"' } },
        ],
      },
      // Only 1 GENERIC result so far -> edit_file completed, run_command still running!
      { step_index: 7, source: 'MODEL', type: 'GENERIC', status: 'DONE', created_at: '2026-11-08T13:01:01Z', content: 'edited' },
    ];

    fs.writeFileSync(transcriptPath, `${steps.map((s) => JSON.stringify(s)).join('\n')}\n`, 'utf8');
    fs.writeFileSync(
      taskMdPath,
      ['# Tasks', '- [x] Parse stdin JSON', '- [/] Parse transcript.jsonl', '- [ ] Validate strict schema'].join('\n'),
      'utf8',
    );

    const summary = parseTranscriptAndTasks(transcriptPath, taskMdPath, {
      agent_state: 'tool_use',
      subagents: [{ name: 'research', role: 'Spec Reader', status: 'running' }],
    });

    assert.equal(summary.sessionStartedAt, Date.parse('2026-11-08T13:00:00Z'));
    assert.deepEqual(summary.skills, ['boost', 'hud']);
    assert.deepEqual(summary.mcpServers, ['context7']);
    assert.deepEqual(summary.mcpErrors, ['context7']);

    // Running tool comes first
    assert.equal(summary.tools[0].name, 'run_command');
    assert.equal(summary.tools[0].status, 'running');
    assert.equal(summary.tools[0].target, 'Run unit tests');

    // Error tool comes second
    const errTool = summary.tools.find((t) => t.name === 'call_mcp_tool');
    assert.equal(errTool?.status, 'error');

    // Folded read_file ×2 across two distinct files clears misleading single target
    const readTool = summary.tools.find((t) => t.name === 'read_file');
    assert.equal(readTool?.count, 2);
    assert.equal(readTool?.target, undefined);

    // Subagent merged with live running status
    assert.equal(summary.subagents.length, 1);
    assert.equal(summary.subagents[0].status, 'running');
    assert.equal(summary.subagents[0].role, 'Spec Reader');

    // Todos parsed from task.md
    assert.equal(summary.todos.length, 3);
    assert.equal(summary.todos[0].status, 'completed');
    assert.equal(summary.todos[1].status, 'in_progress');
    assert.equal(summary.todos[2].status, 'pending');
  } finally {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

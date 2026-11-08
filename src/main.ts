import * as fs from 'node:fs';
import { executeSubcommand } from './commands.ts';
import { loadConfigFile } from './config.ts';
import { parseMemoryInfo, parseVcsInfo } from './probes.ts';
import { renderHudLines } from './render.ts';
import { parseTranscriptAndTasks } from './transcript.ts';
import type { CageInput, CageOutput, ProbeAnswers } from './types.ts';

type ProbeName = 'vcs' | 'memory';

let resolveInitialInput: ((input: CageInput) => void) | null = null;
const pendingAsks = new Map<number, (answers: ProbeAnswers) => void>();
let nextAskId = 1;

function writeLineToStdout(payload: unknown): void {
  const buf = Buffer.from(`${JSON.stringify(payload)}\n`, 'utf8');
  let offset = 0;
  while (offset < buf.length) {
    offset += fs.writeSync(1, buf, offset, buf.length - offset);
  }
}

function startFd0Reader(): void {
  const chunk = Buffer.alloc(64 * 1024);
  let pending = Buffer.alloc(0);

  const pump = (): void => {
    fs.read(0, chunk, 0, chunk.length, null, (err, bytesRead) => {
      if (err || bytesRead === 0) return;
      pending = Buffer.concat([pending, chunk.subarray(0, bytesRead)]);
      let nl = pending.indexOf(0x0a);
      while (nl !== -1) {
        const line = pending.subarray(0, nl).toString('utf8').trim();
        pending = pending.subarray(nl + 1);
        if (line) {
          const msg = JSON.parse(line) as { id?: number; answers?: ProbeAnswers } & CageInput;
          if (typeof msg.id === 'number') {
            const cb = pendingAsks.get(msg.id);
            pendingAsks.delete(msg.id);
            cb?.(msg.answers ?? {});
          } else if (resolveInitialInput) {
            const cb = resolveInitialInput;
            resolveInitialInput = null;
            cb(msg);
          }
        }
        nl = pending.indexOf(0x0a);
      }
      pump();
    });
  };
  pump();
}

function readInitialInput(): Promise<CageInput> {
  return new Promise<CageInput>((resolve) => {
    resolveInitialInput = resolve;
    startFd0Reader();
  });
}

function askProbes(probes: ProbeName[]): Promise<ProbeAnswers> {
  if (probes.length === 0) return Promise.resolve({});
  return new Promise<ProbeAnswers>((resolve) => {
    const id = nextAskId++;
    pendingAsks.set(id, resolve);
    writeLineToStdout({ id, ask: probes });
  });
}

async function run(): Promise<void> {
  const input = await readInitialInput();

  if (input.mode === 'command' && input.command) {
    const out = executeSubcommand(
      input.command.name,
      input.command.args,
      input.command.facts,
      input.paths,
      input.now,
    );
    writeLineToStdout({ result: out });
    return;
  }

  const { config } = loadConfigFile(input.paths.config);
  const needed: ProbeName[] = [];
  if (config.vcs.enabled && (config.vcs.showAheadBehind || config.vcs.showFileStats || !input.stdin?.vcs?.branch)) {
    needed.push('vcs');
  }
  if (config.display.showMemory) {
    needed.push('memory');
  }

  const answers = await askProbes(needed);
  const transcript = parseTranscriptAndTasks(input.paths.transcript, input.paths.taskMd, input.stdin);
  const vcs = parseVcsInfo(input.stdin, answers);
  const memory = parseMemoryInfo(answers);

  const lines = renderHudLines({
    stdin: input.stdin,
    config,
    transcript,
    vcs,
    memory,
    columns: input.columns ?? input.stdin?.terminal_width ?? null,
    now: input.now,
  });

  const out: CageOutput = {
    lines,
    snapshotToWrite: input.stdin
      ? {
          at: input.now,
          stdin: input.stdin,
          probes: answers,
        }
      : undefined,
  };

  // Persist last snapshot inside the one writable `data/` directory if available
  if (out.snapshotToWrite && input.paths.lastSnapshot) {
    try {
      const tmp = `${input.paths.lastSnapshot}.${process.pid}.tmp`;
      fs.writeFileSync(tmp, JSON.stringify(out.snapshotToWrite), { mode: 0o600 });
      fs.renameSync(tmp, input.paths.lastSnapshot);
    } catch {
      // non-fatal if read-only filesystem
    }
  }

  writeLineToStdout({ result: out });
}

run().catch((err) => {
  const msg = err instanceof Error ? err.message : String(err);
  writeLineToStdout({
    result: {
      lines: [`agy-hud error: ${msg}`],
      failed: true,
    },
  });
});

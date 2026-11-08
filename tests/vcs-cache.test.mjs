import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  GIT_SAFE_PREFIX,
  HG_SAFE_PREFIX,
  JJ_PROBE_ARGS,
  agyPaths,
  computeVcsSignature,
  detectVcsRepo,
  parseHgProbeOutput,
  parseJjProbeOutput,
  probeGit,
  probeHg,
  probeJj,
  probeVcsCached,
  stripLogicFiles,
} from '../scripts/collector.mjs';
import { runCage } from '../scripts/hud.mjs';
import { stripAnsi } from '../src/ansi.ts';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

test('stripLogicFiles uses manifest.json fast-path on warm runs and invalidates on .ts change', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-hud-strip-'));
  try {
    const fakeRoot = path.join(tmp, 'plugin');
    const fakeSrc = path.join(fakeRoot, 'src');
    const strippedDir = path.join(tmp, 'stripped');
    fs.mkdirSync(fakeSrc, { recursive: true });
    const sampleTs = path.join(fakeSrc, 'demo.ts');
    fs.writeFileSync(sampleTs, 'export const answer: number = 42;\n', 'utf8');

    const cold = stripLogicFiles(strippedDir, fakeRoot);
    assert.ok(cold);
    assert.equal(cold.cacheHit, false);
    assert.ok(fs.existsSync(path.join(strippedDir, 'manifest.json')));

    const warm = stripLogicFiles(strippedDir, fakeRoot);
    assert.ok(warm);
    assert.equal(warm.cacheHit, true);
    assert.deepEqual(warm.files, cold.files);

    // Modify file size/content -> invalidates manifest entry and sweeps stale .js
    fs.writeFileSync(sampleTs, 'export const answer: number = 9999;\n', 'utf8');
    const updated = stripLogicFiles(strippedDir, fakeRoot);
    assert.ok(updated);
    assert.equal(updated.cacheHit, false);
    assert.notDeepEqual(updated.files, cold.files);
    // Old stripped .js was swept while manifest.json + new .js remain
    const remaining = fs.readdirSync(strippedDir).sort();
    assert.equal(remaining.length, 2);
    assert.ok(remaining.includes('manifest.json'));
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('detectVcsRepo handles colocated .jj, nested .git, Git worktree .git files, .hg, and ignores symlinked markers', () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-hud-vcs-detect-'));
  try {
    // 1. Colocated .jj + .git prefers jj
    const colocated = path.join(tmp, 'colocated');
    fs.mkdirSync(path.join(colocated, '.jj'), { recursive: true });
    fs.mkdirSync(path.join(colocated, '.git'), { recursive: true });
    const detColocated = detectVcsRepo(colocated);
    assert.equal(detColocated?.type, 'jj');

    // 2. Nested .git inside outer .jj stops at inner .git boundary
    const nestedGit = path.join(colocated, 'packages', 'sub-git');
    fs.mkdirSync(path.join(nestedGit, '.git'), { recursive: true });
    const detNested = detectVcsRepo(nestedGit);
    assert.equal(detNested?.type, 'git');
    assert.equal(detNested?.root, fs.realpathSync(nestedGit));

    // 3. Git worktree (.git regular file pointing to gitdir + commondir)
    const mainGitDir = path.join(tmp, 'main-repo', '.git');
    const wtGitDir = path.join(mainGitDir, 'worktrees', 'wt-feature');
    fs.mkdirSync(wtGitDir, { recursive: true });
    fs.writeFileSync(path.join(wtGitDir, 'HEAD'), 'ref: refs/heads/wt-feature\n');
    fs.writeFileSync(path.join(wtGitDir, 'commondir'), '../..\n');

    const worktreeDir = path.join(tmp, 'wt-checkout');
    fs.mkdirSync(worktreeDir, { recursive: true });
    fs.writeFileSync(path.join(worktreeDir, '.git'), `gitdir: ${wtGitDir}\n`);

    const detWt = detectVcsRepo(worktreeDir);
    assert.equal(detWt?.type, 'git');
    assert.equal(detWt?.gitDir, fs.realpathSync(wtGitDir));
    assert.equal(detWt?.commonDir, fs.realpathSync(mainGitDir));
    assert.ok(computeVcsSignature(detWt).startsWith('git|'));

    // 4. Mercurial (.hg)
    const hgRepo = path.join(tmp, 'hg-repo');
    fs.mkdirSync(path.join(hgRepo, '.hg'), { recursive: true });
    assert.equal(detectVcsRepo(hgRepo)?.type, 'hg');

    // 5. Symlinked .jj marker is ignored for safety
    const fakeSymlinkRepo = path.join(tmp, 'symlink-repo');
    fs.mkdirSync(fakeSymlinkRepo, { recursive: true });
    fs.symlinkSync(path.join(colocated, '.jj'), path.join(fakeSymlinkRepo, '.jj'));
    assert.equal(detectVcsRepo(fakeSymlinkRepo), null);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test('Jujutsu (jj) and Mercurial (hg) probes use strictly read-only flags and parse bookmarks, change IDs, conflicts, and file stats', async () => {
  // 1. Verify JJ_PROBE_ARGS safety invariants
  assert.ok(JJ_PROBE_ARGS.includes('--ignore-working-copy'));
  assert.ok(JJ_PROBE_ARGS.includes('--at-operation=@'));
  assert.ok(JJ_PROBE_ARGS.includes('--no-pager'));

  // Anonymous change ID (no bookmark), clean, no conflict
  const anon = parseJjProbeOutput('kpqxywon\x1f\x1f0\x1f0\n');
  assert.deepEqual(anon, {
    type: 'jj',
    branch: '@kpqxywon',
    dirty: false,
    ahead: 0,
    behind: 0,
    modified: 0,
    added: 0,
    deleted: 0,
    untracked: 0,
    conflicts: 0,
  });

  // Multiple bookmarks + dirty + conflict
  const conflicted = parseJjProbeOutput('zvtrplmn\x1ffeat/hud\x1emain\x1f1\x1f1\n');
  assert.equal(conflicted?.branch, 'feat/hud+1');
  assert.equal(conflicted?.dirty, true);
  assert.equal(conflicted?.conflicts, 1);

  const seenCalls = [];
  const jjJson = await probeJj('/tmp/fake-jj', async (cmd, args) => {
    seenCalls.push({ cmd, args });
    return 'abcd1234\x1ffeat/jj-probe\x1f1\x1f1\n';
  });
  assert.equal(seenCalls[0].cmd, 'jj');
  assert.deepEqual(seenCalls[0].args, JJ_PROBE_ARGS);

  // Render jj conflict badge (!1) end-to-end inside cage
  const tmpEnvDir = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-hud-jj-render-'));
  try {
    const { output } = await runCage({
      mode: 'render',
      stdin: { cwd: ROOT, vcs: { type: 'jj' }, terminal_width: 120 },
      columns: 120,
      env: {
        ...process.env,
        AGY_CLI_DIR: path.join(tmpEnvDir, 'cli'),
        AGY_HUD_HOME: path.join(tmpEnvDir, 'hud'),
      },
      probesRunner: async () => ({ vcs: jjJson }),
    });
    const plain = output.lines.map(stripAnsi).join('\n');
    assert.ok(plain.includes('jj:(feat/jj-probe* !1)'), `Expected jj conflict badge in: ${plain}`);
  } finally {
    fs.rmSync(tmpEnvDir, { recursive: true, force: true });
  }

  // 2. Verify Mercurial (hg) probe & parser
  const hgCalls = [];
  const hgJson = await probeHg('/tmp/fake-hg', async (cmd, args) => {
    hgCalls.push({ cmd, args });
    if (args.includes('branch')) return 'feature-hg\n';
    return 'M src/app.ts\nA src/new.ts\nR src/old.ts\n! src/missing.ts\n? scratch.txt\n';
  });
  assert.equal(hgCalls.length, 2);
  for (const c of hgCalls) {
    assert.equal(c.cmd, 'hg');
    for (const flag of HG_SAFE_PREFIX) assert.ok(c.args.includes(flag));
  }
  const hgParsed = JSON.parse(hgJson);
  assert.deepEqual(hgParsed, {
    type: 'hg',
    branch: 'feature-hg',
    dirty: true,
    ahead: 0,
    behind: 0,
    modified: 1,
    added: 1,
    deleted: 2,
    untracked: 1,
    conflicts: 0,
  });
  assert.equal(parseHgProbeOutput('', ''), null);
});

test('probeGit enforces untrusted-repo safety overrides (protocol.allow=never, core.fsmonitor=false, --no-optional-locks)', async () => {
  let capturedArgs = [];
  const res = await probeGit('/tmp/fake-git', async (_cmd, args) => {
    capturedArgs = args;
    return '# branch.head main\n# branch.ab +3 -1\n1 .M N... 100644 100644 100644 000 000 file.ts\n';
  });
  for (const item of GIT_SAFE_PREFIX) {
    assert.ok(capturedArgs.includes(item), `Missing git safety arg: ${item}`);
  }
  assert.ok(capturedArgs.includes('--ignore-submodules=dirty'));
  const parsed = JSON.parse(res);
  assert.equal(parsed.branch, 'main');
  assert.equal(parsed.ahead, 3);
  assert.equal(parsed.behind, 1);
  assert.equal(parsed.modified, 1);
});

test('probeVcsCached avoids subprocess spawns within TTL and invalidates immediately on mtime, stdin.vcs flip, or TTL expiry', async () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'agy-hud-vcache-'));
  try {
    const repoDir = path.join(tmp, 'repo');
    const gitDir = path.join(repoDir, '.git');
    fs.mkdirSync(path.join(gitDir, 'refs', 'heads'), { recursive: true });
    fs.writeFileSync(path.join(gitDir, 'HEAD'), 'ref: refs/heads/main\n');
    const indexPath = path.join(gitDir, 'index');
    fs.writeFileSync(indexPath, 'v1');

    const paths = agyPaths({
      ...process.env,
      AGY_CLI_DIR: path.join(tmp, 'cli'),
      AGY_HUD_HOME: path.join(tmp, 'hud'),
    });

    let spawns = 0;
    const fakeRunner = async () => {
      spawns++;
      return `# branch.head main\n# branch.ab +${spawns} -0\n`;
    };

    const t0 = 1_700_000_000_000;

    // 1. Cold miss -> spawns once
    const r1 = JSON.parse(
      await probeVcsCached({ cwd: repoDir, paths, ttlMs: 1500, now: t0, runner: fakeRunner }),
    );
    assert.equal(spawns, 1);
    assert.equal(r1.ahead, 1);

    // 2. Warm hit at t0 + 300ms -> zero new spawns!
    const r2 = JSON.parse(
      await probeVcsCached({ cwd: repoDir, paths, ttlMs: 1500, now: t0 + 300, runner: fakeRunner }),
    );
    assert.equal(spawns, 1);
    assert.equal(r2.ahead, 1);

    // 3. Mutate .git/index size/mtime -> instant cache invalidation even at t0 + 400ms!
    fs.writeFileSync(indexPath, 'v2-staged-change');
    const r3 = JSON.parse(
      await probeVcsCached({ cwd: repoDir, paths, ttlMs: 1500, now: t0 + 400, runner: fakeRunner }),
    );
    assert.equal(spawns, 2);
    assert.equal(r3.ahead, 2);

    // 4. stdin.vcs.dirty mismatch -> instant cache invalidation!
    const r4 = JSON.parse(
      await probeVcsCached({
        cwd: repoDir,
        paths,
        ttlMs: 1500,
        stdinVcs: { dirty: true },
        now: t0 + 500,
        runner: fakeRunner,
      }),
    );
    assert.equal(spawns, 3);
    assert.equal(r4.ahead, 3);

    // 5. Advance clock past TTL (t0 + 2500ms) -> refreshes
    const r5 = JSON.parse(
      await probeVcsCached({ cwd: repoDir, paths, ttlMs: 1500, now: t0 + 2500, runner: fakeRunner }),
    );
    assert.equal(spawns, 4);
    assert.equal(r5.ahead, 4);

    // 6. ttlMs = 0 disables cache completely
    await probeVcsCached({ cwd: repoDir, paths, ttlMs: 0, now: t0 + 2600, runner: fakeRunner });
    assert.equal(spawns, 5);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

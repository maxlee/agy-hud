import assert from 'node:assert/strict';
import fs from 'node:fs';
import nodeModule from 'node:module';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

function stripTypesQuiet(code) {
  const orig = process.emitWarning;
  process.emitWarning = () => {};
  try {
    return nodeModule.stripTypeScriptTypes(code);
  } finally {
    process.emitWarning = orig;
  }
}

test('ARCHITECTURE.md Mermaid diagram matches src/*.ts runtime imports and fence.mjs builtins', () => {
  const archMd = fs.readFileSync(path.join(ROOT, 'ARCHITECTURE.md'), 'utf8');
  const mermaidMatch = /```mermaid\s+flowchart TD([\s\S]*?)```/.exec(archMd);
  assert.ok(mermaidMatch, 'Expected flowchart TD mermaid block in ARCHITECTURE.md');
  const diagram = mermaidMatch[1];

  // Parse square boxes: id["label: file1.ts file2.ts"]
  const fileToBox = new Map();
  const boxes = new Set();
  for (const m of diagram.matchAll(/^\s*([a-zA-Z0-9_]+)\["[^:]+:\s*([^"]+)"\]/gm)) {
    const boxId = m[1];
    boxes.add(boxId);
    for (const f of m[2].trim().split(/\s+/)) {
      assert.ok(!fileToBox.has(f), `File ${f} listed in multiple boxes in ARCHITECTURE.md`);
      fileToBox.set(f, boxId);
    }
  }

  // Parse rounded builtin nodes: id(["node:fs node:fs/promises"])
  const builtinToNode = new Map();
  for (const m of diagram.matchAll(/^\s*([a-zA-Z0-9_]+)\(\["([^"]+)"\]\)/gm)) {
    const nodeId = m[1];
    for (const b of m[2].trim().split(/\s+/)) {
      builtinToNode.set(b, nodeId);
    }
  }

  // Verify rounded builtin nodes match ALLOWED_BUILTINS in scripts/fence.mjs
  const fenceSrc = fs.readFileSync(path.join(ROOT, 'scripts', 'fence.mjs'), 'utf8');
  const fenceAllowed = new Set(
    [...fenceSrc.matchAll(/'(node:[^']+)'\s*:\s*1/g)].map((m) => m[1]),
  );
  assert.deepEqual(
    new Set(builtinToNode.keys()),
    fenceAllowed,
    'Rounded builtin nodes in ARCHITECTURE.md must match ALLOWED_BUILTINS in scripts/fence.mjs',
  );

  // Parse declared arrows: `src --> dst1 & dst2`
  const declaredEdges = new Set();
  for (const m of diagram.matchAll(/^\s*([a-zA-Z0-9_]+)\s*-->\s*(.+)$/gm)) {
    const from = m[1];
    for (const to of m[2].split('&').map((s) => s.trim())) {
      if (to) declaredEdges.add(`${from}->${to}`);
    }
  }

  // Inspect actual runtime imports across all src/*.ts files
  const srcDir = path.join(ROOT, 'src');
  const actualFiles = fs.readdirSync(srcDir).filter((f) => f.endsWith('.ts'));
  assert.deepEqual(
    new Set(actualFiles),
    new Set(fileToBox.keys()),
    'Every src/*.ts file must appear in exactly one box in ARCHITECTURE.md',
  );

  const observedEdges = new Set();
  const importRegex = /\bimport\s+(?:[^'"]+?\s+from\s+)?['"]([^'"]+)['"]/g;

  for (const file of actualFiles) {
    const fromBox = fileToBox.get(file);
    const stripped = stripTypesQuiet(fs.readFileSync(path.join(srcDir, file), 'utf8'));
    for (const m of stripped.matchAll(importRegex)) {
      const spec = m[1];
      if (spec.startsWith('./')) {
        const targetFile = path.basename(spec);
        const toBox = fileToBox.get(targetFile);
        assert.ok(toBox, `Import ${spec} in ${file} points to unknown file`);
        if (toBox !== fromBox) {
          observedEdges.add(`${fromBox}->${toBox}`);
        }
      } else {
        const norm = spec.startsWith('node:') ? spec : `node:${spec}`;
        const toNode = builtinToNode.get(norm);
        assert.ok(toNode, `Disallowed builtin import ${spec} in ${file}`);
        observedEdges.add(`${fromBox}->${toNode}`);
      }
    }
  }

  assert.deepEqual(
    observedEdges,
    declaredEdges,
    'Observed runtime import edges in src/*.ts must match ARCHITECTURE.md Mermaid arrows',
  );
});

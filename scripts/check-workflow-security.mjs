#!/usr/bin/env node
// Keep GitHub Actions dependencies immutable and workflow token scope explicit.
// Dependabot updates SHA pins using their adjacent version comments.
import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const workflows = join(root, '.github', 'workflows');
const problems = [];

for (const file of readdirSync(workflows)
  .filter((name) => name.endsWith('.yml') || name.endsWith('.yaml'))
  .sort()) {
  const text = readFileSync(join(workflows, file), 'utf8');
  if (!/^permissions:\s*(?:#.*)?$/m.test(text)) {
    problems.push(`${file}: missing explicit top-level permissions`);
  }

  const uses = /^\s*(?:-\s+)?uses:\s+([^\s#]+)/gm;
  let match;
  while ((match = uses.exec(text)) !== null) {
    const action = match[1];
    if (action.startsWith('./') || action.startsWith('docker://')) continue;
    const separator = action.lastIndexOf('@');
    const revision = separator === -1 ? '' : action.slice(separator + 1);
    if (!/^[0-9a-f]{40}$/.test(revision)) {
      problems.push(`${file}: action is not pinned to a full commit SHA: ${action}`);
    }
  }
}

if (problems.length) {
  console.error(`✗ ${problems.length} workflow security problem(s):`);
  for (const problem of problems) console.error(`  ${problem}`);
  process.exit(1);
}

console.log('✓ workflow security OK (explicit permissions, immutable action pins)');

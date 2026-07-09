#!/usr/bin/env node
// Deterministic backstop for the docs: every relative markdown link must resolve
// to a real file, and every `#anchor` must match a heading in the target file.
// Splitting DESIGN.md into docs/agent-guides/ moved anchors around; this is what
// keeps the nested CLAUDE.md maps and skills from silently pointing at nothing.
//
// Zero deps. Run: `node scripts/check-doc-links.mjs` (CI + the doc-drift hook use it).
// Scope: tracked + untracked-not-ignored *.md (so it sees new docs before commit).
import { execSync } from 'node:child_process';
import { readFileSync, existsSync, statSync } from 'node:fs';
import { dirname, resolve, join } from 'node:path';

const root = execSync('git rev-parse --show-toplevel').toString().trim();
const files = execSync("git ls-files -co --exclude-standard '*.md'", { cwd: root })
  .toString().split('\n').filter(Boolean);

// GitHub-flavored heading slug: lowercase, drop anything but word chars/space/hyphen
// (backticks and punctuation vanish), spaces -> hyphens.
const slug = (s) =>
  s.toLowerCase().replace(/[^\w\s-]/g, '').trim().replace(/\s+/g, '-');

const anchorsOf = (absPath) => {
  const set = new Set();
  for (const line of readFileSync(absPath, 'utf8').split('\n')) {
    const m = /^#{1,6}\s+(.*?)\s*#*\s*$/.exec(line);
    if (m) set.add(slug(m[1]));
  }
  return set;
};

const anchorCache = new Map();
const getAnchors = (p) => {
  if (!anchorCache.has(p)) anchorCache.set(p, anchorsOf(p));
  return anchorCache.get(p);
};

// [text](target) — skip images are fine to check too; ignore ![.. the same way.
const LINK = /\[[^\]]*\]\(([^)]+)\)/g;
const problems = [];

for (const rel of files) {
  const abs = join(root, rel);
  const dir = dirname(abs);
  const text = readFileSync(abs, 'utf8');
  let m;
  while ((m = LINK.exec(text)) !== null) {
    let target = m[1].trim().split(/\s+/)[0]; // drop optional "title"
    if (/^(https?:|mailto:|tel:|#!)/.test(target)) continue; // external
    const [pathPart, anchor] = target.split('#');
    // Same-file anchor link: [x](#heading)
    if (pathPart === '') {
      if (anchor && !getAnchors(abs).has(slug(anchor)))
        problems.push(`${rel}: missing same-file anchor #${anchor}`);
      continue;
    }
    const tgtAbs = resolve(dir, pathPart);
    if (!existsSync(tgtAbs)) {
      problems.push(`${rel}: broken link -> ${pathPart}`);
      continue;
    }
    if (anchor && statSync(tgtAbs).isFile() && tgtAbs.endsWith('.md')) {
      if (!getAnchors(tgtAbs).has(slug(anchor)))
        problems.push(`${rel}: ${pathPart} has no anchor #${anchor}`);
    }
  }
}

if (problems.length) {
  console.error(`✗ ${problems.length} broken doc link(s):`);
  for (const p of problems) console.error('  ' + p);
  process.exit(1);
}
console.log(`✓ doc links OK (${files.length} markdown files checked)`);

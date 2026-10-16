import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  areasTouching,
  extractRoutingBlock,
  featuresTouching,
  globToRegExp,
  listFiles,
  loadRegistry,
  matchesAny,
  renderRoutingTable,
  validateRegistry,
  writeRoutingBlock,
} from './check-feature-registry.mjs';

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..');
const m = (glob, path) => globToRegExp(glob).test(path);

test('globToRegExp: ** segments, trailing /**, bare **', () => {
  assert.ok(m('a/**/d.rs', 'a/d.rs'));
  assert.ok(m('a/**/d.rs', 'a/b/c/d.rs'));
  assert.ok(!m('a/**/d.rs', 'a/bd.rs'));
  assert.ok(m('a/**', 'a/b/c.rs'));
  assert.ok(m('a/**', 'a'));
  assert.ok(!m('a/**', 'ab/c'));
  assert.ok(m('**', 'any/thing.rs'));
  assert.ok(m('**/x.ts', 'x.ts'));
  assert.ok(m('**/x.ts', 'p/q/x.ts'));
});

test('globToRegExp: * and ? stay in one segment', () => {
  assert.ok(m('b/*.rs', 'b/x.rs'));
  assert.ok(!m('b/*.rs', 'b/s/x.rs'));
  assert.ok(m('e?.txt', 'ef.txt'));
  assert.ok(!m('e?.txt', 'e/.txt'));
  assert.ok(!m('e?.txt', 'e.txt'));
});

test('globToRegExp: classes, exact files, anchoring, literal dots', () => {
  assert.ok(m('w/[Tt]imeline*', 'w/Timeline.svelte'));
  assert.ok(m('w/[Tt]imeline*', 'w/timelineStore.ts'));
  assert.ok(!m('w/[Tt]imeline*', 'w/Xtimeline.ts'));
  assert.ok(m('a/b.rs', 'a/b.rs'));
  assert.ok(!m('a/b.rs', 'a/bxrs'));
  assert.ok(!m('a/b.rs', 'xa/b.rs'));
  assert.ok(!m('a/b.rs', 'a/b.rs.bak'));
  assert.ok(m('plugins.lock', 'plugins.lock'));
  assert.ok(m('a/(b).rs', 'a/(b).rs'));
});

test('matchesAny', () => {
  assert.ok(matchesAny('a/b.rs', ['x/**', 'a/*.rs']));
  assert.ok(!matchesAny('a/b.rs', ['x/**']));
  assert.ok(!matchesAny('a/b.rs', []));
});

// A valid tree: one area, one feature with page/map/rule, a README carrying the block.
const baseRegistry = () => ({
  areas: [{ id: 'core', map: 'core/AGENTS.md', paths: ['core/src/**'] }],
  features: [
    {
      id: 'alpha',
      name: 'Alpha',
      page: 'docs/features/alpha.md',
      paths: ['core/src/**', 'ui/a.ts'],
      maps: ['core/AGENTS.md'],
      rules: ['.claude/rules/alpha.md'],
      checks: ['cargo test -p core'],
    },
  ],
});

function fixture(mutate) {
  const root = mkdtempSync(join(tmpdir(), 'registry-test-'));
  const put = (rel, text = 'x\n') => {
    mkdirSync(dirname(join(root, rel)), { recursive: true });
    writeFileSync(join(root, rel), text);
  };
  put('core/AGENTS.md');
  put('core/src/lib.rs');
  put('ui/a.ts');
  put('.claude/rules/alpha.md');
  put('docs/features/alpha.md', '# Alpha\n\nbody\n');
  const registry = baseRegistry();
  const ctx = { root, put, registry, readme: true };
  mutate?.(ctx);
  if (ctx.readme) put('docs/features/README.md', `# Features\n\n## The pages\n\ntext\n\n## Other\n\n${renderRoutingTable(ctx.registry)}\n`);
  return ctx;
}

function problemsFor(mutate) {
  const ctx = fixture(mutate);
  try {
    return validateRegistry(ctx.registry, ctx.root, { files: listFilesForFixture(ctx) });
  } finally {
    rmSync(ctx.root, { recursive: true, force: true });
  }
}
// The universe the fixture would have in git: its created files.
function listFilesForFixture(ctx) {
  return ['core/AGENTS.md', 'core/src/lib.rs', 'ui/a.ts', '.claude/rules/alpha.md', 'docs/features/alpha.md', ...(ctx.extraFiles ?? [])];
}
const has = (problems, text) => assert.ok(problems.some((p) => p.includes(text)), `expected "${text}" in:\n${problems.join('\n')}`);

test('validateRegistry passes on a valid fixture', () => {
  assert.deepEqual(problemsFor(), []);
});

test('validateRegistry: page with no entry', () => {
  has(problemsFor((c) => c.put('docs/features/beta.md', '# Beta\n')), 'page docs/features/beta.md has no entry');
});

test('validateRegistry: missing page', () => {
  has(problemsFor((c) => (c.registry.features[0].page = 'docs/features/gone.md')), 'page docs/features/gone.md does not exist');
});

test('validateRegistry: id differs from page stem', () => {
  has(problemsFor((c) => (c.registry.features[0].id = 'omega')), 'id must equal the page stem "alpha"');
});

test('validateRegistry: glob matching no file', () => {
  has(problemsFor((c) => c.registry.features[0].paths.push('nowhere/**')), 'glob "nowhere/**" matches no file');
});

test('validateRegistry: malformed globs', () => {
  has(problemsFor((c) => c.registry.areas[0].paths.push('./core/x')), 'malformed glob');
  has(problemsFor((c) => c.registry.areas[0].paths.push('core/')), 'malformed glob');
});

test('validateRegistry: missing map (feature and area)', () => {
  has(problemsFor((c) => c.registry.features[0].maps.push('no/AGENTS.md')), 'map no/AGENTS.md does not exist');
  has(problemsFor((c) => (c.registry.areas[0].map = 'no/AGENTS.md')), 'map no/AGENTS.md does not exist');
});

test('validateRegistry: missing rule and rule outside .claude/rules/', () => {
  has(problemsFor((c) => c.registry.features[0].rules.push('.claude/rules/none.md')), 'rule .claude/rules/none.md does not exist');
  has(problemsFor((c) => (c.registry.features[0].rules = ['core/AGENTS.md'])), 'is not under .claude/rules/');
});

test('validateRegistry: empty paths, maps, checks', () => {
  has(problemsFor((c) => (c.registry.features[0].paths = [])), 'paths is empty');
  has(problemsFor((c) => (c.registry.features[0].maps = [])), 'maps is empty');
  has(problemsFor((c) => (c.registry.features[0].checks = [])), 'checks is empty');
  has(problemsFor((c) => (c.registry.areas[0].paths = [])), 'paths is empty');
});

test('validateRegistry: empty rules is allowed', () => {
  assert.deepEqual(problemsFor((c) => (c.registry.features[0].rules = [])), []);
});

test('validateRegistry: duplicate id across areas and features', () => {
  has(problemsFor((c) => (c.registry.areas[0].id = 'alpha')), 'duplicate id "alpha"');
});

test('validateRegistry: unknown and missing keys', () => {
  has(problemsFor((c) => (c.registry.features[0].blurb = 'hi')), 'unknown key "blurb"');
  has(problemsFor((c) => delete c.registry.areas[0].map), 'missing key "map"');
  has(problemsFor((c) => (c.registry.extra = [])), 'unknown key "extra"');
});

test('validateRegistry: non-kebab id', () => {
  has(problemsFor((c) => (c.registry.areas[0].id = 'Core_X')), 'id must be kebab-case');
});

test('validateRegistry: name differs from the H1', () => {
  has(problemsFor((c) => (c.registry.features[0].name = 'Beta')), 'differs from the page\'s H1 "Alpha"');
});

test('validateRegistry: routing block missing or hand-edited', () => {
  has(problemsFor((c) => {
    c.readme = false;
    c.put('docs/features/README.md', '# Features\n');
  }), 'no <!-- registry:start -->');
  has(problemsFor((c) => {
    c.readme = false;
    c.put('docs/features/README.md', `# F\n${renderRoutingTable(c.registry).replace('Alpha', 'Edited')}\n`);
  }), 'routing block differs');
  has(problemsFor((c) => {
    c.readme = false;
  }), 'README.md does not exist');
});

test('validateRegistry output is sorted', () => {
  const p = problemsFor((c) => {
    c.registry.features[0].paths.push('zzz/**', 'aaa/**');
  });
  assert.deepEqual(p, [...p].sort());
});

test('listFiles reads the git universe incl. untracked, minus deleted', async () => {
  const { execFileSync } = await import('node:child_process');
  const root = mkdtempSync(join(tmpdir(), 'registry-git-'));
  try {
    const git = (...a) => execFileSync('git', ['-c', 'user.name=t', '-c', 'user.email=t@t', ...a], { cwd: root, stdio: 'pipe' });
    git('init', '-q');
    writeFileSync(join(root, 'tracked.txt'), 'x');
    writeFileSync(join(root, 'gone.txt'), 'x');
    git('add', '.');
    git('commit', '-qm', 'i');
    rmSync(join(root, 'gone.txt'));
    writeFileSync(join(root, 'new.txt'), 'x');
    assert.deepEqual(listFiles(root), ['new.txt', 'tracked.txt']);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('renderRoutingTable is deterministic and sorted by id', () => {
  const reg = baseRegistry();
  reg.features.push({ ...reg.features[0], id: 'aardvark', name: 'Aardvark', page: 'docs/features/aardvark.md' });
  const a = renderRoutingTable(reg);
  const reversed = { ...reg, features: [...reg.features].reverse() };
  assert.equal(renderRoutingTable(reversed), a);
  assert.ok(a.indexOf('[Aardvark]') < a.indexOf('[Alpha]'));
  assert.ok(a.startsWith('<!-- registry:start -->') && a.endsWith('<!-- registry:end -->'));
  assert.ok(a.includes('[core](../../core/AGENTS.md)'));
  assert.ok(a.includes('[alpha](../../.claude/rules/alpha.md)'));
  assert.ok(a.includes('`cargo test -p core`'));
  const noRules = baseRegistry();
  noRules.features[0].rules = [];
  assert.ok(renderRoutingTable(noRules).includes('| — |'));
});

test('writeRoutingBlock inserts after "The pages", then round-trips', () => {
  const reg = baseRegistry();
  const readme = '# Features\n\n## The pages\n\nlist\n\n## Intent\n\ntext\n';
  const once = writeRoutingBlock(readme, reg);
  assert.equal(extractRoutingBlock(once), renderRoutingTable(reg));
  assert.ok(once.indexOf('## Where each feature is wired') > once.indexOf('## The pages'));
  assert.ok(once.indexOf('## Where each feature is wired') < once.indexOf('## Intent'));
  assert.equal(writeRoutingBlock(once, reg), once);
  const changed = baseRegistry();
  changed.features[0].checks = ['just check'];
  const again = writeRoutingBlock(once, changed);
  assert.ok(again.includes('`just check`') && !again.includes('cargo test -p core'));
  assert.equal(extractRoutingBlock('no block here'), null);
});

test('writeRoutingBlock appends when there is no "The pages" section', () => {
  const out = writeRoutingBlock('# Features\n', baseRegistry());
  assert.ok(out.includes('## Where each feature is wired'));
  assert.ok(extractRoutingBlock(out));
});

test('featuresTouching / areasTouching', () => {
  const reg = baseRegistry();
  reg.features.push({ ...reg.features[0], id: 'beta', paths: ['web/**'] });
  assert.deepEqual(featuresTouching(reg, ['ui/a.ts']).map((f) => f.id), ['alpha']);
  assert.deepEqual(featuresTouching(reg, ['web/x/y.ts', 'ui/a.ts']).map((f) => f.id), ['alpha', 'beta']);
  assert.deepEqual(featuresTouching(reg, ['README.md']), []);
  assert.deepEqual(areasTouching(reg, ['core/src/a/b.rs']).map((a) => a.id), ['core']);
  assert.deepEqual(areasTouching(reg, ['ui/a.ts']), []);
  assert.deepEqual(featuresTouching({}, ['x']), []);
});

test("the repository's own registry validates", () => {
  const registry = loadRegistry(repoRoot);
  assert.deepEqual(validateRegistry(registry, repoRoot), []);
});

import assert from 'node:assert/strict';
import { spawnSync, execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  changedPaths,
  codeSpan,
  evaluate,
  impactReport,
  parseDocsUnaffected,
  renderMarkdown,
  renderText,
  visibleLines,
} from './change-impact.mjs';

const script = join(dirname(fileURLToPath(import.meta.url)), 'change-impact.mjs');
const ids = (m) => [...m.keys()];

test('parseDocsUnaffected: every separator', () => {
  for (const sep of [' — ', ' – ', ' - ', ': ', '—']) {
    const m = parseDocsUnaffected(`Docs unaffected: alpha${sep}pure refactor`);
    assert.equal(m.get('alpha'), 'pure refactor', JSON.stringify(sep));
    assert.deepEqual(m.invalid, []);
  }
});

test('parseDocsUnaffected: id lists, lines, case, whitespace, markdown noise', () => {
  const body = [
    'intro text',
    'Docs unaffected: alpha, beta — one reason',
    '   docs UNAFFECTED:   gamma delta   —   another  ',
    '- **Docs unaffected:** `epsilon` — bulleted',
    '> Docs unaffected: zeta-two: quoted',
  ].join('\r\n');
  const m = parseDocsUnaffected(body);
  assert.deepEqual(ids(m), ['alpha', 'beta', 'gamma', 'delta', 'epsilon', 'zeta-two']);
  assert.equal(m.get('beta'), 'one reason');
  assert.equal(m.get('delta'), 'another');
  assert.equal(m.get('epsilon'), 'bulleted');
  assert.deepEqual(m.invalid, []);
});

test('parseDocsUnaffected: first reason for an id wins; hyphenated ids survive', () => {
  const m = parseDocsUnaffected('Docs unaffected: a-b — first\nDocs unaffected: a-b — second');
  assert.equal(m.get('a-b'), 'first');
});

test('parseDocsUnaffected: invalid lines land in .invalid and excuse nothing', () => {
  const m = parseDocsUnaffected(
    ['Docs unaffected: alpha —', 'Docs unaffected: * — everything', 'Docs unaffected: <ids> — <reason>', 'Docs unaffected: alpha', 'Docs unaffected: — why'].join('\n'),
  );
  assert.equal(m.size, 0);
  assert.equal(m.invalid.length, 5);
  assert.match(m.invalid[0], /empty reason/);
  assert.match(m.invalid[1], /not a feature\/area id \(\*\)/);
  assert.match(m.invalid[2], /not a feature\/area id/);
  assert.match(m.invalid[3], /no reason/);
  assert.match(m.invalid[4], /no ids/);
});

test('parseDocsUnaffected: a valid id beside a bad one still counts; no line, null body', () => {
  const m = parseDocsUnaffected('Docs unaffected: alpha, * — r');
  assert.deepEqual(ids(m), ['alpha']);
  assert.equal(m.invalid.length, 1);
  assert.equal(parseDocsUnaffected('nothing here').size, 0);
  assert.deepEqual(parseDocsUnaffected(null).invalid, []);
  assert.equal(parseDocsUnaffected('see Docs unaffected: a — b in prose').size, 0);
});

const registry = {
  areas: [{ id: 'core', map: 'core/AGENTS.md', paths: ['core/**'] }],
  features: [
    { id: 'alpha', name: 'Alpha', page: 'docs/features/alpha.md', paths: ['core/**', 'ui/a/**'] },
    { id: 'beta', name: 'Beta', page: 'docs/features/beta.md', paths: ['ui/b.ts'] },
  ],
};

test('impactReport: code hits and whether the doc moved', () => {
  const r = impactReport({ registry, changed: ['core/x.rs', 'ui/b.ts', 'unrelated.md'] });
  assert.deepEqual(r.features.map((f) => [f.id, f.pageChanged, f.hits]), [
    ['alpha', false, ['core/x.rs']],
    ['beta', false, ['ui/b.ts']],
  ]);
  assert.deepEqual(r.areas.map((a) => [a.id, a.mapChanged]), [['core', false]]);
  const withDocs = impactReport({ registry, changed: ['core/x.rs', 'docs/features/alpha.md', 'core/AGENTS.md'] });
  assert.equal(withDocs.features[0].pageChanged, true);
  assert.equal(withDocs.areas[0].mapChanged, true);
});

test('impactReport: a registered page or map alone is not a code hit', () => {
  // core/AGENTS.md sits under the core/** glob but is a registered map.
  const r = impactReport({ registry, changed: ['core/AGENTS.md', 'docs/features/alpha.md'] });
  assert.deepEqual(r, { features: [], areas: [] });
  assert.deepEqual(impactReport({ registry, changed: [] }), { features: [], areas: [] });
});

test('evaluate: ok when docs moved; failure names id and doc', () => {
  const ok = evaluate(impactReport({ registry, changed: ['core/x.rs', 'docs/features/alpha.md', 'core/AGENTS.md'] }), parseDocsUnaffected(''));
  assert.equal(ok.ok, true);
  const bad = evaluate(impactReport({ registry, changed: ['core/x.rs'] }), parseDocsUnaffected(''));
  assert.equal(bad.ok, false);
  assert.deepEqual(bad.failures.map((f) => `${f.kind}:${f.id}:${f.doc}`), ['feature:alpha:docs/features/alpha.md', 'area:core:core/AGENTS.md']);
});

test('evaluate: excuses, stale excuses, partial coverage', () => {
  const report = impactReport({ registry, changed: ['core/x.rs', 'ui/b.ts'] });
  const full = evaluate(report, parseDocsUnaffected('Docs unaffected: alpha, core, beta — internal'));
  assert.equal(full.ok, true);
  assert.equal(full.excused.length, 3);
  assert.equal(full.excused[0].reason, 'internal');
  const partial = evaluate(report, parseDocsUnaffected('Docs unaffected: alpha, core — internal'));
  assert.equal(partial.ok, false);
  assert.deepEqual(partial.failures.map((f) => f.id), ['beta']);
  const stale = evaluate(report, parseDocsUnaffected('Docs unaffected: alpha, core, beta, ghost — x'));
  assert.equal(stale.ok, true);
  assert.deepEqual(stale.warnings, ['stale excuse: ghost is not touched by this diff']);
  const inv = evaluate(report, parseDocsUnaffected('Docs unaffected: * — x'));
  assert.equal(inv.ok, false);
  assert.equal(inv.warnings.length, 1);
});

const failing = () => {
  const report = impactReport({ registry, changed: ['core/x.rs', 'core/y.rs', 'ui/b.ts'] });
  return [report, evaluate(report, parseDocsUnaffected(''))];
};

test('renderMarkdown: table, marks, paste line on failure', () => {
  const [report, verdict] = failing();
  const md = renderMarkdown(report, verdict);
  assert.ok(!md.includes('chimaera-change-impact'));
  assert.match(md, /### Change impact: ✗ 3 doc\(s\) to update or excuse/);
  assert.ok(md.includes('| feature `alpha` | `docs/features/alpha.md` | ✗ | `core/x.rs +1` |'));
  assert.ok(md.includes('| feature `beta` | `docs/features/beta.md` | ✗ | `ui/b.ts` |'));
  assert.ok(md.includes('Docs unaffected: alpha, beta, core — <reason>'));
});

test('renderMarkdown: ✓, excused, warnings, and the quiet case', () => {
  const report = impactReport({ registry, changed: ['core/x.rs', 'docs/features/alpha.md', 'ui/b.ts'] });
  const verdict = evaluate(report, parseDocsUnaffected('Docs unaffected: core, beta, ghost — fine | really'));
  const md = renderMarkdown(report, verdict);
  assert.match(md, /✓ docs in step/);
  assert.ok(md.includes('| feature `alpha` | `docs/features/alpha.md` | ✓ |'));
  assert.ok(md.includes('| excused |'));
  assert.ok(md.includes('- `core` — `fine \\| really`'));
  assert.ok(md.includes('stale excuse: ghost'));
  assert.ok(!md.includes('Docs unaffected: '));
  const none = impactReport({ registry, changed: ['README.md'] });
  assert.match(renderMarkdown(none, evaluate(none, new Map())), /✓ no registered feature or area touched/);
});

test('renderText mirrors the markdown', () => {
  const [report, verdict] = failing();
  const text = renderText(report, verdict);
  assert.match(text, /^✗ change impact: 3 doc\(s\)/);
  assert.match(text, /✗\s+feature alpha: docs\/features\/alpha\.md \(code: core\/x\.rs \+1\)/);
  assert.ok(text.includes('Docs unaffected: alpha, beta, core — <reason>'));
  const ok = impactReport({ registry, changed: ['core/x.rs'] });
  const v = evaluate(ok, parseDocsUnaffected('Docs unaffected: alpha, core — r'));
  const t = renderText(ok, v);
  assert.match(t, /^✓ change impact: docs in step/);
  assert.ok(t.includes('excused feature alpha') && t.includes('— r'));
  assert.equal(renderText({ features: [], areas: [] }, evaluate({ features: [], areas: [] }, new Map())), '✓ change impact: no registered feature or area touched\n');
});

// ---------------------------------------------------------------- git fixtures

function repo() {
  const root = mkdtempSync(join(tmpdir(), 'impact-test-'));
  const git = (...a) => execFileSync('git', ['-c', 'user.name=t', '-c', 'user.email=t@t', '-c', 'commit.gpgsign=false', ...a], { cwd: root, stdio: 'pipe', encoding: 'utf8' });
  const put = (rel, text) => {
    mkdirSync(dirname(join(root, rel)), { recursive: true });
    writeFileSync(join(root, rel), text);
  };
  git('init', '-q', '-b', 'main');
  return { root, git, put, cleanup: () => rmSync(root, { recursive: true, force: true }) };
}

test('changedPaths: adds, modifies, deletes, and both sides of a rename', () => {
  const r = repo();
  try {
    r.put('keep.txt', 'k\n');
    r.put('gone.txt', 'g\n');
    r.put('old/name.txt', 'a long enough body so rename detection fires\n'.repeat(5));
    r.git('add', '.');
    r.git('commit', '-qm', 'base');
    r.git('checkout', '-qb', 'work');
    mkdirSync(join(r.root, 'new'));
    r.git('mv', 'old/name.txt', 'new/name.txt');
    r.git('rm', '-q', 'gone.txt');
    r.put('keep.txt', 'changed\n');
    r.put('added.txt', 'x\n');
    r.git('add', '.');
    r.git('commit', '-qm', 'work');
    assert.deepEqual(changedPaths(r.root, 'main...HEAD'), ['added.txt', 'gone.txt', 'keep.txt', 'new/name.txt', 'old/name.txt']);
  } finally {
    r.cleanup();
  }
});

test('CLI end to end: exit 1 without an excuse, 0 with one via --body-env', () => {
  const r = repo();
  try {
    r.put('docs/features/registry.json', JSON.stringify({
      areas: [{ id: 'core', map: 'core/AGENTS.md', paths: ['core/**'] }],
      features: [{ id: 'alpha', name: 'Alpha', page: 'docs/features/alpha.md', paths: ['core/**'], maps: ['core/AGENTS.md'], rules: [], checks: ['x'] }],
    }));
    r.put('docs/features/alpha.md', '# Alpha\n');
    r.put('core/AGENTS.md', '# map\n');
    r.put('core/lib.rs', 'fn a() {}\n');
    r.git('add', '.');
    r.git('commit', '-qm', 'base');
    r.git('checkout', '-qb', 'work');
    r.put('core/lib.rs', 'fn b() {}\n');
    r.git('commit', '-qam', 'code only');
    const run = (env = {}, extra = []) =>
      spawnSync(process.execPath, [script, '--root', r.root, '--range', 'main...HEAD', '--format', 'json', ...extra], {
        encoding: 'utf8',
        env: { ...process.env, ...env },
      });
    const bare = run();
    assert.equal(bare.status, 1, bare.stderr);
    const json = JSON.parse(bare.stdout);
    assert.equal(json.changed, 1);
    assert.equal(json.verdict.ok, false);
    assert.deepEqual(json.verdict.failures.map((f) => f.id), ['alpha', 'core']);

    const excused = run({ PR_BODY: 'Docs unaffected: alpha, core — comment-level change' }, ['--body-env', 'PR_BODY']);
    assert.equal(excused.status, 0, excused.stderr);
    assert.equal(JSON.parse(excused.stdout).verdict.excused.length, 2);

    const unset = run({}, ['--body-env', 'NO_SUCH_VAR']);
    assert.equal(unset.status, 1);

    const md = spawnSync(process.execPath, [script, '--root', r.root, '--range', 'main...HEAD', '--format', 'markdown'], { encoding: 'utf8' });
    assert.equal(md.status, 1);
    assert.ok(md.stdout.includes('Docs unaffected: alpha, core — <reason>'));

    r.put('docs/features/alpha.md', '# Alpha\n\nmore\n');
    r.put('core/AGENTS.md', '# map\nmore\n');
    r.git('commit', '-qam', 'docs');
    const done = run();
    assert.equal(done.status, 0, done.stderr);
  } finally {
    r.cleanup();
  }
});

test('CLI: bad arguments and an unreadable range exit 2', () => {
  const bad = spawnSync(process.execPath, [script, '--nope'], { encoding: 'utf8' });
  assert.equal(bad.status, 2);
  const r = repo();
  try {
    r.put('a.txt', 'x');
    r.git('add', '.');
    r.git('commit', '-qm', 'i');
    const noRange = spawnSync(process.execPath, [script, '--root', r.root, '--range', 'nope...HEAD'], { encoding: 'utf8' });
    assert.equal(noRange.status, 2);
  } finally {
    r.cleanup();
  }
});

test('parseDocsUnaffected ignores excuses hidden in code fences and HTML comments', () => {
  const body = [
    '```',
    'Docs unaffected: alpha — hidden in a fence',
    '```',
    '<!-- Docs unaffected: beta — hidden in a comment -->',
    '<!--',
    'Docs unaffected: core — hidden in a multi-line comment',
    '-->',
    '~~~md',
    'Docs unaffected: ghost — tilde fence',
    '~~~',
    'Docs unaffected: ui — visible',
  ].join('\n');
  const excuses = parseDocsUnaffected(body);
  assert.deepEqual([...excuses.keys()], ['ui']);
  assert.equal(visibleLines(body).length, body.split('\n').length);
});

test('codeSpan neutralizes author-controlled text', () => {
  assert.equal(codeSpan('plain'), '`plain`');
  assert.equal(codeSpan('a | b'), '`a \\| b`');
  assert.equal(codeSpan('has `tick` inside'), '``has `tick` inside``');
  assert.equal(codeSpan('`edge`'), '`` `edge` ``');
  assert.equal(codeSpan('two\nlines'), '`two lines`');
  assert.equal(codeSpan(''), '` `');
  const md = renderMarkdown(
    impactReport({ registry, changed: ['core/x.rs'] }),
    evaluate(impactReport({ registry, changed: ['core/x.rs'] }), parseDocsUnaffected('Docs unaffected: core, alpha — see @someone <img src=x>')),
  );
  assert.ok(md.includes('`see @someone <img src=x>`'));
  // Every line that carries the mention is a code-span line, never bare prose.
  assert.ok(md.split('\n').filter((l) => l.includes('@someone')).every((l) => /`[^`]*@someone[^`]*`/.test(l)));
});

import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { checkRust, checkUi, parseCargoDeps, rustGraph, uiEdges } from './check-dependency-direction.mjs';

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), '..');
const sorted = (set) => [...set].sort();

test('parseCargoDeps: all dependency tables and forms', () => {
  const toml = `
[package]
name = "chimaera-x"

[dependencies]
chimaera-core = { path = "../chimaera-core" }
chimaera-pty.workspace = true
chimaera-agent = { version = "1", features = ["a"] }
serde = "1"
tokio = { version = "1", features = ["full"] }

[dev-dependencies]
chimaera-remote = { path = "../chimaera-remote" } # trailing comment

[build-dependencies]
chimaera-plugin-api = "0.1"

[target.'cfg(unix)'.dependencies]
chimaera-server.workspace = true
libc = "0.2"

[features]
chimaera-ignored = []
`;
  assert.deepEqual(sorted(parseCargoDeps(toml, 'crates/chimaera-x')), [
    'chimaera-agent',
    'chimaera-core',
    'chimaera-plugin-api',
    'chimaera-pty',
    'chimaera-remote',
    'chimaera-server',
  ]);
});

test('parseCargoDeps: external deps are ignored; non-chimaera key with a crates/ path counts', () => {
  const toml = `[dependencies]
serde = "1"
helper = { path = "../helper" }
vendored = { path = "../../vendor/thing" }
renamed = { package = "chimaera-core", path = "../chimaera-core" }
`;
  assert.deepEqual(sorted(parseCargoDeps(toml, 'crates/chimaera-x')), ['chimaera-core', 'helper']);
  assert.deepEqual(sorted(parseCargoDeps('[dependencies]\nserde = "1"\n')), []);
});

test('parseCargoDeps: sub-table form and dotted target tables', () => {
  const toml = `[dependencies.chimaera-core]
path = "../chimaera-core"

[target.'cfg(windows)'.dev-dependencies.chimaera-pty]
workspace = true

[dependencies.other]
version = "1"
`;
  assert.deepEqual(sorted(parseCargoDeps(toml)), ['chimaera-core', 'chimaera-pty']);
});

const graphOf = (edges, members) => {
  const names = ['chimaera', 'chimaera-core', 'chimaera-plugin-api', 'chimaera-pty', 'chimaera-agent', 'chimaera-remote', 'chimaera-server', 'chimaera-app'];
  const graph = new Map(names.map((n) => [n, new Set()]));
  for (const [from, to] of edges) graph.get(from).add(to);
  graph.members = new Set(members ?? names.filter((n) => n !== 'chimaera-app'));
  return graph;
};
const goodEdges = [
  ['chimaera', 'chimaera-server'],
  ['chimaera', 'chimaera-core'],
  ['chimaera-server', 'chimaera-agent'],
  ['chimaera-server', 'chimaera-pty'],
  ['chimaera-server', 'chimaera-remote'],
  ['chimaera-server', 'chimaera-core'],
  ['chimaera-server', 'chimaera-plugin-api'],
  ['chimaera-agent', 'chimaera-core'],
  ['chimaera-pty', 'chimaera-core'],
  ['chimaera-remote', 'chimaera-core'],
  ['chimaera-app', 'chimaera-core'],
  ['chimaera-app', 'chimaera-server'],
];

test('checkRust: the real graph shape passes', () => {
  assert.deepEqual(checkRust(graphOf(goodEdges)), []);
});

for (const [from, to, label] of [
  ['chimaera-core', 'chimaera-server', 'core → server'],
  ['chimaera-plugin-api', 'chimaera-core', 'plugin-api → core'],
  ['chimaera-pty', 'chimaera-server', 'pty → server'],
  ['chimaera-agent', 'chimaera-app', 'agent → app'],
  ['chimaera-remote', 'chimaera-server', 'remote → server'],
  ['chimaera-server', 'chimaera-app', 'server → app'],
  ['chimaera-server', 'chimaera', 'server → chimaera'],
]) {
  test(`checkRust: ${label} fails`, () => {
    const problems = checkRust(graphOf([...goodEdges, [from, to]]));
    assert.ok(problems.some((p) => p.startsWith(`${from} → ${to}:`)), problems.join('\n'));
  });
}

test('checkRust: app as a root workspace member fails', () => {
  const names = [...graphOf(goodEdges).keys()];
  const problems = checkRust(graphOf(goodEdges, names));
  assert.ok(problems.some((p) => p.includes('chimaera-app is a member of the root workspace')));
});

test('checkRust: a dependency on a crate that does not exist fails', () => {
  const problems = checkRust(graphOf([...goodEdges, ['chimaera-pty', 'chimaera-ghost']]));
  assert.ok(problems.some((p) => p.includes('chimaera-ghost: no crate of that name')));
});

test('checkRust: a member other than the app depending on the app fails', () => {
  const problems = checkRust(graphOf([...goodEdges, ['chimaera', 'chimaera-app']]));
  assert.ok(problems.some((p) => p.startsWith('chimaera → chimaera-app')));
});

// ------------------------------------------------------------------ UI fixtures

function uiTree(files) {
  const root = mkdtempSync(join(tmpdir(), 'depdir-test-'));
  for (const [rel, text] of Object.entries(files)) {
    const abs = join(root, 'web-ui/src/lib', rel);
    mkdirSync(dirname(abs), { recursive: true });
    writeFileSync(abs, text);
  }
  return root;
}
const L = (p) => `web-ui/src/lib/${p}`;
const withTree = (files, fn) => {
  const root = uiTree(files);
  try {
    return fn(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
};

test('uiEdges: static, re-export, import type, dynamic, component; resolution forms', () => {
  const edges = withTree(
    {
      'chat/a.ts': '',
      'chat/b.ts': '',
      'chat/c.svelte': '',
      'chat/d.svelte.ts': '',
      'chat/idx/index.ts': '',
      'chat/e.ts': '',
      'chat/f.ts': '',
      'shared/x.ts': `import { a } from '../chat/a';
export { b } from '../chat/b';
import type { E } from '../chat/e';
import C from '../chat/c.svelte';
import { d } from '../chat/d.svelte';
import { i } from '../chat/idx';
const lazy = () => import('../chat/f');
// import { no } from '../chat/ignored';
/* import { no2 } from '../chat/ignored2'; */
import ext from 'svelte';
import { same } from './sibling';
`,
      'shared/sibling.ts': '',
    },
    (root) => uiEdges(root),
  );
  const byTo = Object.fromEntries(edges.filter((e) => e.from === L('shared/x.ts')).map((e) => [e.to, e]));
  assert.deepEqual(Object.keys(byTo).sort(), [
    L('chat/a.ts'),
    L('chat/b.ts'),
    L('chat/c.svelte'),
    L('chat/d.svelte.ts'),
    L('chat/e.ts'),
    L('chat/f.ts'),
    L('chat/idx/index.ts'),
    L('shared/sibling.ts'),
  ]);
  assert.equal(byTo[L('chat/a.ts')].kind, 'static');
  assert.equal(byTo[L('chat/f.ts')].kind, 'dynamic');
  assert.equal(byTo[L('chat/c.svelte')].kind, 'component');
  assert.equal(byTo[L('chat/a.ts')].area, 'chat');
  assert.equal(byTo[L('shared/sibling.ts')].area, 'shared');
});

test('uiEdges: svelte script imports, net scanned, tests and fixtures skipped, static outranks dynamic', () => {
  const edges = withTree(
    {
      'chat/a.ts': '',
      'shared/comp.svelte': `<script lang="ts">\n  import { a } from '../chat/a';\n</script>\n<p>import x from '../chat/nope'</p>`,
      'net/n.ts': `const l = () => import('../chat/a');\nimport { a } from '../chat/a';`,
      'shared/x.test.ts': `import { a } from '../chat/a';`,
      'shared/x.fixture.json': `import '../chat/a'`,
      'previews/p.ts': `import { a } from '../chat/a';`,
    },
    (root) => uiEdges(root),
  );
  assert.deepEqual(
    edges.map((e) => `${e.from}>${e.to}:${e.kind}`),
    [`${L('net/n.ts')}>${L('chat/a.ts')}:static`, `${L('shared/comp.svelte')}>${L('chat/a.ts')}:static`],
  );
});

const edge = (from, to, kind = 'static') => ({ from: L(from), to: L(to), area: to.split('/')[0], kind });
const why = 'a reason';

test('checkUi: unlisted edge fails, listed passes, stale reported', () => {
  const edges = [edge('shared/x.ts', 'chat/a.ts'), edge('shared/y.ts', 'shared/z.ts')];
  assert.match(checkUi(edges, []).problems[0], /shared\/x\.ts → .*chat\/a\.ts \(static\): shared\/ imports from no feature area \(chat\/\); not in the exception list/);
  const listed = [{ from: L('shared/x.ts'), to: L('chat/a.ts'), why }];
  assert.deepEqual(checkUi(edges, listed), { problems: [], stale: [] });
  const stale = checkUi([], listed);
  assert.equal(stale.problems.length, 0);
  assert.match(stale.stale[0], /stale exception/);
  // An exception for an edge that is allowed anyway is stale too.
  const allowed = checkUi([edge('shared/y.ts', 'shared/z.ts')], [{ from: L('shared/y.ts'), to: L('shared/z.ts'), why }]);
  assert.equal(allowed.stale.length, 1);
});

test('checkUi: a trailing-slash exception covers a directory; others do not', () => {
  const edges = [edge('shared/x.ts', 'chat/a.ts'), edge('shared/x.ts', 'chat/sub/b.ts')];
  assert.deepEqual(checkUi(edges, [{ from: L('shared/x.ts'), to: L('chat/'), why }]), { problems: [], stale: [] });
  assert.equal(checkUi(edges, [{ from: L('shared/x.ts'), to: L('chat/a.ts'), why }]).problems.length, 1);
  assert.equal(checkUi(edges, [{ from: L('shared/other.ts'), to: L('chat/'), why }]).problems.length, 2);
});

test('checkUi: net → feature area and net → .svelte fail; a listed exception can excuse them', () => {
  const comp = edge('net/n.ts', 'shared/Banner.svelte', 'component');
  const feat = edge('net/n.ts', 'chat/a.ts');
  assert.match(checkUi([comp], []).problems[0], /net\/ imports no \.svelte component/);
  // The ban holds inside net/ too: it is transport, never a component tree.
  assert.match(checkUi([edge('net/n.ts', 'net/Local.svelte', 'component')], []).problems[0], /net\/ imports no \.svelte component/);
  assert.match(checkUi([feat], []).problems[0], /net\/ imports from no feature area/);
  const list = [
    { from: comp.from, to: comp.to, why },
    { from: feat.from, to: feat.to, why },
  ];
  assert.deepEqual(checkUi([comp, feat], list), { problems: [], stale: [] });
});

test('checkUi: shared → .svelte inside shared is fine; net → shared is fine', () => {
  assert.deepEqual(checkUi([edge('shared/x.ts', 'shared/C.svelte', 'component'), edge('net/n.ts', 'shared/s.ts')], []), { problems: [], stale: [] });
});

test('checkUi: malformed and duplicate exceptions', () => {
  const dup = checkUi([], [
    { from: 'a', to: 'b', why },
    { from: 'a', to: 'b', why },
  ]);
  assert.ok(dup.problems.some((p) => p.startsWith('duplicate exception')));
  assert.ok(checkUi([], [{ from: 'a', to: 'b', why: '  ' }]).problems.some((p) => p.includes('needs from, to and a why')));
});

test('rustGraph reads a fixture crates/ tree and root members', () => {
  const root = mkdtempSync(join(tmpdir(), 'depdir-rust-'));
  try {
    const crate = (dir, name, deps = '') => {
      mkdirSync(join(root, 'crates', dir), { recursive: true });
      writeFileSync(join(root, 'crates', dir, 'Cargo.toml'), `[package]\nname = "${name}"\n\n[dependencies]\n${deps}\n`);
    };
    crate('chimaera-core', 'chimaera-core');
    crate('chimaera-pty', 'chimaera-pty', 'chimaera-core = { path = "../chimaera-core" }');
    crate('chimaera-app', 'chimaera-app', 'chimaera-core.workspace = true');
    writeFileSync(join(root, 'Cargo.toml'), '[workspace]\nmembers = ["crates/*"]\nexclude = ["crates/chimaera-app"]\n');
    const g = rustGraph(root);
    assert.deepEqual(sorted(g.get('chimaera-pty')), ['chimaera-core']);
    assert.deepEqual(sorted(g.get('chimaera-core')), []);
    assert.ok(g.members.has('chimaera-pty'));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("the repository's own dependency direction holds", () => {
  assert.deepEqual(checkRust(rustGraph(repoRoot)), []);
  const file = join(repoRoot, 'scripts', 'dependency-direction-exceptions.json');
  const exceptions = existsSync(file) ? (JSON.parse(readFileSync(file, 'utf8')).ui ?? []) : [];
  const { problems, stale } = checkUi(uiEdges(repoRoot), exceptions);
  assert.deepEqual(problems, []);
  assert.deepEqual(stale, []);
});

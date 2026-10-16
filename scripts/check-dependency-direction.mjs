#!/usr/bin/env node
// Keep internal dependencies pointing the way the architecture says they do:
// Rust crates (static Cargo.toml parse, optional `cargo metadata` cross-check)
// and the web UI's leaf areas (shared/, net/). Every UI edge that breaks the
// rule today is a reasoned entry in dependency-direction-exceptions.json; an
// unlisted edge fails, and so does a listed edge that no longer exists, so the
// list only shrinks. Zero dependencies.
//
//   node scripts/check-dependency-direction.mjs [--root DIR] [--json] [--cargo]
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { basename, dirname, join, posix, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// ---------------------------------------------------------------- Rust

const DEP_TABLE = /^(?:target\.(?:'[^']*'|"[^"]*"|[^.]+)\.)?(?:dependencies|dev-dependencies|build-dependencies)$/;

/** Internal (chimaera) dependency names declared in one Cargo.toml.
 *  `manifestDir` is the manifest's repo-relative directory, used to decide
 *  whether a non-chimaera-named path dependency lands under crates/. */
export function parseCargoDeps(tomlText, manifestDir = 'crates/_') {
  const deps = new Set();
  let table = null; // the dependency table we are in, or null
  let subKey = null; // `[dependencies.<key>]` form
  let subBody = '';
  const flushSub = () => {
    if (subKey) addDep(subKey, subBody);
    subKey = null;
    subBody = '';
  };
  const addDep = (key, value) => {
    const pkg = value.match(/\bpackage\s*=\s*["']([^"']+)["']/)?.[1];
    const path = value.match(/\bpath\s*=\s*["']([^"']+)["']/)?.[1];
    const name = pkg ?? key;
    if (name.startsWith('chimaera')) {
      deps.add(name);
      return;
    }
    if (path) {
      const landed = posix.normalize(posix.join(manifestDir, path));
      if (landed.startsWith('crates/')) deps.add(posix.basename(landed));
    }
  };
  for (const raw of tomlText.split('\n')) {
    const line = raw.replace(/(^|\s)#.*$/, '').trim();
    if (!line) continue;
    const header = line.match(/^\[\s*([^\[\]]+?)\s*\]$/);
    if (header) {
      flushSub();
      const name = header[1].replace(/\s+/g, '');
      if (DEP_TABLE.test(name)) {
        table = name;
        continue;
      }
      // `[dependencies.chimaera-core]` / `[target.'cfg(unix)'.dependencies.x]`
      const sub = name.match(/^(.*\b(?:dependencies|dev-dependencies|build-dependencies))\.([A-Za-z0-9_-]+|"[^"]+")$/);
      if (sub && DEP_TABLE.test(sub[1])) {
        table = null;
        subKey = sub[2].replace(/"/g, '');
        continue;
      }
      table = null;
      continue;
    }
    if (line.startsWith('[[')) {
      flushSub();
      table = null;
      continue;
    }
    if (subKey) {
      subBody += ` ${line}`;
      continue;
    }
    if (!table) continue;
    const entry = line.match(/^("?)([A-Za-z0-9_-]+)\1((?:\.[A-Za-z0-9_-]+)*)\s*=\s*(.*)$/);
    if (entry) addDep(entry[2], `${entry[3]} = ${entry[4]}`);
  }
  flushSub();
  return deps;
}

const packageName = (tomlText, fallback) => {
  const pkg = tomlText.split(/^\[/m).find((s) => s.startsWith('package]'));
  return pkg?.match(/^\s*name\s*=\s*["']([^"']+)["']/m)?.[1] ?? fallback;
};

const crateDirs = (root) =>
  readdirSync(join(root, 'crates'), { withFileTypes: true })
    .filter((e) => e.isDirectory() && existsSync(join(root, 'crates', e.name, 'Cargo.toml')))
    .map((e) => e.name)
    .sort();

/** crate name → Set of internal crate names it depends on (every crates/*). */
export function rustGraph(root) {
  const byDir = new Map(); // dir name → package name, so path deps map to packages
  const texts = new Map();
  for (const dir of crateDirs(root)) {
    const text = readFileSync(join(root, 'crates', dir, 'Cargo.toml'), 'utf8');
    texts.set(dir, text);
    byDir.set(dir, packageName(text, dir));
  }
  const graph = new Map();
  for (const [dir, text] of texts) {
    const deps = [...parseCargoDeps(text, `crates/${dir}`)].map((d) => (byDir.has(d) && !d.startsWith('chimaera') ? byDir.get(d) : d));
    graph.set(byDir.get(dir), new Set(deps.sort()));
  }
  graph.members = rootMembers(root, byDir);
  return graph;
}

/** Package names of the root workspace's members (simple `*` globs expanded). */
function rootMembers(root, byDir) {
  const text = readFileSync(join(root, 'Cargo.toml'), 'utf8');
  const block = text.match(/^\s*members\s*=\s*\[([\s\S]*?)\]/m)?.[1] ?? '';
  const members = new Set();
  for (const [, member] of block.replace(/#.*$/gm, '').matchAll(/["']([^"']+)["']/g)) {
    if (member.includes('*')) {
      const re = new RegExp(`^${member.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '[^/]*')}$`);
      for (const dir of byDir.keys()) if (re.test(`crates/${dir}`)) members.add(byDir.get(dir));
    } else {
      const dir = member.replace(/^crates\//, '').replace(/\/$/, '');
      members.add(byDir.get(dir) ?? basename(member));
    }
  }
  return members;
}

const NO_INTERNAL = ['chimaera-core', 'chimaera-plugin-api'];
const FORBIDDEN = {
  'chimaera-pty': ['chimaera-server', 'chimaera-app'],
  'chimaera-agent': ['chimaera-server', 'chimaera-app'],
  'chimaera-remote': ['chimaera-server', 'chimaera-app'],
  'chimaera-server': ['chimaera-app', 'chimaera'],
};

/** Problems in a crate graph from rustGraph. */
export function checkRust(graph) {
  const problems = [];
  const members = graph.members ?? new Set([...graph.keys()].filter((c) => c !== 'chimaera-app'));
  for (const [crate, deps] of graph) {
    for (const dep of deps) {
      if (!graph.has(dep)) problems.push(`${crate} → ${dep}: no crate of that name under crates/`);
      if (NO_INTERNAL.includes(crate)) problems.push(`${crate} → ${dep}: ${crate} depends on no internal crate`);
      else if (FORBIDDEN[crate]?.includes(dep)) problems.push(`${crate} → ${dep}: ${crate} never depends on ${dep}`);
      else if (dep === 'chimaera-app' && members.has(crate))
        problems.push(`${crate} → chimaera-app: nothing in the daemon workspace depends on the native app`);
    }
  }
  if (members.has('chimaera-app')) problems.push('chimaera-app is a member of the root workspace (it must stay its own workspace)');
  return problems;
}

/** Cross-check the static graph against cargo's own view of the root workspace.
 *  Returns problems, or null when cargo is not on PATH. */
function cargoProblems(root, graph) {
  const run = spawnSync('cargo', ['metadata', '--no-deps', '--offline', '--format-version', '1'], {
    cwd: root,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  if (run.error?.code === 'ENOENT') return null;
  if (run.status !== 0) return [`cargo metadata failed: ${(run.stderr || '').trim().split('\n').pop()}`];
  const problems = [];
  const meta = JSON.parse(run.stdout);
  const seen = new Set();
  for (const pkg of meta.packages) {
    seen.add(pkg.name);
    const cargoDeps = new Set(pkg.dependencies.filter((d) => d.path || d.name.startsWith('chimaera')).map((d) => d.name));
    const staticDeps = graph.get(pkg.name);
    if (!staticDeps) {
      problems.push(`cargo sees workspace member ${pkg.name}, the static parse does not`);
      continue;
    }
    for (const d of cargoDeps) if (!staticDeps.has(d)) problems.push(`${pkg.name} → ${d}: cargo has it, the static parse missed it`);
    for (const d of staticDeps) if (!cargoDeps.has(d)) problems.push(`${pkg.name} → ${d}: the static parse has it, cargo does not`);
  }
  for (const member of graph.members ?? []) {
    if (!seen.has(member)) problems.push(`static parse lists root member ${member}, cargo does not`);
  }
  return problems;
}

// ---------------------------------------------------------------- UI

const UI_LIB = 'web-ui/src/lib';
const SCANNED = ['shared', 'net'];
const FEATURE_AREAS = ['chat', 'settings', 'dashboard', 'knowledge', 'plugins', 'previews', 'terminal', 'browser', 'workspace', 'layout'];
const RESOLVE_SUFFIXES = ['', '.ts', '.svelte', '.svelte.ts', '/index.ts'];

const walk = (dir) =>
  readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const path = join(dir, e.name);
    return e.isDirectory() ? walk(path) : [path];
  });

const scannedFile = (path) =>
  (path.endsWith('.ts') || path.endsWith('.svelte')) && !path.endsWith('.test.ts') && !path.endsWith('.fixture.json');

/** Code with comments blanked (strings kept, so specifiers survive). */
function stripComments(src) {
  let out = '';
  for (let i = 0; i < src.length; ) {
    const c = src[i];
    const n = src[i + 1];
    if (c === '/' && n === '/') {
      while (i < src.length && src[i] !== '\n') i++;
    } else if (c === '/' && n === '*') {
      const end = src.indexOf('*/', i + 2);
      i = end < 0 ? src.length : end + 2;
      out += ' ';
    } else if (c === '"' || c === "'" || c === '`') {
      let j = i + 1;
      while (j < src.length && src[j] !== c) j += src[j] === '\\' ? 2 : 1;
      out += src.slice(i, j + 1);
      i = j + 1;
    } else {
      out += c;
      i++;
    }
  }
  return out;
}

/** The script blocks of a .svelte file (where imports live). */
const svelteScripts = (src) => [...src.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)].map((m) => m[1]).join('\n');

/** [{ spec, dynamic }] for every import/re-export specifier in a source file. */
function specifiers(src, isSvelte) {
  const code = stripComments(isSvelte ? svelteScripts(src) : src);
  const found = [];
  const fromRe = /(?<![\w$.])(?:import|export)\s+(?:type\s+)?[^;'"`]*?\bfrom\s*(['"])([^'"]+)\1/g;
  const bareRe = /(?<![\w$.])import\s*(['"])([^'"]+)\1/g;
  const dynRe = /(?<![\w$.])import\s*\(\s*(['"`])([^'"`]+)\1\s*\)/g;
  for (const m of code.matchAll(fromRe)) found.push({ spec: m[2], dynamic: false });
  for (const m of code.matchAll(bareRe)) found.push({ spec: m[2], dynamic: false });
  for (const m of code.matchAll(dynRe)) found.push({ spec: m[2], dynamic: true });
  return found;
}

function resolveSpec(root, fromRel, spec) {
  const base = posix.normalize(posix.join(posix.dirname(fromRel), spec.replace(/[?#].*$/, '')));
  for (const suffix of RESOLVE_SUFFIXES) {
    const candidate = base + suffix;
    const abs = join(root, candidate);
    if (existsSync(abs) && statSync(abs).isFile()) return candidate;
  }
  return base;
}

/** The `web-ui/src/lib/<area>/` a path sits in; null for files directly in lib/. */
const areaOf = (rel) => {
  if (!rel.startsWith(`${UI_LIB}/`)) return null;
  const parts = rel.slice(UI_LIB.length + 1).split('/');
  return parts.length > 1 ? parts[0] : null;
};

/** Every internal edge out of shared/ and net/: [{from, to, area, kind}]. */
export function uiEdges(root) {
  const edges = new Map();
  for (const scanned of SCANNED) {
    const dir = join(root, UI_LIB, scanned);
    if (!existsSync(dir)) continue;
    for (const abs of walk(dir).filter(scannedFile).sort()) {
      const from = relative(root, abs).split('\\').join('/');
      for (const { spec, dynamic } of specifiers(readFileSync(abs, 'utf8'), from.endsWith('.svelte'))) {
        if (!spec.startsWith('.')) continue;
        const to = resolveSpec(root, from, spec);
        if (!to.startsWith(`${UI_LIB}/`)) continue;
        const kind = to.endsWith('.svelte') ? 'component' : dynamic ? 'dynamic' : 'static';
        const key = `${from}\0${to}`;
        const prior = edges.get(key);
        // One edge per file pair: a static import outranks a lazy one.
        if (!prior || (prior.kind === 'dynamic' && kind !== 'dynamic')) edges.set(key, { from, to, area: areaOf(to), kind });
      }
    }
  }
  return [...edges.values()].sort((a, b) => a.from.localeCompare(b.from) || a.to.localeCompare(b.to));
}

/** Why an edge breaks the rule, or null. */
function violation(edge) {
  const fromArea = areaOf(edge.from);
  // net/ is transport: no components even from its own directory.
  if (fromArea === 'net' && edge.kind === 'component') return 'net/ imports no .svelte component';
  if (edge.area === fromArea) return null;
  if (fromArea === 'shared' && FEATURE_AREAS.includes(edge.area)) return `shared/ imports from no feature area (${edge.area}/)`;
  if (fromArea === 'net' && FEATURE_AREAS.includes(edge.area)) return `net/ imports from no feature area (${edge.area}/)`;
  return null;
}

const covers = (exception, edge) =>
  exception.from === edge.from && (exception.to === edge.to || (exception.to.endsWith('/') && edge.to.startsWith(exception.to)));

/** { problems, stale }: unlisted violating edges, and listed exceptions matching none. */
export function checkUi(edges, exceptions) {
  const problems = [];
  const used = new Set();
  const keys = new Set();
  exceptions.forEach((exception, i) => {
    if (!exception.from || !exception.to || !exception.why?.trim()) problems.push(`exception #${i + 1} needs from, to and a why`);
    const key = `${exception.from} → ${exception.to}`;
    if (keys.has(key)) problems.push(`duplicate exception: ${key}`);
    keys.add(key);
  });
  for (const edge of edges) {
    const why = violation(edge);
    if (!why) continue;
    const i = exceptions.findIndex((exception) => covers(exception, edge));
    if (i < 0) problems.push(`${edge.from} → ${edge.to} (${edge.kind}): ${why}; not in the exception list`);
    else used.add(i);
  }
  const stale = exceptions
    .filter((_, i) => !used.has(i))
    .map((e) => `stale exception, the edge is gone or allowed: ${e.from} → ${e.to} (remove it from the list)`);
  return { problems, stale };
}

// ---------------------------------------------------------------- main

function main(argv) {
  const arg = (name) => argv.includes(name);
  const rootAt = argv.indexOf('--root');
  const root = rootAt >= 0 ? resolve(argv[rootAt + 1]) : resolve(dirname(fileURLToPath(import.meta.url)), '..');

  const graph = rustGraph(root);
  const edges = uiEdges(root);
  const exceptionsPath = join(root, 'scripts', 'dependency-direction-exceptions.json');
  const exceptions = existsSync(exceptionsPath) ? (JSON.parse(readFileSync(exceptionsPath, 'utf8')).ui ?? []) : [];

  if (arg('--json')) {
    const rust = Object.fromEntries([...graph].map(([crate, deps]) => [crate, [...deps]]));
    console.log(JSON.stringify({ rust, ui: edges }, null, 2));
    return;
  }

  const problems = checkRust(graph);
  if (arg('--cargo')) {
    const fromCargo = cargoProblems(root, graph);
    if (fromCargo === null) console.log('· cargo not on PATH; skipped the cargo metadata cross-check');
    else problems.push(...fromCargo);
  }
  const ui = checkUi(edges, exceptions);
  problems.push(...ui.problems, ...ui.stale);

  if (problems.length) {
    console.error(`✗ ${problems.length} dependency direction problem(s):`);
    for (const problem of problems) console.error(`  ${problem}`);
    process.exit(1);
  }
  console.log(`✓ dependency direction OK (${graph.size} crates, ${edges.length} UI edges, ${exceptions.length} exceptions)`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main(process.argv.slice(2));

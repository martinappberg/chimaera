#!/usr/bin/env node
// Keep docs/features/registry.json true: every feature page has an entry, every
// entry points at real code, maps, rules and checks, and the README's routing
// table is the one the registry produces. Zero dependencies; the file universe is
// tracked plus untracked-but-not-ignored files, so a new file counts before commit.
//
//   node scripts/check-feature-registry.mjs [--root <dir>] [--write] [--json]
import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, posix, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const REGISTRY_PATH = 'docs/features/registry.json';
export const README_PATH = 'docs/features/README.md';
export const FEATURES_DIR = 'docs/features';
export const BLOCK_START = '<!-- registry:start -->';
export const BLOCK_END = '<!-- registry:end -->';
export const BLOCK_HEADING = '## Where each feature is wired';

const TOP_KEYS = ['areas', 'features'];
const AREA_KEYS = ['id', 'map', 'paths'];
const FEATURE_KEYS = ['id', 'name', 'page', 'paths', 'maps', 'rules', 'checks'];
const ID_RE = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

const byId = (a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);

// Anchored at both ends. `**/` spans zero or more whole segments, a trailing `/**`
// also matches the directory itself, `*`/`?` stay inside one segment, and `[…]`
// passes through as a character class. Every consumer must share this conversion.
export function globToRegExp(glob) {
  let out = '';
  let i = 0;
  while (i < glob.length) {
    if (glob.startsWith('**/', i)) {
      out += '(?:.*/)?';
      i += 3;
    } else if (glob.startsWith('/**', i) && i + 3 === glob.length) {
      out += '(?:/.*)?';
      i += 3;
    } else if (glob.startsWith('**', i)) {
      out += '.*';
      i += 2;
    } else if (glob[i] === '*') {
      out += '[^/]*';
      i += 1;
    } else if (glob[i] === '?') {
      out += '[^/]';
      i += 1;
    } else if (glob[i] === '[') {
      const close = glob.indexOf(']', i + 1);
      if (close === -1) {
        out += '\\[';
        i += 1;
      } else {
        out += glob.slice(i, close + 1);
        i = close + 1;
      }
    } else {
      out += glob[i].replace(/[.+^${}()|\\\]/]/g, '\\$&');
      i += 1;
    }
  }
  return new RegExp(`^${out}$`);
}

export function matchesAny(path, globs) {
  return globs.some((glob) => globToRegExp(glob).test(path));
}

export function listFiles(root) {
  const out = execFileSync('git', ['ls-files', '-z', '-co', '--exclude-standard'], {
    cwd: root,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  // A tracked file deleted from the working tree is not a file a glob can point at.
  return [...new Set(out.split('\0').filter(Boolean))].filter((f) => existsSync(join(root, f))).sort();
}

export function loadRegistry(root) {
  const file = join(root, REGISTRY_PATH);
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch (error) {
    throw new Error(`cannot read ${REGISTRY_PATH}: ${error.message}`);
  }
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new Error(`${REGISTRY_PATH} is not valid JSON: ${error.message}`);
  }
}

const isStringArray = (value) => Array.isArray(value) && value.every((v) => typeof v === 'string');

const pageTitle = (text) => {
  const line = text.split('\n').find((l) => l.startsWith('# '));
  return line ? line.slice(2).trim() : null;
};

const featurePages = (root) => {
  const dir = join(root, FEATURES_DIR);
  if (!existsSync(dir)) return [];
  return readdirSync(dir, { withFileTypes: true })
    .filter((e) => e.isFile() && e.name.endsWith('.md') && e.name !== 'README.md')
    .map((e) => `${FEATURES_DIR}/${e.name}`)
    .sort();
};

const fileExists = (root, rel) => typeof rel === 'string' && rel !== '' && existsSync(join(root, rel));

function checkKeys(where, entry, allowed, problems) {
  if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
    problems.push(`${where}: must be an object`);
    return false;
  }
  for (const key of Object.keys(entry)) {
    if (!allowed.includes(key)) problems.push(`${where}: unknown key "${key}"`);
  }
  for (const key of allowed) {
    if (!(key in entry)) problems.push(`${where}: missing key "${key}"`);
  }
  return true;
}

function checkGlobs(where, globs, files, problems) {
  if (!isStringArray(globs)) {
    problems.push(`${where}: paths must be an array of strings`);
    return;
  }
  if (globs.length === 0) problems.push(`${where}: paths is empty`);
  for (const glob of globs) {
    if (glob === '' || glob.startsWith('./') || glob.startsWith('/') || glob.endsWith('/')) {
      problems.push(`${where}: malformed glob "${glob}" (no leading ./ or /, no trailing /; write a directory as dir/**)`);
      continue;
    }
    const re = globToRegExp(glob);
    if (!files.some((f) => re.test(f))) problems.push(`${where}: glob "${glob}" matches no file`);
  }
}

// Returns problems sorted, so output is stable however the registry is ordered.
// `options.files` overrides the git file universe (for fixtures without git).
export function validateRegistry(registry, root, options = {}) {
  const problems = [];
  const files = options.files ?? listFiles(root);
  if (!checkKeys('registry', registry, TOP_KEYS, problems)) return problems;
  const areas = Array.isArray(registry.areas) ? registry.areas : [];
  const features = Array.isArray(registry.features) ? registry.features : [];
  if (!Array.isArray(registry.areas)) problems.push('registry: areas must be an array');
  if (!Array.isArray(registry.features)) problems.push('registry: features must be an array');

  const seen = new Map();
  const noteId = (kind, id) => {
    if (typeof id !== 'string' || !ID_RE.test(id)) {
      problems.push(`${kind} "${id}": id must be kebab-case`);
      return;
    }
    if (seen.has(id)) problems.push(`duplicate id "${id}" (${seen.get(id)} and ${kind})`);
    else seen.set(id, kind);
  };

  for (const [index, area] of areas.entries()) {
    const where = `area "${area?.id ?? `#${index}`}"`;
    if (!checkKeys(where, area, AREA_KEYS, problems)) continue;
    noteId('area', area.id);
    if (!fileExists(root, area.map)) problems.push(`${where}: map ${area.map} does not exist`);
    checkGlobs(where, area.paths, files, problems);
  }

  const pages = featurePages(root);
  const covered = new Set();
  for (const [index, feature] of features.entries()) {
    const where = `feature "${feature?.id ?? `#${index}`}"`;
    if (!checkKeys(where, feature, FEATURE_KEYS, problems)) continue;
    noteId('feature', feature.id);
    covered.add(feature.page);
    if (typeof feature.page !== 'string' || !feature.page.startsWith(`${FEATURES_DIR}/`)) {
      problems.push(`${where}: page must be under ${FEATURES_DIR}/`);
    } else if (!fileExists(root, feature.page)) {
      problems.push(`${where}: page ${feature.page} does not exist`);
    } else {
      const stem = posix.basename(feature.page, '.md');
      if (feature.id !== stem) problems.push(`${where}: id must equal the page stem "${stem}"`);
      const title = pageTitle(readFileSync(join(root, feature.page), 'utf8'));
      if (feature.name !== title) problems.push(`${where}: name "${feature.name}" differs from the page's H1 "${title}"`);
    }
    checkGlobs(where, feature.paths, files, problems);
    for (const key of ['maps', 'rules', 'checks']) {
      if (!isStringArray(feature[key])) {
        problems.push(`${where}: ${key} must be an array of strings`);
        continue;
      }
      if (key !== 'rules' && feature[key].length === 0) problems.push(`${where}: ${key} is empty`);
    }
    for (const map of isStringArray(feature.maps) ? feature.maps : []) {
      if (!fileExists(root, map)) problems.push(`${where}: map ${map} does not exist`);
    }
    for (const rule of isStringArray(feature.rules) ? feature.rules : []) {
      if (!rule.startsWith('.claude/rules/')) problems.push(`${where}: rule ${rule} is not under .claude/rules/`);
      else if (!fileExists(root, rule)) problems.push(`${where}: rule ${rule} does not exist`);
    }
  }

  for (const page of pages) {
    if (!covered.has(page)) problems.push(`page ${page} has no entry in ${REGISTRY_PATH}`);
  }

  const readme = join(root, README_PATH);
  if (!existsSync(readme)) {
    problems.push(`${README_PATH} does not exist`);
  } else {
    const current = extractRoutingBlock(readFileSync(readme, 'utf8'));
    if (current === null) {
      problems.push(`${README_PATH}: no ${BLOCK_START} … ${BLOCK_END} block; run with --write`);
    } else if (current !== renderRoutingTable(registry)) {
      problems.push(`${README_PATH}: the routing block differs from ${REGISTRY_PATH}; run with --write (never hand-edit it)`);
    }
  }
  return problems.sort();
}

const relFromReadme = (target) => posix.relative(FEATURES_DIR, target);
const cell = (text) => text.replaceAll('|', '\\|');
const code = (text) => `\`${cell(text)}\``;

// The block from BLOCK_START through BLOCK_END inclusive; a pure function of the
// registry so --write and the check agree byte for byte.
export function renderRoutingTable(registry) {
  const mapNames = new Map((registry.areas ?? []).map((a) => [a.map, a.id]));
  const rows = [...(registry.features ?? [])].sort(byId).map((f) => {
    const page = `[${cell(f.name)}](${relFromReadme(f.page)})`;
    const paths = (f.paths ?? []).map(code).join(', ');
    const maps = (f.maps ?? []).map((m) => `[${cell(mapNames.get(m) ?? m)}](${relFromReadme(m)})`).join(', ');
    const rules = (f.rules ?? []).map((r) => `[${posix.basename(r, '.md')}](${relFromReadme(r)})`).join(', ');
    const checks = (f.checks ?? []).map(code).join(', ');
    return `| ${page} | ${paths} | ${maps} | ${rules || '—'} | ${checks} |`;
  });
  return [
    BLOCK_START,
    `Generated from [registry.json](registry.json) by \`node scripts/check-feature-registry.mjs --write\`; do not hand-edit it. Code globs are the entry points whose change usually wants the page updated.`,
    '',
    '| Feature | Code | Maps | Rules | Checks |',
    '|---|---|---|---|---|',
    ...rows,
    BLOCK_END,
  ].join('\n');
}

export function extractRoutingBlock(readme) {
  const start = readme.indexOf(BLOCK_START);
  const end = readme.indexOf(BLOCK_END);
  if (start === -1 || end === -1 || end < start) return null;
  return readme.slice(start, end + BLOCK_END.length);
}

// Replaces the existing block, or inserts the section after "## The pages".
export function writeRoutingBlock(readme, registry) {
  const block = renderRoutingTable(registry);
  const current = extractRoutingBlock(readme);
  if (current !== null) return readme.replace(current, () => block);
  const pages = readme.indexOf('\n## The pages');
  const next = pages === -1 ? -1 : readme.indexOf('\n## ', pages + 1);
  const section = `${BLOCK_HEADING}\n\n${block}\n`;
  if (next === -1) return `${readme.replace(/\n*$/, '\n\n')}${section}`;
  return `${readme.slice(0, next + 1)}${section}\n${readme.slice(next + 1)}`;
}

const touching = (entries, paths) =>
  [...entries].filter((e) => paths.some((p) => matchesAny(p, e.paths ?? []))).sort(byId);

export function featuresTouching(registry, paths) {
  return touching(registry.features ?? [], paths);
}

export function areasTouching(registry, paths) {
  return touching(registry.areas ?? [], paths);
}

function defaultRoot() {
  const here = dirname(fileURLToPath(import.meta.url));
  try {
    return execFileSync('git', ['rev-parse', '--show-toplevel'], { cwd: here, encoding: 'utf8' }).trim();
  } catch {
    return resolve(here, '..');
  }
}

function parseArgs(argv) {
  const args = { root: null, write: false, json: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--root') args.root = argv[++i];
    else if (arg.startsWith('--root=')) args.root = arg.slice('--root='.length);
    else if (arg === '--write') args.write = true;
    else if (arg === '--json') args.json = true;
    else throw new Error(`unknown argument: ${arg}`);
  }
  if (args.root === undefined || args.root === '') throw new Error('--root needs a directory');
  return args;
}

function fail(problems) {
  console.error(`✗ ${problems.length} feature registry problem(s):`);
  for (const problem of problems) console.error(`  ${problem}`);
  process.exit(1);
}

function main() {
  let args;
  try {
    args = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.error(`✗ ${error.message}`);
    console.error('usage: node scripts/check-feature-registry.mjs [--root <dir>] [--write] [--json]');
    process.exit(2);
  }
  const root = resolve(args.root ?? defaultRoot());
  let registry;
  try {
    registry = loadRegistry(root);
  } catch (error) {
    fail([error.message]);
  }
  if (args.json) {
    console.log(JSON.stringify(registry, null, 2));
    return;
  }
  if (args.write) {
    const readme = join(root, README_PATH);
    const before = readFileSync(readme, 'utf8');
    const after = writeRoutingBlock(before, registry);
    if (after !== before) writeFileSync(readme, after);
    console.log(after === before ? `✓ ${README_PATH} routing block already current` : `✓ wrote the routing block in ${README_PATH}`);
  }
  const problems = validateRegistry(registry, root);
  if (problems.length) fail(problems);
  console.log(`✓ feature registry OK (${registry.features.length} features, ${registry.areas.length} areas)`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();

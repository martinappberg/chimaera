#!/usr/bin/env node
// Change impact: which registered features and areas a diff touches, and whether
// each one's page or map moved with the code. The author updates the doc or says
// why behavior did not change (`Docs unaffected: <ids> — <reason>` in the PR
// body); a blanket `*` is refused so every excuse names what it covers. Zero
// dependencies. The glob conversion mirrors scripts/check-feature-registry.mjs
// on purpose (that script is not imported, so this one runs even when the
// registry checker is broken); both follow the same written rule.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const REGISTRY_PATH = 'docs/features/registry.json';
const ID = /^[a-z0-9][a-z0-9-]*$/;
// Leading list/quote markers and emphasis are tolerated so the line survives
// whatever template formatting the author pastes it into.
const EXCUSE_LINE = /^\s*(?:>\s*)*(?:[-*+]\s+)?(?:\*\*|__)?docs unaffected(?:\*\*|__)?\s*:\s*(?:\*\*|__)?\s*(.*)$/i;
// Ids are kebab-case, so a hyphen only separates when spaced.
const SEPARATOR = /\s*[—–]\s*|\s+-\s+|:\s+/;

export function globToRegExp(glob) {
  let out = '';
  for (let i = 0; i < glob.length; ) {
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
    } else if (glob[i] === '[' && glob.indexOf(']', i + 1) !== -1) {
      // A class passes through whole; an unclosed `[` is a literal.
      const close = glob.indexOf(']', i + 1);
      out += glob.slice(i, close + 1);
      i = close + 1;
    } else {
      out += glob[i].replace(/[.+^${}()|[\]\\]/g, '\\$&');
      i += 1;
    }
  }
  return new RegExp(`^${out}$`);
}

export function matchesAny(path, globs) {
  return globs.some((glob) => globToRegExp(glob).test(path));
}

// Returns Map id → reason. Lines that look like an excuse but cannot be used
// ride along as `.invalid` (strings explaining why) so they surface as warnings.
// Fenced code and HTML comments are invisible to a reviewer reading the PR, so
// an excuse there does not count; blanked line by line so nothing else shifts.
export function visibleLines(body) {
  const text = String(body ?? '').replace(/<!--[\s\S]*?-->/g, (m) => m.replace(/[^\n]/g, ''));
  let fence = null;
  return text.split(/\r?\n/).map((line) => {
    const run = /^\s{0,3}(`{3,}|~{3,})/.exec(line)?.[1];
    if (fence !== null) {
      if (run && run[0] === fence[0] && run.length >= fence.length) fence = null;
      return '';
    }
    if (run) {
      fence = run;
      return '';
    }
    return line;
  });
}

export function parseDocsUnaffected(body) {
  const excuses = new Map();
  excuses.invalid = [];
  for (const raw of visibleLines(body)) {
    const match = raw.match(EXCUSE_LINE);
    if (!match) continue;
    const line = raw.trim();
    const rest = match[1].replace(/(?:\*\*|__)\s*$/, '').trim();
    const sep = rest.match(SEPARATOR);
    if (!sep) {
      excuses.invalid.push(`no reason after the ids: ${line}`);
      continue;
    }
    const reason = rest.slice(sep.index + sep[0].length).trim();
    const ids = rest
      .slice(0, sep.index)
      .replaceAll('`', '')
      .split(/[\s,]+/)
      .filter(Boolean);
    if (!reason) {
      excuses.invalid.push(`empty reason: ${line}`);
      continue;
    }
    if (ids.length === 0) {
      excuses.invalid.push(`no ids: ${line}`);
      continue;
    }
    const bad = ids.filter((id) => !ID.test(id));
    if (bad.length) {
      excuses.invalid.push(`not a feature/area id (${bad.join(', ')}): ${line}`);
    }
    for (const id of ids) if (ID.test(id) && !excuses.has(id)) excuses.set(id, reason);
  }
  return excuses;
}

export function impactReport({ registry, changed }) {
  const inDiff = new Set(changed);
  // A registered doc is never itself the code that demands a doc update, or
  // editing a map would flag the feature whose glob happens to cover it.
  const docs = new Set([
    ...(registry.features ?? []).map((f) => f.page),
    ...(registry.areas ?? []).map((a) => a.map),
  ]);
  const code = changed.filter((path) => !docs.has(path));
  const hitsFor = (globs) => {
    const regexes = (globs ?? []).map(globToRegExp);
    return code.filter((path) => regexes.some((re) => re.test(path)));
  };
  const features = [];
  for (const feature of registry.features ?? []) {
    const hits = hitsFor(feature.paths);
    if (hits.length === 0) continue;
    features.push({
      id: feature.id,
      name: feature.name,
      page: feature.page,
      pageChanged: inDiff.has(feature.page),
      hits,
    });
  }
  const areas = [];
  for (const area of registry.areas ?? []) {
    const hits = hitsFor(area.paths);
    if (hits.length === 0) continue;
    areas.push({ id: area.id, map: area.map, mapChanged: inDiff.has(area.map), hits });
  }
  return { features, areas };
}

// An id excuses every touched entry carrying it (a feature and an area may
// share an id; one line covers both, which is what the author would expect).
export function evaluate(report, excuses) {
  const failures = [];
  const excused = [];
  const warnings = [...(excuses.invalid ?? [])];
  const entries = [
    ...report.features.map((f) => ({ kind: 'feature', id: f.id, doc: f.page, updated: f.pageChanged, hits: f.hits })),
    ...report.areas.map((a) => ({ kind: 'area', id: a.id, doc: a.map, updated: a.mapChanged, hits: a.hits })),
  ];
  for (const entry of entries) {
    if (entry.updated) continue;
    if (excuses.has(entry.id)) excused.push({ ...entry, reason: excuses.get(entry.id) });
    else failures.push(entry);
  }
  const touched = new Set(entries.map((entry) => entry.id));
  for (const id of excuses.keys()) {
    if (!touched.has(id)) warnings.push(`stale excuse: ${id} is not touched by this diff`);
  }
  return { ok: failures.length === 0, failures, warnings, excused };
}

function rows(report, verdict) {
  const excusedIds = new Set(verdict.excused.map((entry) => `${entry.kind}:${entry.id}`));
  const toRow = (kind, id, doc, updated, hits) => ({
    kind,
    id,
    doc,
    mark: updated ? '✓' : excusedIds.has(`${kind}:${id}`) ? 'excused' : '✗',
    hits,
  });
  return [
    ...report.features.map((f) => toRow('feature', f.id, f.page, f.pageChanged, f.hits)),
    ...report.areas.map((a) => toRow('area', a.id, a.map, a.mapChanged, a.hits)),
  ];
}

const sample = (hits) => (hits.length === 1 ? hits[0] : `${hits[0]} +${hits.length - 1}`);
// A code span neutralizes @mentions, links and HTML in author-controlled text. The
// fence is one backtick longer than any run inside, padded when the text itself
// starts or ends with one; newlines and pipes cannot survive a table cell.
export function codeSpan(text) {
  const flat = String(text).replace(/[\r\n]+/g, ' ').replaceAll('|', '\\|');
  const longest = Math.max(0, ...[...flat.matchAll(/`+/g)].map((m) => m[0].length));
  const fence = '`'.repeat(longest + 1);
  const pad = /^`|`$/.test(flat) ? ' ' : '';
  return `${fence}${pad}${flat || ' '}${pad}${fence}`;
}
const pasteLine = (verdict) =>
  `Docs unaffected: ${[...new Set(verdict.failures.map((f) => f.id))].join(', ')} — <reason>`;

export function renderMarkdown(report, verdict) {
  const all = rows(report, verdict);
  const lines = [];
  if (all.length === 0) {
    lines.push('### Change impact: ✓ no registered feature or area touched');
    return `${lines.join('\n')}\n`;
  }
  lines.push(
    verdict.ok
      ? '### Change impact: ✓ docs in step'
      : `### Change impact: ✗ ${verdict.failures.length} doc(s) to update or excuse`,
    '',
    '| Touched | Doc | Updated | Code |',
    '|---|---|---|---|',
    ...all.map((r) => `| ${r.kind} ${codeSpan(r.id)} | ${codeSpan(r.doc)} | ${r.mark} | ${codeSpan(sample(r.hits))} |`),
  );
  if (verdict.excused.length) {
    lines.push('', 'Excused:', ...verdict.excused.map((e) => `- ${codeSpan(e.id)} — ${codeSpan(e.reason)}`));
  }
  if (verdict.warnings.length) {
    lines.push('', 'Warnings:', ...verdict.warnings.map((w) => `- ${codeSpan(w)}`));
  }
  if (!verdict.ok) {
    lines.push(
      '',
      "Update each ✗ doc, or if this change leaves its behavior as documented, add this line to the PR description:",
      '',
      '```',
      pasteLine(verdict),
      '```',
    );
  }
  return `${lines.join('\n')}\n`;
}

export function renderText(report, verdict) {
  const all = rows(report, verdict);
  const lines = [];
  if (all.length === 0) return '✓ change impact: no registered feature or area touched\n';
  const reasons = new Map(verdict.excused.map((e) => [`${e.kind}:${e.id}`, e.reason]));
  for (const r of all) {
    const reason = reasons.get(`${r.kind}:${r.id}`);
    lines.push(`  ${r.mark.padEnd(7)} ${r.kind} ${r.id}: ${r.doc} (code: ${sample(r.hits)})${reason ? ` — ${reason}` : ''}`);
  }
  for (const w of verdict.warnings) lines.push(`  warning: ${w}`);
  if (verdict.ok) return `✓ change impact: docs in step\n${lines.join('\n')}\n`;
  lines.push('', `  Update each ✗ doc, or add to the PR description:`, `  ${pasteLine(verdict)}`);
  return `✗ change impact: ${verdict.failures.length} doc(s) to update or excuse\n${lines.join('\n')}\n`;
}

const git = (root, args) =>
  execFileSync('git', args, {
    cwd: root,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'pipe'],
  });

export function defaultRange(root) {
  try {
    git(root, ['rev-parse', '--verify', '--quiet', 'origin/main']);
    return 'origin/main...HEAD';
  } catch {
    return 'main...HEAD';
  }
}

// Both sides of a rename count: moving code out of an area changes that area.
export function changedPaths(root, range) {
  const out = git(root, ['diff', '--name-status', '-M', '-z', '--diff-filter=ACMRDT', range, '--']);
  const fields = out.split('\0');
  const paths = new Set();
  for (let i = 0; i < fields.length && fields[i]; ) {
    const status = fields[i];
    if (status.startsWith('R') || status.startsWith('C')) {
      paths.add(fields[i + 1]);
      paths.add(fields[i + 2]);
      i += 3;
    } else {
      paths.add(fields[i + 1]);
      i += 2;
    }
  }
  return [...paths].sort();
}

export function loadRegistry(root) {
  return JSON.parse(readFileSync(join(root, REGISTRY_PATH), 'utf8'));
}

function parseArgs(argv) {
  const opts = { format: 'text' };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    const value = () => {
      if (i + 1 >= argv.length) throw new Error(`${flag} needs a value`);
      return argv[++i];
    };
    if (flag === '--range') opts.range = value();
    else if (flag === '--body') opts.bodyFile = value();
    else if (flag === '--body-env') opts.bodyEnv = value();
    else if (flag === '--format') opts.format = value();
    else if (flag === '--root') opts.root = resolve(value());
    else throw new Error(`unknown argument: ${flag}`);
  }
  if (!['text', 'markdown', 'json'].includes(opts.format)) throw new Error(`unknown --format: ${opts.format}`);
  if (opts.bodyFile && opts.bodyEnv) throw new Error('pass --body or --body-env, not both');
  return opts;
}

export function main(argv = process.argv.slice(2)) {
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (error) {
    console.error(`✗ ${error.message}`);
    console.error('usage: change-impact.mjs [--range <a>...<b>] [--body <file>|--body-env <VAR>] [--format text|markdown|json] [--root <dir>]');
    return 2;
  }
  try {
    const root =
      opts.root ?? git(dirname(fileURLToPath(import.meta.url)), ['rev-parse', '--show-toplevel']).trim();
    const range = opts.range ?? defaultRange(root);
    const body = opts.bodyFile
      ? readFileSync(opts.bodyFile, 'utf8')
      : opts.bodyEnv
        ? (process.env[opts.bodyEnv] ?? '')
        : '';
    const changed = changedPaths(root, range);
    const report = impactReport({ registry: loadRegistry(root), changed });
    const verdict = evaluate(report, parseDocsUnaffected(body));
    if (opts.format === 'json') {
      process.stdout.write(`${JSON.stringify({ range, changed: changed.length, ...report, verdict }, null, 2)}\n`);
    } else if (opts.format === 'markdown') {
      process.stdout.write(renderMarkdown(report, verdict));
    } else {
      (verdict.ok ? process.stdout : process.stderr).write(renderText(report, verdict));
    }
    return verdict.ok ? 0 : 1;
  } catch (error) {
    console.error(`✗ change impact could not run: ${error.message}`);
    // The comment must say so too, or the PR keeps the previous run's verdict.
    if (opts.format === 'markdown') {
      process.stdout.write(`### Change impact: ✗ could not run\n\n${codeSpan(error.message)}\n`);
    }
    return 2;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = main();
}

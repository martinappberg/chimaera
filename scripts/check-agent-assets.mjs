#!/usr/bin/env node
// Keep the lightweight Codex-facing bridges aligned with Claude's canonical
// skills and role prompts. Zero dependencies; checks tracked and untracked files.
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const problems = [];

const directoryNames = (path) =>
  readdirSync(path, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();

const fileStems = (path, extension) =>
  readdirSync(path, { withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(extension))
    .map((entry) => entry.name.slice(0, -extension.length))
    .sort();

const canonicalSkills = directoryNames(join(root, '.claude', 'skills'));
const bridgedSkills = directoryNames(join(root, '.agents', 'skills'));

for (const name of canonicalSkills) {
  const bridge = join(root, '.agents', 'skills', name, 'SKILL.md');
  if (!existsSync(bridge)) {
    problems.push(`missing Codex skill bridge: .agents/skills/${name}/SKILL.md`);
    continue;
  }
  const expected = `../../../.claude/skills/${name}/SKILL.md`;
  if (!readFileSync(bridge, 'utf8').includes(expected)) {
    problems.push(`skill bridge does not reference ${expected}: .agents/skills/${name}/SKILL.md`);
  }
}
for (const name of bridgedSkills) {
  if (!canonicalSkills.includes(name)) {
    problems.push(`orphaned Codex skill bridge: .agents/skills/${name}/SKILL.md`);
  }
}

const claudeAgents = fileStems(join(root, '.claude', 'agents'), '.md');
const codexAgents = fileStems(join(root, '.codex', 'agents'), '.toml');
for (const name of claudeAgents) {
  if (!codexAgents.includes(name)) {
    problems.push(`missing Codex agent bridge: .codex/agents/${name}.toml`);
    continue;
  }
  const bridge = join(root, '.codex', 'agents', `${name}.toml`);
  const expected = `.claude/agents/${name}.md`;
  if (!readFileSync(bridge, 'utf8').includes(expected)) {
    problems.push(`agent bridge does not reference ${expected}: .codex/agents/${name}.toml`);
  }
}
for (const name of codexAgents) {
  if (!claudeAgents.includes(name)) problems.push(`orphaned Codex agent bridge: .codex/agents/${name}.toml`);
}

if (problems.length) {
  console.error(`✗ ${problems.length} agent asset parity problem(s):`);
  for (const problem of problems) console.error(`  ${problem}`);
  process.exit(1);
}

console.log(`✓ agent assets OK (${canonicalSkills.length} skills, ${claudeAgents.length} agents)`);

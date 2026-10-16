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

// Hook parity: a cross-agent concern must be wired in both .claude/settings.json and
// .codex/hooks.json. Hooks on this allow-list are deliberately Claude-only.
const claudeOnlyHooks = {
  'fmt-rust.sh': 'Claude PostToolUse Edit|Write formatter; Codex edits via apply_patch with no equivalent matcher',
  'guard-bash.sh': 'Claude PreToolUse Bash guard; Codex has its own sandbox/approval policy',
  'guard-edit.sh': 'Claude PreToolUse Edit|Write guard; Codex has its own sandbox/approval policy',
  'session-orient.sh': 'Claude-specific session orientation context',
  'cloud-bootstrap.sh': 'Anthropic-hosted Claude Code cloud sessions only',
};
const hookCommands = (path) => {
  const commands = [];
  const walk = (node) => {
    if (Array.isArray(node)) node.forEach(walk);
    else if (node && typeof node === 'object') {
      if (typeof node.command === 'string') commands.push(node.command);
      Object.values(node).forEach(walk);
    }
  };
  walk(JSON.parse(readFileSync(path, 'utf8')).hooks ?? {});
  return commands;
};
// Identify each hook by script plus its --hook phase argument, e.g. "worktree-gc session-end".
const hookKeys = (commands) =>
  new Set(
    commands.flatMap((c) => {
      // Every script a command runs, not only the first: a chained command
      // must register each of them for parity.
      const phase = c.match(/--hook\s+([\w-]+)/);
      return [...c.matchAll(/(?:\.claude\/hooks\/)?(worktree-gc|[\w-]+\.sh)/g)].map((m) =>
        phase && m[1] === 'worktree-gc' ? `${m[1]} ${phase[1]}` : m[1],
      );
    }),
  );
const claudeHooks = hookKeys(hookCommands(join(root, '.claude', 'settings.json')));
const codexHooks = hookKeys(hookCommands(join(root, '.codex', 'hooks.json')));
for (const key of claudeHooks) {
  if (claudeOnlyHooks[key]) continue;
  if (!codexHooks.has(key)) problems.push(`hook wired in .claude/settings.json but not .codex/hooks.json: ${key}`);
}
for (const key of codexHooks) {
  if (!claudeHooks.has(key)) problems.push(`hook wired in .codex/hooks.json but not .claude/settings.json: ${key}`);
}
for (const key of Object.keys(claudeOnlyHooks)) {
  if (!claudeHooks.has(key)) problems.push(`stale Claude-only hook allow-list entry (not in settings.json): ${key}`);
  if (codexHooks.has(key)) problems.push(`Claude-only hook is wired in .codex/hooks.json: ${key}`);
}

if (problems.length) {
  console.error(`✗ ${problems.length} agent asset parity problem(s):`);
  for (const problem of problems) console.error(`  ${problem}`);
  process.exit(1);
}

console.log(`✓ agent assets OK (${canonicalSkills.length} skills, ${claudeAgents.length} agents, ${claudeHooks.size} hooks)`);

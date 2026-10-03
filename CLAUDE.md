# Claude Code compatibility

@AGENTS.md

Claude Code 2.1.277 and later read AGENTS.md when CLAUDE.md is absent. Keep this
explicit bridge for older runtimes and for the Claude-specific guidance below;
shared instructions still belong in AGENTS.md.

Worktree disk hygiene: `.claude/settings.json` runs `scripts/worktree-gc` at
SessionStart (flags low disk; cleans idle, merged worktrees in the background) and
SessionEnd (sweeps this worktree's stale debug objects). Follow the
[worktree-lifecycle](.claude/skills/worktree-lifecycle/SKILL.md) skill; never touch a
worktree the script calls ACTIVE.

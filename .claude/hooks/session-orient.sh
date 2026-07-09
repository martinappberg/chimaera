#!/usr/bin/env bash
# SessionStart: print a short orientation. stdout is injected into the session
# context, so keep it terse, current, and bounded — it's paid for every session.
set -u
root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$root" 2>/dev/null || exit 0
branch=$(git branch --show-current 2>/dev/null)
dirty=$(git status --porcelain 2>/dev/null | grep -c . || true)
echo "Chimaera orientation (.claude/hooks/session-orient.sh):"
echo "- git: branch '${branch:-detached}', ${dirty} uncommitted path(s)."
echo "- Start at CLAUDE.md (the index). Deep docs: docs/agent-guides/. Area rules auto-load from .claude/rules/."
echo "- What the app DOES, feature by feature: docs/features/ (index → per-feature pages). Read the one page you're touching."
echo "- Run it live: preview_start 'chimaerad-isolated' (see the develop skill). Gate: 'just check' + 'npm --prefix web-ui run check'."
if [ -f "$root/.chimaera-dev/manifest.json" ]; then
  echo "- An isolated dev-daemon manifest exists (.chimaera-dev) — a preview may already be running."
fi
echo "- Docs may have drifted: verify paths/commands against the repo before trusting them, and fix docs you find wrong."
exit 0

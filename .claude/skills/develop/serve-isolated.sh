#!/usr/bin/env bash
#
# Run THIS worktree's chimaera daemon against an ISOLATED state dir, so
# parallel worktrees/chats never clobber each other's ~/.chimaera manifest
# (`serve` writes it on start and REMOVES it on stop — a shared daemon would
# delete a sibling's). State lives under <worktree>/.chimaera-dev (gitignored);
# spawned shells/agents keep the real $HOME, so ~/.claude auth still works.
#
# The port comes from $PORT (preview_start's autoPort assigns one) or, absent
# that, an OS-assigned free port. Meant to be launched via the `chimaerad-
# isolated` config in .claude/launch.json; see the develop skill.
set -euo pipefail

ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
export CHIMAERA_HOME="$ROOT/.chimaera-dev"
mkdir -p "$CHIMAERA_HOME"

BIN="$ROOT/target/debug/chimaera"
if [ ! -x "$BIN" ]; then
  echo "chimaera not built — run first:  cargo build -p chimaera" >&2
  exit 1
fi
if [ ! -f "$ROOT/web-ui/dist/index.html" ]; then
  echo "web-ui not built — run first (Node 22):  npm --prefix web-ui ci && npm --prefix web-ui run build" >&2
  exit 1
fi

# exec so signals (preview_stop) reach the daemon directly; CHIMAERA_HOME and
# $PORT are inherited.
exec "$BIN" serve

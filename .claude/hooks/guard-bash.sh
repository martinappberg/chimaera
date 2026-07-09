#!/usr/bin/env bash
# PreToolUse(Bash): deny irreversible / destructive commands. This is a best-effort
# guardrail (the user can still override a deny), not a security boundary — the
# threat model is an ACCIDENTAL destructive command, not a determined actor.
#
# False-positive avoidance, all with PORTABLE sed (works on BSD/macOS + GNU):
#   - process the command line by line, split each on && || ; | into segments;
#   - strip single-line quoted substrings from each segment;
#   - ANCHOR each dangerous pattern to the segment's LEADING command.
# So `git commit -m "explain git reset --hard"`, a multi-line commit body, and
# `echo 'rm -rf /'` are allowed; a real `git reset --hard` / `rm -rf /` invocation
# is denied. Legit subpath deletes (`rm -rf ./build`, `rm -rf ~/tmp`) are allowed —
# only bare roots deny. Degrades to a no-op without jq.
set -u
command -v jq >/dev/null 2>&1 || exit 0
cmd=$(jq -r '.tool_input.command // empty' 2>/dev/null)
[ -n "$cmd" ] || exit 0

reason=""
match() { printf '%s' "$s" | grep -qE "$1"; }

while IFS= read -r line; do
  segs=$(printf '%s' "$line" | sed -E 's/&&|\|\||[;|]/\n/g')
  while IFS= read -r seg; do
    s=$(printf '%s' "$seg" | sed -E "s/\"[^\"]*\"//g; s/'[^']*'//g; s/^[[:space:]]+//; s/[[:space:]]+$//; s/^(sudo|env|command|time)[[:space:]]+//")
    [ -n "$s" ] || continue
    if match '^git[[:space:]]+push([[:space:]]|$)' \
       && match '(--force([^-]|$)|--force-with-lease|[[:space:]]-f([[:space:]]|$))' \
       && match '[[:space:]]upstream([[:space:]]|$)'; then
      reason="force-push to 'upstream' is blocked — this repo's history is protected. Push to your fork/branch or open a PR."; break
    fi
    match '^git[[:space:]]+reset[[:space:]]+--hard' \
      && { reason="'git reset --hard' discards work — use 'git stash' or a soft/mixed reset."; break; }
    match '^git[[:space:]]+branch[[:space:]]+-D[[:space:]]+(main|master)([[:space:]]|$)' \
      && { reason="deleting the main/master branch is blocked."; break; }
    match '^git[[:space:]]+push[[:space:]].*upstream.*(:|--delete[[:space:]])(main|master)' \
      && { reason="deleting the remote main branch is blocked."; break; }
    # rm, recursive AND force, targeting a BARE root/home/cwd token (not a subpath).
    if match '^rm[[:space:]]' \
       && match '[[:space:]]-[a-zA-Z]*[rR]|--recursive' \
       && match '[[:space:]]-[a-zA-Z]*f|--force' \
       && match '[[:space:]](/|/\*|~|~/|~/\*|\.|\./|\./\*|\$HOME|\$\{HOME\}|\$HOME/|\$HOME/\*)([[:space:]]|$)'; then
      reason="'rm -rf' on a root/home/repo path is blocked — target a specific subpath."; break
    fi
  done <<< "$segs"
  [ -n "$reason" ] && break
done <<< "$cmd"

[ -n "$reason" ] && jq -cn --arg r "$reason" \
  '{hookSpecificOutput:{hookEventName:"PreToolUse",permissionDecision:"deny",permissionDecisionReason:$r}}'
exit 0

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
input=$(cat)
cmd=$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)
[ -n "$cmd" ] || exit 0
# Where the command runs — Claude's Bash cwd persists across calls, so this can be
# a different clone than the project root.
here=$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)
here=${here:-${CLAUDE_PROJECT_DIR:-.}}

reason=""
match() { printf '%s' "$s" | grep -qE "$1"; }

# The canonical repo's history is protected — PR branches included (a stale branch
# merges main in rather than rebasing). Identify it by URL, not remote name: it's
# `origin` in the maintainer's checkout and in Claude cloud clones, `upstream` in a
# fork layout.
CANON='martinappberg/chimaera'
# Parse the `git [-C dir] push ...` in $s the way git reads it: the destination is
# the first positional argument (a remote name or URL), else the remote a bare
# `git push` uses. Sets push_canon (destination is CANON), push_force (--force,
# --force-with-lease, --mirror, a short-option cluster with f, or a +refspec), and
# push_delmain (main/master deleted via -d/--delete or a :main refspec).
parse_push() {
  local tok dir="$here" stage=git want="" dest="" refs="" delete=0 b url
  push_canon=0 push_force=0 push_delmain=0
  set -f
  for tok in $s; do
    if [ -n "$want" ]; then # the value of the previous option
      case "$want" in
        dir) case "$tok" in /*) dir=$tok ;; *) dir="$dir/$tok" ;; esac ;;
        repo) [ -n "$dest" ] || dest=$tok ;;
      esac
      want=""; continue
    fi
    if [ "$stage" = git ]; then
      case "$tok" in -C) want=dir ;; -c) want=skip ;; push) stage=push ;; esac
      continue
    fi
    case "$tok" in
      --force|--force-with-lease|--force-with-lease=*|--mirror) push_force=1 ;;
      --delete) delete=1 ;;
      --repo=*) [ -n "$dest" ] || dest=${tok#--repo=} ;;
      --repo) want=repo ;;
      --push-option|--receive-pack|--exec) want=skip ;;
      --*) ;;
      -o) want=skip ;;
      -o*) ;;
      -*) case "$tok" in *f*) push_force=1 ;; esac
          case "$tok" in *d*) delete=1 ;; esac ;;
      *) if [ -z "$dest" ]; then dest=$tok; else refs="$refs $tok"; fi ;;
    esac
  done
  # Refspecs are judged after the loop: git accepts options after them
  # (`git push origin main --delete`).
  for tok in $refs; do
    case "$tok" in +*) push_force=1 ;; esac
    case "${tok#+}" in
      :main|:master|:refs/heads/main|:refs/heads/master) push_delmain=1 ;;
      main|master|refs/heads/main|refs/heads/master) [ "$delete" = 0 ] || push_delmain=1 ;;
    esac
  done
  set +f
  if [ -z "$dest" ]; then
    b=$(git -C "$dir" symbolic-ref --short -q HEAD 2>/dev/null)
    dest=$(git -C "$dir" config "branch.$b.pushRemote" 2>/dev/null \
      || git -C "$dir" config remote.pushDefault 2>/dev/null \
      || git -C "$dir" config "branch.$b.remote" 2>/dev/null \
      || echo origin)
  fi
  url=$(git -C "$dir" remote get-url "$dest" 2>/dev/null) || url=$dest
  case "$url" in *"$CANON"*) push_canon=1 ;; esac
}

while IFS= read -r line; do
  segs=$(printf '%s' "$line" | sed -E 's/&&|\|\||[;|]/\n/g')
  while IFS= read -r seg; do
    s=$(printf '%s' "$seg" | sed -E "s/\"[^\"]*\"//g; s/'[^']*'//g; s/^[[:space:]]+//; s/[[:space:]]+$//; s/^(sudo|env|command|time)[[:space:]]+//")
    [ -n "$s" ] || continue
    if match '^git([[:space:]]+(-[Cc][[:space:]]+[^[:space:]]+|-[^[:space:]]+))*[[:space:]]+push([[:space:]]|$)'; then
      parse_push
      if [ "$push_canon" = 1 ] && [ "$push_force" = 1 ]; then
        reason="force-push to ${CANON} is blocked — its history is protected, PR branches included. Bring a stale branch up to date by merging main, not rebasing."; break
      fi
      if [ "$push_canon" = 1 ] && [ "$push_delmain" = 1 ]; then
        reason="deleting main on ${CANON} is blocked."; break
      fi
    fi
    match '^git[[:space:]]+reset[[:space:]]+--hard' \
      && { reason="'git reset --hard' discards work — use 'git stash' or a soft/mixed reset."; break; }
    match '^git[[:space:]]+branch[[:space:]]+-D[[:space:]]+(main|master)([[:space:]]|$)' \
      && { reason="deleting the main/master branch is blocked."; break; }
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

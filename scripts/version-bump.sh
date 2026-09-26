#!/usr/bin/env bash
# Decide the next release version — or "skip" — from the latest v-tag and the
# squash-commit SUBJECTS of every merge since it. release.yml calls this on
# every push to main.
#
# Every merge since the tag, not just the one that triggered the run: the
# `release` concurrency group keeps one waiting run, and a newer push cancels
# it, so a merge that landed while a release was still running was otherwise
# never looked at (v0.48.1 → a feat, a fix and a test merged close together;
# only the test's subject was read, so nothing shipped). Whichever run
# executes now covers them all, and a re-run on an already-tagged commit
# finds no subjects and skips.
#
# SUBJECT-anchored on purpose: the squash message body is the folded PR
# description, so reading the type / `!` / `[skip release]` from the whole message
# lets a stray body line flip the semver bump or accidentally skip a release.
# Read them from the subject (= the PR title) instead.
#
# Policy (Conventional Commits), per subject:
#   feat                         -> minor   (reserved for new user-facing capability)
#   fix | perf | revert          -> patch
#   `!` in the subject           -> major
#   refactor|chore|docs|test|ci|build|style -> NO release
#   [skip release] in subject    -> NO release (for that merge)
#   anything else / no prefix    -> patch   (safe default; never a silent skip)
# The release is the largest bump any subject asks for; "skip" when none does.
# First release (no tag yet) is 0.1.0 (this is a pre-1.0 project).
#
# Usage: version-bump.sh <latest-tag-or-empty> [<subject>...]
#   prints exactly one line: "skip" or "MAJOR.MINOR.PATCH"
set -euo pipefail
latest="${1:-}"
[ "$#" -gt 0 ] && shift

# One subject's bump: 0 none, 1 patch, 2 minor, 3 major.
rank() {
  local subject="$1" lc
  lc=$(printf '%s' "$subject" | tr '[:upper:]' '[:lower:]')
  case "$subject" in
    *"[skip release]"*) echo 0; return ;;
  esac
  if printf '%s' "$subject" | grep -qE '^[A-Za-z]+(\([^)]*\))?!:' \
     || printf '%s' "$subject" | grep -qE 'BREAKING[ -]CHANGE'; then
    echo 3
  elif printf '%s' "$lc" | grep -qE '^feat(\([^)]*\))?:'; then
    echo 2
  elif printf '%s' "$lc" | grep -qE '^(fix|perf|revert)(\([^)]*\))?:'; then
    echo 1
  elif printf '%s' "$lc" | grep -qE '^(refactor|chore|docs|test|ci|build|style)(\([^)]*\))?:'; then
    echo 0
  else
    echo 1
  fi
}

best=0
for subject in "$@"; do
  r=$(rank "$subject")
  if [ "$r" -gt "$best" ]; then best=$r; fi
done
if [ "$best" -eq 0 ]; then
  echo skip
  exit 0
fi

# First release: no tag yet.
if [ -z "$latest" ]; then
  echo "0.1.0"
  exit 0
fi

IFS=. read -r ma mi pa <<< "${latest#v}"
ma=${ma:-0}; mi=${mi:-0}; pa=${pa:-0}
case "$best" in
  3) ma=$((ma + 1)); mi=0; pa=0 ;;
  2) mi=$((mi + 1)); pa=0 ;;
  1) pa=$((pa + 1)) ;;
esac
echo "$ma.$mi.$pa"

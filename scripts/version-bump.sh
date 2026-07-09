#!/usr/bin/env bash
# Decide the next release version — or "skip" — from the latest v-tag and the
# squash-commit SUBJECT. release.yml calls this on every push to main.
#
# SUBJECT-anchored on purpose: the squash message body is the folded PR
# description, so reading the type / `!` / `[skip release]` from the whole message
# lets a stray body line flip the semver bump or accidentally skip a release.
# Read them from the subject (= the PR title) instead.
#
# Policy (Conventional Commits):
#   feat                         -> minor   (reserved for new user-facing capability)
#   fix | perf | revert          -> patch
#   `!` in the subject           -> major
#   refactor|chore|docs|test|ci|build|style -> NO release
#   [skip release] in subject    -> NO release
#   anything else / no prefix    -> patch   (safe default; never a silent skip)
# First release (no tag yet) is 0.1.0 (this is a pre-1.0 project).
#
# Usage: version-bump.sh <latest-tag-or-empty> <subject>
#   prints exactly one line: "skip" or "MAJOR.MINOR.PATCH"
set -euo pipefail
latest="${1:-}"
subject="${2:-}"
lc=$(printf '%s' "$subject" | tr '[:upper:]' '[:lower:]')

# [skip release] wins outright.
case "$subject" in
  *"[skip release]"*) echo skip; exit 0 ;;
esac

# Bump kind from the subject's Conventional-Commit prefix.
if printf '%s' "$subject" | grep -qE '^[A-Za-z]+(\([^)]*\))?!:' \
   || printf '%s' "$subject" | grep -qE 'BREAKING[ -]CHANGE'; then
  kind=major
elif printf '%s' "$lc" | grep -qE '^feat(\([^)]*\))?:'; then
  kind=minor
elif printf '%s' "$lc" | grep -qE '^(fix|perf|revert)(\([^)]*\))?:'; then
  kind=patch
elif printf '%s' "$lc" | grep -qE '^(refactor|chore|docs|test|ci|build|style)(\([^)]*\))?:'; then
  echo skip; exit 0
else
  kind=patch
fi

# First release: no tag yet.
if [ -z "$latest" ]; then
  echo "0.1.0"
  exit 0
fi

IFS=. read -r ma mi pa <<< "${latest#v}"
ma=${ma:-0}; mi=${mi:-0}; pa=${pa:-0}
case "$kind" in
  major) ma=$((ma + 1)); mi=0; pa=0 ;;
  minor) mi=$((mi + 1)); pa=0 ;;
  patch) pa=$((pa + 1)) ;;
esac
echo "$ma.$mi.$pa"

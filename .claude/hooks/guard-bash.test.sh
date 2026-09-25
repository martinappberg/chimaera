#!/usr/bin/env bash
# Regression table for guard-bash.sh. Run: bash .claude/hooks/guard-bash.test.sh
# (optionally pass a guard path as $1). Needs jq. Exits non-zero on any failure.
set -u
G="${1:-$(cd "$(dirname "$0")" && pwd)/guard-bash.sh}"
pass=0; fail=0

# The force-push rule resolves remote names to URLs, so pin the remotes in a
# scratch repo instead of depending on how the caller's clone names them.
repo=$(mktemp -d)
trap 'rm -rf "$repo"' EXIT
git -C "$repo" init -q
git -C "$repo" remote add origin git@github.com:martinappberg/chimaera.git
git -C "$repo" remote add upstream https://github.com/martinappberg/chimaera
git -C "$repo" remote add fork git@github.com:someone/chimaera.git
export CLAUDE_PROJECT_DIR="$repo"
run() {
  local expect="$1" cmd="$2" out got
  out=$(printf '%s' "$cmd" | jq -Rs '{tool_input:{command:.}}' | bash "$G")
  got="ALLOW"; [ -n "$out" ] && got="DENY"
  if [ "$got" = "$expect" ]; then pass=$((pass+1))
  else fail=$((fail+1)); printf 'FAIL exp=%s got=%s : %s\n' "$expect" "$got" "$cmd"; fi
}

run DENY  'git push --force origin main'
run DENY  'git push -f origin my-branch'
run DENY  'git push origin +HEAD:my-branch'
run DENY  'git push --force upstream main'
run DENY  'git push -f upstream HEAD'
run DENY  'git push upstream main --force-with-lease'
run DENY  'git push --force git@github.com:martinappberg/chimaera.git my-branch'
run DENY  'git push origin --delete main'
run DENY  'git push origin :main'
run DENY  'git reset --hard HEAD~2'
run DENY  'sudo git reset --hard'
run DENY  'git branch -D main'
run DENY  'rm -rf /'
run DENY  'rm -rf ~'
run DENY  'rm -rf .'
run DENY  'rm -rf /*'
run DENY  'cargo build && rm -rf ~/'
run DENY  'git add -A && git reset --hard origin/main'
run ALLOW 'git push origin main'
run ALLOW 'git push -u origin HEAD'
run ALLOW 'git push --force fork my-branch'
run ALLOW 'git push fork +HEAD:my-branch'
run ALLOW 'git reset --soft HEAD~1'
run ALLOW 'git reset HEAD file.txt'
run ALLOW 'git branch -D feature/old'
run ALLOW 'rm -rf ./build'
run ALLOW 'rm -rf ~/tmp/scratch'
run ALLOW 'rm -rf node_modules'
run ALLOW 'rm -f somefile'
run ALLOW 'git status'
run ALLOW 'git commit -m "explain: guard denies git reset --hard and rm -rf /"'
run ALLOW 'echo "run git push -f origin to force"'
run ALLOW "echo 'rm -rf / is dangerous'"
run ALLOW 'npm --prefix web-ui run build'
run ALLOW 'cargo test --workspace'
run ALLOW 'git commit -m "guards:

- deny git reset --hard
- deny force-push origin
- deny rm -rf /"'

echo "guard-bash.test.sh: pass=$pass fail=$fail"
[ "$fail" -eq 0 ]

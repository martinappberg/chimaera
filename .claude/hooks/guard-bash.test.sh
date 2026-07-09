#!/usr/bin/env bash
# Regression table for guard-bash.sh. Run: bash .claude/hooks/guard-bash.test.sh
# (optionally pass a guard path as $1). Needs jq. Exits non-zero on any failure.
set -u
G="${1:-$(cd "$(dirname "$0")" && pwd)/guard-bash.sh}"
pass=0; fail=0
run() {
  local expect="$1" cmd="$2" out got
  out=$(printf '%s' "$cmd" | jq -Rs '{tool_input:{command:.}}' | bash "$G")
  got="ALLOW"; [ -n "$out" ] && got="DENY"
  if [ "$got" = "$expect" ]; then pass=$((pass+1))
  else fail=$((fail+1)); printf 'FAIL exp=%s got=%s : %s\n' "$expect" "$got" "$cmd"; fi
}

run DENY  'git push --force upstream main'
run DENY  'git push -f upstream HEAD'
run DENY  'git push upstream main --force-with-lease'
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
run ALLOW 'git push --force origin my-branch'
run ALLOW 'git reset --soft HEAD~1'
run ALLOW 'git reset HEAD file.txt'
run ALLOW 'git branch -D feature/old'
run ALLOW 'rm -rf ./build'
run ALLOW 'rm -rf ~/tmp/scratch'
run ALLOW 'rm -rf node_modules'
run ALLOW 'rm -f somefile'
run ALLOW 'git status'
run ALLOW 'git commit -m "explain: guard denies git reset --hard and rm -rf /"'
run ALLOW 'echo "run git push -f upstream to force"'
run ALLOW "echo 'rm -rf / is dangerous'"
run ALLOW 'npm --prefix web-ui run build'
run ALLOW 'cargo test --workspace'
run ALLOW 'git commit -m "guards:

- deny git reset --hard
- deny force-push upstream
- deny rm -rf /"'

echo "guard-bash.test.sh: pass=$pass fail=$fail"
[ "$fail" -eq 0 ]

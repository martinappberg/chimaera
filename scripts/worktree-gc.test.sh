#!/usr/bin/env bash
# Characterization tests for scripts/worktree-gc. It deletes worktrees and build
# output, so its safety rules are pinned here against a throwaway repo (a local
# bare origin, a fake `gh` that serves canned PR states) and run in CI.
# Run: bash scripts/worktree-gc.test.sh
set -u
here=$(cd "$(dirname "$0")" && pwd)
T=$(mktemp -d "${TMPDIR:-/tmp}/worktree-gc-test.XXXXXX")
T=$(cd "$T" && pwd -P)
pids=""
cleanup() {
  for p in $pids; do kill "$p" 2>/dev/null; done
  rm -rf "$T"
}
trap cleanup EXIT
pass=0 fail=0
ok() { pass=$((pass + 1)); }
no() { fail=$((fail + 1)); printf 'FAIL  %s\n' "$*"; }

# Hermetic git: no user/system config (signing, hooks, templates).
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com
unset CLAUDE_PROJECT_DIR CLAUDE_CODE_REMOTE WORKTREE_GC
export WORKTREE_GC_LOW_GB=0

mkdir -p "$T/bin"
cat >"$T/bin/gh" <<'EOF'
#!/bin/sh
[ -z "${GH_FAKE_FAIL:-}" ] || exit 1
cat "$GH_FAKE_PRS"
EOF
chmod +x "$T/bin/gh"
export PATH="$T/bin:$PATH" GH_FAKE_PRS="$T/prs.tsv"

git init -q --bare -b main "$T/origin.git"
git clone -q "$T/origin.git" "$T/main" 2>/dev/null
M="$T/main"
cd "$M" || exit 1
printf 'target/\nnode_modules/\n' >.gitignore
echo base >file.txt
git add -A && git commit -qm base && git push -q origin main
git remote set-head origin main
# The script acts on the repo it lives in, so the copy under test lives here.
mkdir -p "$M/tools" && cp "$here/worktree-gc" "$M/tools/worktree-gc"
GC="$M/tools/worktree-gc"

W="$T/wt"
mkdir -p "$W"
commit() { # worktree message
  echo "$2" >>"$1/file.txt"
  git -C "$1" commit -qam "$2"
}
new_wt() { # name [start]
  git -C "$M" worktree add -q -b "b/$1" "$W/$1" "${2:-main}"
}
fake_target() { # worktree [rel]
  local d="$1/${2:-target}"
  mkdir -p "$d/debug/deps"
  echo 'Signature: 8a477f597d28d172789f06886806bc55' >"$d/CACHEDIR.TAG"
  head -c 4096 /dev/zero >"$d/debug/deps/libx.rlib"
}

# merged: PR merged, then its remote branch deleted (GitHub's auto-delete) —
# the commits are only in the PR, which must still count as pushed.
new_wt merged
commit "$W/merged" m1
git -C "$W/merged" push -q origin b/merged
merged_oid=$(git -C "$W/merged" rev-parse HEAD)
git push -q origin --delete b/merged
fake_target "$W/merged"
mkdir -p "$W/merged/web-ui/node_modules/x"

# closed: PR closed unmerged, branch still on the remote.
new_wt closed
commit "$W/closed" c1
git -C "$W/closed" push -q origin b/closed
closed_oid=$(git -C "$W/closed" rev-parse HEAD)

# contained: no commits of its own — HEAD is in origin/main.
new_wt contained

# open PR, idle: build output trimmed, worktree kept.
new_wt open
commit "$W/open" o1
git -C "$W/open" push -q origin b/open
open_oid=$(git -C "$W/open" rev-parse HEAD)
fake_target "$W/open"
fake_target "$W/open" "crates/app/target"

# local-only commits, no PR: never removed; build output trimmed when idle.
new_wt localonly
commit "$W/localonly" l1
fake_target "$W/localonly"

# local-only, no build output: nothing to do.
new_wt localbare
commit "$W/localbare" l2

# merged PR but uncommitted changes.
new_wt dirty
commit "$W/dirty" d1
git -C "$W/dirty" push -q origin b/dirty
dirty_oid=$(git -C "$W/dirty" rev-parse HEAD)
echo scratch >"$W/dirty/notes.txt"

# merged PR, then more commits pushed to the same branch.
new_wt aftermerge
commit "$W/aftermerge" a1
aftermerge_oid=$(git -C "$W/aftermerge" rev-parse HEAD)
commit "$W/aftermerge" a2
git -C "$W/aftermerge" push -q origin b/aftermerge

# detached HEAD at a merged PR's head: matched by commit, not branch.
git -C "$M" worktree add -q --detach "$W/detached" "$closed_oid"

# locked, in use (a process's cwd): ACTIVE though merged.
new_wt locked
git -C "$M" worktree lock --reason "agent pid 999999" "$W/locked"
new_wt inuse

# Recently changed. A merged PR is removable anyway: merged, clean and pushed
# leaves nothing in progress. A closed PR may be reopened, a fresh worktree
# from main has its HEAD in main before any work, and a new branch that reuses
# a merged PR's name is not that PR — all three stay ACTIVE.
new_wt recent
new_wt recentclosed
commit "$W/recentclosed" rc1
git -C "$W/recentclosed" push -q origin b/recentclosed
new_wt recentbase
new_wt reused

# An archived session: the Claude app detaches its branch (a fresh HEAD and
# reflog write), and the PR's head moved on after this checkout last pushed,
# so only the branch still pointing at HEAD ties it to its merged PR.
new_wt archived
commit "$W/archived" r1
git -C "$W/archived" push -q origin b/archived
commit "$W/archived" r2
git -C "$W/archived" push -q origin b/archived
archived_oid=$(git -C "$W/archived" rev-parse HEAD)
git -C "$W/archived" reset -q --hard HEAD~1
git -C "$W/archived" checkout -q --detach

# A worktree whose folder was deleted by hand: only git's entry is left.
new_wt gone
rm -rf "$W/gone"

printf '%s\t%s\t%s\t%s\n' \
  b/merged MERGED 11 "$merged_oid" \
  b/closed CLOSED 12 "$closed_oid" \
  b/open OPEN 13 "$open_oid" \
  b/dirty MERGED 14 "$dirty_oid" \
  b/aftermerge MERGED 15 "$aftermerge_oid" \
  b/locked MERGED 16 "$(git -C "$W/locked" rev-parse HEAD)" \
  b/inuse MERGED 17 "$(git -C "$W/inuse" rev-parse HEAD)" \
  b/recent MERGED 18 "$(git -C "$W/recent" rev-parse HEAD)" \
  b/recentclosed CLOSED 19 "$(git -C "$W/recentclosed" rev-parse HEAD)" \
  b/reused MERGED 20 "$merged_oid" \
  b/archived MERGED 21 "$archived_oid" >"$T/prs.tsv"

# Age everything but the recent ones by months: files, dirs, and git's
# per-worktree admin files (HEAD, index, reflog).
export GIT_OPTIONAL_LOCKS=0
for d in "$W"/*; do
  case "${d##*/}" in recent | recentclosed | recentbase | reused | archived) continue ;; esac
  find "$d" -exec touch -t 202601010000 {} + 2>/dev/null
  find "$M/.git/worktrees/${d##*/}" -exec touch -t 202601010000 {} + 2>/dev/null
done
(cd "$W/inuse" && exec sleep 300) &
pids="$pids $!"
disown $! 2>/dev/null
sleep 1

row() { grep -E "^[A-Z]+ +[^ ]+ +$1 " "$T/out" | head -1; }
expect() { # branch group [reason-substring]
  local r
  r=$(row "$1")
  case "$r" in
    "$2 "*) case "$r" in *"${3:-}"*) ok ;; *) no "$1: expected reason '$3' in: $r" ;; esac ;;
    *) no "$1: expected $2, got: ${r:-<no row>}" ;;
  esac
}
expect_wt() { # worktree-name group [reason-substring] — for detached rows
  local r
  r=$(grep -F "$W/$1 " "$T/out" | head -1)
  case "$r" in
    "$2 "*) case "$r" in *"${3:-}"*) ok ;; *) no "$1: expected reason '$3' in: $r" ;; esac ;;
    *) no "$1: expected $2, got: ${r:-<no row>}" ;;
  esac
}

bash "$GC" >"$T/out" 2>&1
expect b/merged REMOVE "PR #11 merged"
expect b/closed REMOVE "PR #12 closed"
expect b/contained REMOVE "HEAD is in origin/main"
expect_wt detached REMOVE "PR #12 closed"
expect b/open TRIM "PR #13 open"
expect b/localonly TRIM "no PR"
expect b/localbare KEEP "no build output"
expect b/dirty KEEP "uncommitted"
expect b/aftermerge KEEP "since MERGED #15"
expect b/locked ACTIVE "locked: agent pid 999999 (stale?"
expect b/inuse ACTIVE "in use by sleep"
expect b/recent REMOVE "PR #18 merged"
expect b/recentclosed ACTIVE "changed"
expect b/recentbase ACTIVE "changed"
expect b/reused ACTIVE "changed"
expect_wt archived REMOVE "PR #21 merged"
expect main ACTIVE "this session runs here"
expect b/gone REMOVE "directory is gone"
grep -q "Dry run: nothing changed" "$T/out" && ok || no "dry run footer missing"
for n in merged closed contained open localonly; do [ -d "$W/$n" ] && ok || no "dry run touched $n"; done
[ -d "$W/open/target" ] && ok || no "dry run trimmed open/target"

# Without gh, nothing is removable by PR state (HEAD-in-main still is).
GH_FAKE_FAIL=1 bash "$GC" --no-fetch >"$T/out" 2>&1
expect b/merged TRIM "PR state unknown"
expect b/contained REMOVE "HEAD is in origin/main"

# Run from inside another repo, it still judges its own repo's worktrees.
git init -q -b main "$T/other" && git -C "$T/other" commit -q --allow-empty -m x
git -C "$T/other" worktree add -q -b o/foreign "$T/other-wt"
(cd "$T/other" && bash "$GC" --no-fetch) >"$T/out" 2>&1
expect b/merged REMOVE "PR #11 merged"
grep -q "o/foreign" "$T/out" && no "judged another repo's worktree: $(grep o/foreign "$T/out")" || ok

bash "$GC" --no-fetch >"$T/out" 2>&1

# --markdown renders the same rows.
bash "$GC" --markdown --no-fetch >"$T/md" 2>&1
grep -q '^| REMOVE | .* | `b/merged` | MERGED #11 |' "$T/md" && ok || no "markdown row: $(grep b/merged "$T/md")"

bash "$GC" --apply >"$T/apply" 2>&1
for n in merged closed contained detached recent archived; do [ ! -e "$W/$n" ] && ok || no "$n not removed: $(cat "$T/apply")"; done
[ -z "$(git -C "$M" worktree list --porcelain | grep -F "$W/gone")" ] && ok || no "gone's entry not pruned"
[ -d "$T/other-wt" ] && ok || no "another repo's worktree was touched"
for b in b/merged b/closed b/contained; do git -C "$M" rev-parse -q --verify "refs/heads/$b" >/dev/null && ok || no "branch $b deleted"; done
[ ! -e "$W/open/target" ] && [ ! -e "$W/open/crates/app/target" ] && ok || no "open's build output not trimmed"
[ -f "$W/open/file.txt" ] && ok || no "trim deleted open's source"
[ ! -e "$W/localonly/target" ] && [ -f "$W/localonly/file.txt" ] && ok || no "localonly trim wrong"
for n in localbare dirty aftermerge locked inuse recentclosed recentbase reused; do [ -d "$W/$n" ] && ok || no "$n was removed"; done
[ -f "$W/dirty/notes.txt" ] && ok || no "dirty lost its uncommitted file"
[ -z "$(git -C "$M" worktree list --porcelain | grep -F "$W/merged")" ] && ok || no "merged still registered"
grep -q "df free: " "$T/apply" && ok || no "apply did not report df: $(tail -3 "$T/apply")"

# --self --sweep: only unreferenced, old objects go; referenced and young stay.
S="$W/localbare"
fake_target "$S"
D="$S/target/debug/deps"
printf 'junk\0%s/app-1.a.cgu.00.rcgu.o\0junk' "$D" >"$D/app-1"
chmod +x "$D/app-1"
for o in app-1.a.cgu.00.rcgu.o app-1.old.cgu.07.rcgu.o app-1.young.cgu.03.rcgu.o app-1.dev.cgu.05.rcgu.o; do echo o >"$D/$o"; done
# An older build still hard-linked outside deps/ (run-app-isolated.sh's .app).
mkdir -p "$S/target/debug/dev.app/Contents/MacOS"
printf 'x\0%s/app-1.dev.cgu.05.rcgu.o\0' "$D" >"$S/target/debug/dev.app/Contents/MacOS/dev"
chmod +x "$S/target/debug/dev.app/Contents/MacOS/dev"
touch -t 202601010000 "$D/app-1.a.cgu.00.rcgu.o" "$D/app-1.old.cgu.07.rcgu.o" "$D/app-1.dev.cgu.05.rcgu.o" "$D"
deps_mtime() { if stat -f %m "$D" >/dev/null 2>&1; then stat -f %m "$D"; else stat -c %Y "$D"; fi; }
before_mtime=$(deps_mtime)
(cd "$S" && bash "$GC" --self --sweep) >"$T/self" 2>&1
grep -q "1 stale debug object" "$T/self" && [ -f "$D/app-1.old.cgu.07.rcgu.o" ] && ok || no "self sweep dry run: $(cat "$T/self")"
(cd "$S" && bash "$GC" --self --sweep --apply) >"$T/self" 2>&1
[ ! -e "$D/app-1.old.cgu.07.rcgu.o" ] && ok || no "stale object not swept: $(cat "$T/self")"
[ -f "$D/app-1.a.cgu.00.rcgu.o" ] && [ -f "$D/app-1.young.cgu.03.rcgu.o" ] && [ -f "$D/libx.rlib" ] && ok || no "sweep deleted a live object"
[ -f "$D/app-1.dev.cgu.05.rcgu.o" ] && ok || no "sweep deleted an object a binary outside deps/ references"
[ "$(deps_mtime)" = "$before_mtime" ] && ok || no "sweep moved deps/ mtime (the idle clock)"
(cd "$S" && bash "$GC" --self --sweep --apply) >"$T/self" 2>&1
grep -q "no stale debug objects" "$T/self" && ok || no "empty sweep: $(cat "$T/self")"

# --self: the whole target dir, but not while something runs from it.
(cd "$S" && bash "$GC" --self) >"$T/self" 2>&1
[ -d "$S/target" ] && grep -q "would delete" "$T/self" && ok || no "self dry run: $(cat "$T/self")"
# (A process holding a file under target/ open, as a daemon running from
# target/debug does. Not a copied system binary: macOS kills those on exec.)
(cd / && exec sleep 300) <"$S/target/debug/deps/libx.rlib" &
holder=$!
disown "$holder" 2>/dev/null
sleep 1
(cd "$S" && bash "$GC" --self --apply) >"$T/self" 2>&1
[ -d "$S/target" ] && grep -q "skipped" "$T/self" && ok || no "self trim ignored an open file: $(cat "$T/self")"
kill "$holder" 2>/dev/null
sleep 1
(cd "$S" && bash "$GC" --self --apply) >"$T/self" 2>&1
[ ! -e "$S/target" ] && [ -f "$S/file.txt" ] && ok || no "self trim: $(cat "$T/self")"

# Hooks: the low-disk flag, the off switch, and a detached background run.
# (A fresh interval stamp keeps these first calls from spawning a run.)
mkdir -p "$M/.git/worktree-gc" && touch "$M/.git/worktree-gc/last-run"
out=$(cd "$M" && WORKTREE_GC_LOW_GB=999999999 bash "$GC" --hook session-start </dev/null)
case "$out" in "Disk is low:"*) ok ;; *) no "no low-disk flag: $out" ;; esac
out=$(cd "$M" && WORKTREE_GC=off WORKTREE_GC_LOW_GB=999999999 bash "$GC" --hook session-start </dev/null)
[ -z "$out" ] && ok || no "WORKTREE_GC=off still printed: $out"
LOG="$M/.git/worktree-gc/log"
rm -rf "$M/.git/worktree-gc"
out=$(cd "$M" && bash "$GC" --hook session-start </dev/null)
[ -z "$out" ] && ok || no "session-start printed with plenty of disk: $out"
i=0
while [ $i -lt 60 ] && ! grep -q "nothing to do\|df free: " "$LOG" 2>/dev/null; do sleep 1; i=$((i + 1)); done
grep -q -- "--- auto run" "$LOG" && ok || no "no background run logged: $(ls -la "$M/.git/worktree-gc" 2>&1; cat "$LOG" 2>&1)"
# The run's last log line comes before its exit trap drops the lock: wait for
# the lock, then count runs. A leftover lock proves nothing either way.
i=0
while [ $i -lt 30 ] && [ -d "$M/.git/worktree-gc/lock" ]; do sleep 1; i=$((i + 1)); done
(cd "$M" && bash "$GC" --hook session-start </dev/null)
sleep 3
runs=$(grep -c -- "--- auto run" "$LOG")
[ "$runs" = 1 ] && ok || no "a second run started inside the interval ($runs runs): $(cat "$LOG")"
# session-end: /clear does nothing; a real end sweeps the session's worktree
# (named by the hook payload's cwd) in the background.
fake_target "$S"
printf 'x\0' >"$D/app-1" && chmod +x "$D/app-1"
echo o >"$D/app-1.gone.cgu.01.rcgu.o" && touch -t 202601010000 "$D/app-1.gone.cgu.01.rcgu.o"
printf '{"reason":"clear","cwd":"%s"}' "$S" | (cd "$M" && bash "$GC" --hook session-end) && ok || no "session-end (clear) failed"
sleep 2
[ -f "$D/app-1.gone.cgu.01.rcgu.o" ] && ok || no "session-end swept on /clear"
printf '{"reason":"other","cwd":"%s"}' "$S" | (cd "$M" && bash "$GC" --hook session-end) && ok || no "session-end failed"
i=0
while [ $i -lt 30 ] && [ -f "$D/app-1.gone.cgu.01.rcgu.o" ]; do sleep 1; i=$((i + 1)); done
[ ! -e "$D/app-1.gone.cgu.01.rcgu.o" ] && ok || no "session-end did not sweep $S: $(tail -3 "$LOG")"

# `git config worktree-gc.auto false`: hooks still flag low disk, delete nothing.
git -C "$M" config worktree-gc.auto false
echo o >"$D/app-1.kept.cgu.02.rcgu.o" && touch -t 202601010000 "$D/app-1.kept.cgu.02.rcgu.o"
rm -rf "$M/.git/worktree-gc"
out=$(cd "$M" && WORKTREE_GC_LOW_GB=999999999 bash "$GC" --hook session-start </dev/null)
case "$out" in "Disk is low:"*) ok ;; *) no "auto=false lost the low-disk flag: $out" ;; esac
printf '{"reason":"other","cwd":"%s"}' "$S" | (cd "$M" && bash "$GC" --hook session-end)
(cd "$M" && bash "$GC" --auto) >/dev/null 2>&1
sleep 3
[ ! -e "$M/.git/worktree-gc/last-run" ] && [ ! -e "$M/.git/worktree-gc/log" ] && [ -f "$D/app-1.kept.cgu.02.rcgu.o" ] && ok ||
  no "auto=false still ran: $(ls "$M/.git/worktree-gc" 2>&1)"

echo "worktree-gc: $pass passed, $fail failed"
[ "$fail" -eq 0 ]

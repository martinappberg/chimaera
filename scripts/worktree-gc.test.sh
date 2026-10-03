#!/usr/bin/env bash
# Characterization tests for scripts/worktree-gc. It deletes worktrees and build
# output, so its safety rules are pinned here against a throwaway repo (a local
# bare origin, a fake `gh` that serves canned PR states) and run in CI.
# Run: bash scripts/worktree-gc.test.sh
set -u
here=$(cd "$(dirname "$0")" && pwd)
real_node=$(command -v node)
real_npm=$(command -v npm)
real_just=$(command -v just || true)
T=$(mktemp -d "${TMPDIR:-/tmp}/worktree-gc-test.XXXXXX")
T=$(cd "$T" && pwd -P)
pids=""
stubborn=""
cleanup() {
  [ -z "$stubborn" ] || kill -KILL "$stubborn" 2>/dev/null
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
# from main has its HEAD in main before any work, a new branch that reuses a
# merged PR's name is not that PR, and a fresh detached worktree at a main tip
# that equals a merged PR's head (a fast-forward merge) has no work of its own
# — those stay ACTIVE.
new_wt recent
commit "$W/recent" re1
git -C "$W/recent" push -q origin b/recent
new_wt recentclosed
commit "$W/recentclosed" rc1
git -C "$W/recentclosed" push -q origin b/recentclosed
new_wt recentbase
new_wt reused
git -C "$M" worktree add -q --detach "$W/fresh" main

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
  b/archived MERGED 21 "$archived_oid" \
  b/ff MERGED 22 "$(git -C "$M" rev-parse main)" >"$T/prs.tsv"

# Age everything but the recent ones by months: files, dirs, and git's
# per-worktree admin files (HEAD, index, reflog).
export GIT_OPTIONAL_LOCKS=0
for d in "$W"/*; do
  case "${d##*/}" in recent | recentclosed | recentbase | reused | fresh | archived) continue ;; esac
  find "$d" -exec touch -t 202601010000 {} + 2>/dev/null
  find "$M/.git/worktrees/${d##*/}" -exec touch -t 202601010000 {} + 2>/dev/null
done
(cd "$W/inuse" && exec sleep 300) &
pids="$pids $!"
disown $! 2>/dev/null
sleep 1

judge() { # label row group [reason-substring]
  case "$2" in
    "$3 "*) case "$2" in *"${4:-}"*) ok ;; *) no "$1: expected reason '$4' in: $2" ;; esac ;;
    *) no "$1: expected $3, got: ${2:-<no row>}" ;;
  esac
}
expect() { # branch group [reason-substring]
  judge "$1" "$(grep -E "^[A-Z]+ +[^ ]+ +$1 " "$T/out" | head -1)" "$2" "${3:-}"
}
# Detached rows share a branch column, so these match the folder name (the
# path's tail: short_path shows paths under $HOME as ~/…).
expect_wt() { # worktree-name group [reason-substring]
  judge "$1" "$(grep -F "/wt/$1 " "$T/out" | head -1)" "$2" "${3:-}"
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
expect_wt fresh ACTIVE "changed"
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
for n in localbare dirty aftermerge locked inuse recentclosed recentbase reused fresh; do [ -d "$W/$n" ] && ok || no "$n was removed"; done
[ -f "$W/dirty/notes.txt" ] && ok || no "dirty lost its uncommitted file"
[ -z "$(git -C "$M" worktree list --porcelain | grep -F "$W/merged")" ] && ok || no "merged still registered"
grep -q "df free: " "$T/apply" && ok || no "apply did not report df: $(tail -3 "$T/apply")"

# Real preview processes: executables, cwd, parentage, and SIGTERM handlers
# exercise the same OS inspection as a live daemon. Copying system binaries
# breaks macOS code signing, so compile a tiny local fixture instead.
cat >"$T/preview.c" <<'EOF'
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>
static volatile sig_atomic_t stopping;
static void stop(int sig) { (void)sig; stopping = 1; }
int main(void) {
  signal(SIGTERM, getenv("IGNORE_TERM") ? SIG_IGN : stop);
  if (getenv("HOLD_FILE") && !fopen(getenv("HOLD_FILE"), "r")) return 1;
  pid_t child = -1;
  if (getenv("PREVIEW_CHILD")) {
    child = fork();
    if (!child) {
      if (getenv("VITE_ENTRY")) {
        unsetenv("PREVIEW_CHILD");
        execl(getenv("VITE_NODE"), "node", getenv("VITE_ENTRY"), (char *)NULL);
      } else { execlp("sleep", "sleep", "300", (char *)NULL); }
      _exit(1);
    }
  }
  if (getenv("VITE_READY") && !getenv("PREVIEW_CHILD")) {
    FILE *f = fopen(getenv("VITE_READY"), "w");
    if (f) fclose(f);
  }
  while (!stopping) sleep(1);
  if (child > 0) { kill(child, SIGTERM); waitpid(child, NULL, 0); }
  if (getenv("DIRTY_ON_STOP")) {
    FILE *f = fopen("shutdown-notes.txt", "w");
    if (f) { fputs("preserve this work", f); fclose(f); }
  }
  return 0;
}
EOF
cc "$T/preview.c" -o "$T/preview" || exit 1
preview_wt() { # name [state]
  new_wt "$1"
  commit "$W/$1" "$1"
  git -C "$W/$1" push -q origin "b/$1"
  printf '%s\t%s\t%s\t%s\n' "b/$1" "${2:-MERGED}" 30 "$(git -C "$W/$1" rev-parse HEAD)" >>"$T/prs.tsv"
  fake_target "$W/$1"
  cp "$T/preview" "$W/$1/target/debug/chimaera"
}
start_preview() { # name [env settings...]
  local name=$1
  shift
  (cd "$W/$name" && exec env "$@" target/debug/chimaera serve --port 0) &
  preview_pid=$!
  pids="$pids $preview_pid"
  disown "$preview_pid" 2>/dev/null
}
for name in previewdaemon previewapp previewvite previewnpm previewmixed previewdirty previewunpushed previewlocked previewstubborn previewwrites previewforeign previewrelease previewnested; do preview_wt "$name"; done
preview_wt previewopen OPEN
preview_wt previewclosed CLOSED
start_preview previewdaemon PREVIEW_CHILD=1
daemon_pid=$preview_pid
start_preview previewmixed
mixed_pid=$preview_pid
(cd "$W/previewmixed" && exec sleep 300) &
pids="$pids $!"
disown $! 2>/dev/null
echo notes >"$W/previewdirty/notes.txt"
start_preview previewdirty
commit "$W/previewunpushed" unpushed
start_preview previewunpushed
git -C "$M" worktree lock "$W/previewlocked"
start_preview previewlocked
start_preview previewopen
start_preview previewclosed
git -C "$M" worktree add -q --detach "$W/previewnested/child" main
start_preview previewnested
start_preview previewstubborn IGNORE_TERM=1
stubborn=$preview_pid
start_preview previewwrites DIRTY_ON_STOP=1
start_preview previewforeign HOLD_FILE="$W/previewmixed/target/debug/deps/libx.rlib"
mkdir -p "$W/previewrelease/target/release"
cp "$T/preview" "$W/previewrelease/target/release/chimaera"
(cd "$W/previewrelease" && exec target/release/chimaera serve --port 0) &
pids="$pids $!"
disown $! 2>/dev/null
app_exe="$W/previewapp/crates/chimaera-app/target/debug/chimaera-dev.app/Contents/MacOS/chimaera-dev"
mkdir -p "$(dirname "$app_exe")"
cp "$T/preview" "$app_exe"
(cd "$W/previewapp" && exec "$app_exe" --daemon) &
app_pid=$!
pids="$pids $app_pid"
disown "$app_pid" 2>/dev/null
mkdir -p "$W/previewvite/web-ui/node_modules/.bin"
cp "$T/preview" "$T/bin/node"
(cd "$W/previewvite/web-ui" && exec "$T/bin/node" "$W/previewvite/web-ui/node_modules/.bin/vite" --port 0) &
vite_pid=$!
pids="$pids $vite_pid"
disown "$vite_pid" 2>/dev/null
mkdir -p "$W/previewnpm/web-ui/node_modules/.bin"
(cd "$W/previewnpm/web-ui" && PREVIEW_CHILD=1 VITE_NODE="$T/bin/node" VITE_ENTRY="$W/previewnpm/web-ui/node_modules/.bin/vite" VITE_READY="$T/vite-ready" exec bash -c 'exec -a "npm exec vite" "$1"' _ "$T/bin/node") &
npm_pid=$!
pids="$pids $npm_pid"
disown "$npm_pid" 2>/dev/null
# The child must have exec'd before lsof takes its snapshot; process startup
# (notably macOS code-signature validation) need not finish in a fixed second.
i=0
while [ "$i" -lt 15 ] && [ ! -f "$T/vite-ready" ]; do sleep 1; i=$((i + 1)); done
[ -f "$T/vite-ready" ] && ok || no "npm fixture's Vite child did not start"

# Real npm builds the OS-specific shell chain used by launch.json and the
# dev-ui recipe. The local Vite stand-in needs no dependency installation.
real_previews="previewnpmrun previewnpmprefix"
[ -z "$real_just" ] || real_previews="$real_previews previewjust"
real_preview_pids=""
for name in $real_previews previewnpmcompound; do
  new_wt "$name"
  mkdir -p "$W/$name/web-ui/node_modules/.bin"
  dev=vite
  [ "$name" != previewnpmcompound ] || dev='vite & wait'
  printf '{"private":true,"scripts":{"dev":"%s"}}\n' "$dev" >"$W/$name/web-ui/package.json"
  printf 'dev-ui:\n    cd web-ui && npm run dev\n' >"$W/$name/justfile"
  git -C "$W/$name" add web-ui/package.json justfile
  commit "$W/$name" "$name"
  git -C "$W/$name" push -q origin "b/$name"
  printf '%s\tMERGED\t31\t%s\n' "b/$name" "$(git -C "$W/$name" rev-parse HEAD)" >>"$T/prs.tsv"
  cat >"$W/$name/web-ui/node_modules/.bin/vite" <<'EOF'
#!/usr/bin/env node
require('fs').writeFileSync(process.env.GC_VITE_READY, String(process.pid));
setInterval(() => {}, 1000);
process.on('SIGTERM', () => process.exit(0));
EOF
  chmod +x "$W/$name/web-ui/node_modules/.bin/vite"
  (
    PATH="$(dirname "$real_node"):$PATH"
    export PATH GC_VITE_READY="$T/$name.ready"
    case "$name" in
      previewnpmprefix) cd "$W/$name" && exec "$real_npm" --prefix web-ui run dev -- --port 0 ;;
      previewjust) cd "$W/$name" && exec "$real_just" dev-ui ;;
      *) cd "$W/$name/web-ui" && exec "$real_npm" run dev ;;
    esac
  ) >"$T/$name.log" 2>&1 &
  real_pid=$!
  pids="$pids $real_pid"
  disown "$real_pid" 2>/dev/null
  i=0
  while [ "$i" -lt 15 ] && [ ! -f "$T/$name.ready" ]; do sleep 1; i=$((i + 1)); done
  if [ -f "$T/$name.ready" ]; then
    real_child=$(cat "$T/$name.ready")
    pids="$pids $real_child"
    [ "$name" = previewnpmcompound ] || real_preview_pids="$real_preview_pids $real_pid $real_child"
    ok
  else
    no "$name did not start: $(cat "$T/$name.log")"
  fi
done
sleep 1
# Linux lsof may emit repeated cwd records for Node's threads. Exercise that
# shape on every platform so wrappers are not accidentally kept ACTIVE.
real_lsof=$(command -v lsof)
export GC_TEST_LSOF="$real_lsof"
cat >"$T/bin/lsof" <<'EOF'
#!/bin/sh
"$GC_TEST_LSOF" "$@" | awk '/^f/ { fd = $0 } { print } /^n/ && fd == "fcwd" { print }'
EOF
chmod +x "$T/bin/lsof"
bash "$GC" --no-fetch --no-sizes >"$T/out" 2>&1
for name in previewdaemon previewapp previewvite previewnpm previewstubborn previewwrites $real_previews; do expect "b/$name" PREVIEW "stop dev preview"; done
expect b/previewnpmcompound ACTIVE
for name in previewmixed previewdirty previewunpushed previewlocked previewopen previewclosed previewforeign previewrelease previewnested; do expect "b/$name" ACTIVE; done
for pid in "$daemon_pid" "$app_pid" "$vite_pid" "$npm_pid"; do kill -0 "$pid" 2>/dev/null && ok || no "dry run stopped preview $pid"; done
rm "$T/bin/lsof"
GH_FAKE_FAIL=1 bash "$GC" --no-fetch --no-sizes >"$T/out" 2>&1
expect b/previewdaemon ACTIVE
# Running in the merged checkout itself still protects it.
(cd "$W/previewdaemon" && bash "$GC" --no-fetch --no-sizes) >"$T/out" 2>&1
expect b/previewdaemon ACTIVE "this session runs here"
# Failure to inspect parentage must not weaken normal process protection.
real_ps=$(command -v ps)
export GC_TEST_PS="$real_ps"
cat >"$T/bin/ps" <<'EOF'
#!/bin/sh
[ "$1" != -axo ] || exit 1
exec "$GC_TEST_PS" "$@"
EOF
chmod +x "$T/bin/ps"
bash "$GC" --no-fetch --no-sizes >"$T/out" 2>&1
expect b/previewdaemon ACTIVE
rm "$T/bin/ps"
bash "$GC" --apply --no-fetch --no-sizes >"$T/apply-previews" 2>&1
for name in previewdaemon previewapp previewvite previewnpm $real_previews; do [ ! -e "$W/$name" ] && ok || no "$name not removed: $(cat "$T/apply-previews")"; done
for pid in "$daemon_pid" "$app_pid" "$vite_pid" "$npm_pid" $real_preview_pids; do kill -0 "$pid" 2>/dev/null && no "preview $pid survived cleanup" || ok; done
[ -d "$W/previewnpmcompound" ] && ok || no "compound npm script was removed"
for name in previewmixed previewdirty previewunpushed previewlocked previewopen previewclosed previewforeign previewrelease previewstubborn previewwrites previewnested; do [ -d "$W/$name" ] && ok || no "protected $name removed"; done
kill -0 "$mixed_pid" 2>/dev/null && ok || no "preview was stopped despite an unrelated holder"
kill -0 "$stubborn" 2>/dev/null && ok || no "stubborn preview was forcibly killed"
grep -q 'preview shutdown timed out' "$T/apply-previews" && ok || no "missing shutdown timeout diagnostic"
[ -f "$W/previewwrites/shutdown-notes.txt" ] && ok || no "lost file written during shutdown"
kill -KILL "$stubborn" 2>/dev/null
stubborn=""

# A new file between the initial process scan and apply must prevent even
# SIGTERM. Inject the edit at the second scan, using the real lsof otherwise.
preview_wt previewrace
start_preview previewrace
race_pid=$preview_pid
real_lsof=$(command -v lsof)
export GC_TEST_LSOF="$real_lsof" GC_TEST_SCAN="$T/scans" GC_TEST_EDIT="$W/previewrace/notes.txt"
cat >"$T/bin/lsof" <<'EOF'
#!/bin/sh
n=0
[ ! -f "$GC_TEST_SCAN" ] || n=$(cat "$GC_TEST_SCAN")
n=$((n + 1))
echo "$n" >"$GC_TEST_SCAN"
[ "$n" != 2 ] || echo 'new work' >"$GC_TEST_EDIT"
exec "$GC_TEST_LSOF" "$@"
EOF
chmod +x "$T/bin/lsof"
sleep 1
bash "$GC" --apply --no-fetch --no-sizes >"$T/race" 2>&1
[ -f "$W/previewrace/notes.txt" ] && ok || no "race edit was lost"
kill -0 "$race_pid" 2>/dev/null && ok || no "preview was stopped after the checkout became dirty"
rm "$T/bin/lsof"

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

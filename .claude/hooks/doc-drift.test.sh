#!/usr/bin/env bash
# Regression table for doc-drift.sh. Run: bash .claude/hooks/doc-drift.test.sh
# (optionally pass a registry path as $1; default docs/features/registry.json, or set
# DOC_DRIFT_REGISTRY). Needs jq and git. Exits non-zero on any failure.
#
# The corpus is the hook's old hard-coded `check` list: one concrete changed path per
# prefix, expecting a warning for its doc — and none once that doc changes too. The
# registry under test must keep covering every row. bash 3.2-safe (no mapfile/assoc).
set -u
here=$(cd "$(dirname "$0")" && pwd)
hook="$here/doc-drift.sh"
reg="${1:-${DOC_DRIFT_REGISTRY:-$here/../../docs/features/registry.json}}"
[ -r "$reg" ] || { echo "doc-drift.test.sh: registry not found: $reg" >&2; exit 1; }
reg=$(cd "$(dirname "$reg")" && pwd)/$(basename "$reg")
command -v jq >/dev/null 2>&1 || { echo "doc-drift.test.sh: jq is required" >&2; exit 1; }

# Scratch repo (so rev-parse works) + a `git` shim that fakes only `status --porcelain`.
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
realgit=$(command -v git)
git -c init.defaultBranch=main init -q "$tmp/repo"
git -C "$tmp/repo" -c user.name=t -c user.email=t@t commit -q --allow-empty -m init
mkdir "$tmp/bin"
cat > "$tmp/bin/git" <<SHIM
#!/bin/sh
if [ "\${1:-}" = status ]; then cat "$tmp/status"; exit 0; fi
exec "$realgit" "\$@"
SHIM
chmod +x "$tmp/bin/git"

pass=0; fail=0
# drift REGISTRY PATH... -> hook's warning lines on stdout (empty when it stays silent).
drift() {
  local r="$1" p; shift
  : > "$tmp/status"
  for p in "$@"; do printf ' M %s\n' "$p" >> "$tmp/status"; done
  ( cd "$tmp/repo" && PATH="$tmp/bin:$PATH" DOC_DRIFT_REGISTRY="$r" bash "$hook" ) \
    | jq -r '.systemMessage // empty' | grep '^- ' || true
}
# warns OUT DOC -> 0 when OUT has a "code under … changed, but DOC wasn't updated." line.
warns() { printf '%s\n' "$1" | grep -qF -- "changed, but $2 wasn't updated."; }
ok() { pass=$((pass+1)); }
bad() { fail=$((fail+1)); printf 'FAIL: %s\n' "$1"; }
expect_warn() { if warns "$2" "$3"; then ok; else bad "$1: expected a warning for $3"; fi; }
expect_quiet() { if [ -z "$2" ]; then ok; else bad "$1: expected no output, got: $2"; fi; }

# path|doc
while IFS='|' read -r path doc; do
  [ -n "$path" ] || continue
  out=$(drift "$reg" "$path");        expect_warn "$path" "$out" "$doc"
  out=$(drift "$reg" "$path" "$doc"); if warns "$out" "$doc"; then bad "$path + $doc: still warns"; else ok; fi
done <<'CORPUS'
crates/chimaera-server/src/x/y.rs|crates/chimaera-server/AGENTS.md
crates/chimaera-agent/src/x/y.rs|crates/chimaera-agent/AGENTS.md
crates/chimaera-pty/src/x/y.rs|crates/chimaera-pty/AGENTS.md
crates/chimaera-app/src/x/y.rs|crates/chimaera-app/AGENTS.md
crates/chimaera-core/src/x/y.rs|crates/chimaera-core/AGENTS.md
crates/chimaera/src/x/y.rs|crates/chimaera/AGENTS.md
web-ui/src/lib/chat/x/y.rs|web-ui/src/lib/chat/AGENTS.md
web-ui/src/lib/dashboard/x/y.rs|web-ui/src/lib/dashboard/AGENTS.md
web-ui/src/lib/settings/x/y.rs|web-ui/src/lib/settings/AGENTS.md
web-ui/src/lib/knowledge/x/y.rs|web-ui/src/lib/knowledge/AGENTS.md
web-ui/src/lib/plugins/x/y.rs|web-ui/src/lib/plugins/AGENTS.md
web-ui/src/lib/layout/x/y.rs|docs/features/workbench.md
crates/chimaera-pty/src/x/y.rs|docs/features/terminals.md
web-ui/src/lib/terminal/x/y.rs|docs/features/terminals.md
crates/chimaera-agent/src/x/y.rs|docs/features/chat-mode.md
web-ui/src/lib/chat/x/y.rs|docs/features/chat-mode.md
crates/chimaera-agent/src/native_ui.rs|docs/features/claude-mods.md
crates/chimaera-agent/src/autonomy.rs|docs/features/loops-and-goals.md
crates/chimaera-server/src/goal_outcome.rs|docs/features/loops-and-goals.md
web-ui/src/lib/chat/AheadCardx.ts|docs/features/loops-and-goals.md
web-ui/src/lib/chat/Modx.ts|docs/features/claude-mods.md
web-ui/src/lib/previews/x/y.rs|docs/features/files-and-previews.md
crates/chimaera-server/src/fs.rs|docs/features/files-and-previews.md
crates/chimaera-server/src/git/x/y.rs|docs/features/git.md
crates/chimaera-server/src/environment.rs|docs/features/environment.md
crates/chimaera-server/src/compute.rs|docs/features/compute.md
crates/chimaera-server/src/mcp.rs|docs/features/linked-terminals.md
crates/chimaera-server/src/comms.rs|docs/features/agent-communication.md
web-ui/src/lib/workspace/comms.svelte.ts|docs/features/agent-communication.md
crates/chimaera-server/src/links.rs|docs/features/linked-terminals.md
crates/chimaera-server/src/launcher.rs|docs/features/agents.md
crates/chimaera-server/src/runtimes.rs|docs/features/agents.md
crates/chimaera-server/src/doc_check.rs|docs/features/agents.md
crates/chimaera-server/src/agent_docs.rs|docs/features/agents.md
crates/chimaera-remote/src/x/y.rs|docs/features/remote-connect.md
crates/chimaera-app/src/x/y.rs|docs/features/native-app.md
crates/chimaera-server/src/ledger.rs|docs/features/lifecycle-and-persistence.md
crates/chimaera-server/src/history/x/y.rs|docs/features/session-history.md
web-ui/src/lib/dashboard/x/y.rs|docs/features/dashboard.md
crates/chimaera-server/src/timeline.rs|docs/features/timeline-and-knowledge.md
crates/chimaera-server/src/episodes.rs|docs/features/timeline-and-knowledge.md
crates/chimaera-server/src/knowledge.rs|docs/features/timeline-and-knowledge.md
plugins/plugins.lock|docs/features/timeline-and-knowledge.md
web-ui/src/lib/knowledge/x/y.rs|docs/features/timeline-and-knowledge.md
web-ui/src/lib/workspace/Timelinex.ts|docs/features/timeline-and-knowledge.md
web-ui/src/lib/workspace/knowledge.ts|docs/features/timeline-and-knowledge.md
crates/chimaera-server/src/plugins/x/y.rs|docs/features/plugins.md
crates/chimaera-server/src/agent_probe.rs|docs/features/plugins.md
plugins/plugins.lock|docs/features/plugins.md
crates/chimaera-plugin-api/x/y.rs|docs/agent-guides/plugins.md
plugins/x/y.rs|plugins/AGENTS.md
web-ui/src/lib/plugins/x/y.rs|docs/features/plugins.md
web-ui/src/lib/settings/x/y.rs|docs/features/settings.md
crates/chimaera/src/x/y.rs|docs/features/cli.md
crates/chimaera-agent/src/cron.rs|docs/features/loops-and-goals.md
web-ui/src/lib/chat/WorkStrip.svelte|docs/features/loops-and-goals.md
web-ui/src/lib/chat/aheadState.ts|docs/features/loops-and-goals.md
web-ui/src/lib/chat/AheadCard.svelte|docs/features/loops-and-goals.md
web-ui/src/lib/chat/nativeUi.ts|docs/features/claude-mods.md
web-ui/src/lib/chat/modHost.ts|docs/features/claude-mods.md
web-ui/src/lib/workspace/Timeline.svelte|docs/features/timeline-and-knowledge.md
web-ui/src/lib/workspace/timelineStore.ts|docs/features/timeline-and-knowledge.md
CORPUS

# Behaviours independent of the real registry's contents.
cat > "$tmp/glob.json" <<'JSON'
{"areas":[{"id":"deep","map":"d.md","paths":["a/**"]},
 {"id":"flat","map":"f.md","paths":["b/*.rs"]},
 {"id":"mid","map":"m.md","paths":["c/**/d.rs"]},
 {"id":"one","map":"o.md","paths":["e?.txt","x.y"]}],"features":[]}
JSON
g="$tmp/glob.json"
expect_warn  "** nested"        "$(drift "$g" a/x/y/z.rs)" d.md
expect_warn  "* same segment"   "$(drift "$g" b/x.rs)" f.md
expect_quiet "* no slash cross" "$(drift "$g" b/sub/x.rs)"
expect_warn  "**/ zero dirs"    "$(drift "$g" c/d.rs)" m.md
expect_warn  "**/ many dirs"    "$(drift "$g" c/m/n/d.rs)" m.md
expect_warn  "? one char"       "$(drift "$g" ef.txt)" o.md
expect_quiet "? not slash"      "$(drift "$g" e/.txt)"
expect_quiet "dot is literal"   "$(drift "$g" xzy)"
expect_warn  "exact file"       "$(drift "$g" x.y)" o.md
expect_quiet "anchored"         "$(drift "$g" zz/a/q.rs)"
expect_quiet "no glob matches"  "$(drift "$reg" zzz/unrelated.txt)"
expect_quiet "empty change set" "$(drift "$reg")"
expect_quiet "doc present"      "$(drift "$g" a/q.rs d.md)"

printf ' M a/q.rs\n' > "$tmp/status"
out=$( cd "$tmp/repo" && PATH=/nonexistent "$BASH" "$hook" 2>&1 ); rc=$?
if [ -z "$out" ] && [ "$rc" -eq 0 ]; then ok; else bad "jq missing: rc=$rc out=$out"; fi
printf 'not json' > "$tmp/bad.json"
for r in "$tmp/bad.json" "$tmp/missing.json"; do
  out=$( cd "$tmp/repo" && PATH="$tmp/bin:$PATH" DOC_DRIFT_REGISTRY="$r" bash "$hook" 2>&1 ); rc=$?
  if [ -z "$out" ] && [ "$rc" -eq 0 ]; then ok; else bad "bad registry $r: rc=$rc out=$out"; fi
done

if [ "$fail" -eq 0 ]; then echo "✓ doc-drift: $pass cases"; else echo "doc-drift.test.sh: pass=$pass fail=$fail"; exit 1; fi

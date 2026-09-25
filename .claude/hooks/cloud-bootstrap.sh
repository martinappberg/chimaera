#!/usr/bin/env bash
# SessionStart(startup|resume), Claude Code CLOUD sessions only: make a fresh cloud
# clone buildable. The environment setup script (scripts/cloud-env-setup.sh)
# provisions the machine once per cache; this does the per-clone work, since every
# cloud session starts from a fresh clone with no node_modules or web-ui/dist.
# No-op locally. Idempotent, so a resume costs almost nothing. stdout is injected
# into the session context — keep it to a few lines. Guide:
# docs/agent-guides/cloud-sessions.md.
set -u
[ "${CLAUDE_CODE_REMOTE:-}" = "true" ] || exit 0
root="${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel 2>/dev/null)}"
cd "$root" 2>/dev/null || exit 0
mkdir -p target # gitignored; holds the log so it never shows up in git status
log="target/cloud-bootstrap.log"
notes=""
note() { notes="${notes}
- $1"; }

want=$(cat .nvmrc 2>/dev/null)
have=$(node -p 'process.versions.node.split(".")[0]' 2>/dev/null || echo none)
[ "$have" = "$want" ] || note "node is v${have} but .nvmrc pins ${want}; switch before web-ui work."

# rust-embed derives from web-ui/dist at COMPILE time: until dist exists, every
# cargo build/clippy/test of the workspace fails (E0599). So install + build first,
# synchronously, before Claude's first cargo command can race it.
if [ ! -d web-ui/node_modules ] \
   || [ web-ui/package-lock.json -nt web-ui/node_modules/.package-lock.json ]; then
  npm --prefix web-ui ci --no-audit --no-fund >>"$log" 2>&1 \
    || note "npm ci failed (see $log)."
fi
if [ ! -f web-ui/dist/index.html ]; then
  npm --prefix web-ui run build >>"$log" 2>&1 \
    || note "web-ui build failed (see $log); cargo builds will fail until it exists."
fi

# A cold workspace compile is the slowest part of a cloud session, so start the
# gate's builds now, at low priority, while Claude reads code. Cargo's build lock
# makes a concurrent cargo command wait ("Blocking waiting for file lock") and then
# reuse this work. Opt out with CHIMAERA_CLOUD_WARM=0 in the environment variables.
if [ "${CHIMAERA_CLOUD_WARM:-1}" != "0" ] && [ -f web-ui/dist/index.html ] \
   && ! pgrep -f 'cargo-warm-chimaera' >/dev/null 2>&1; then
  detach=""
  command -v setsid >/dev/null 2>&1 && detach="setsid"
  $detach nohup nice -n 10 bash -c ': cargo-warm-chimaera;
    cargo clippy --workspace --all-targets && cargo test --workspace --no-run' \
    </dev/null >>"$log" 2>&1 &
  note "warming cargo clippy + test builds in the background (log: $log); a cargo command may wait on its lock."
fi

state="web-ui deps + dist ready."
[ -f web-ui/dist/index.html ] || state="web-ui/dist is MISSING."
echo "Cloud session bootstrap (.claude/hooks/cloud-bootstrap.sh): ${state}${notes}"
echo "- Cloud limits (no browser pane, no chat-smoke, no HPC hosts): docs/agent-guides/cloud-sessions.md."
exit 0

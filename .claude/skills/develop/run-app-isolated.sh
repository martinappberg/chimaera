#!/usr/bin/env bash
#
# Run THIS worktree's full Tauri app (the native shell) against an ISOLATED
# state dir, so a dev build never touches your real ~/.chimaera — its windows,
# saved hosts, sessions, or the manifest/port a running app owns. The app spawns
# its OWN daemon of THIS worktree's build on a free port, all under
# CHIMAERA_HOME, so you exercise the full binary end to end — the native
# clipboard command, the reauth overlay, the daemon changes — in isolation,
# alongside your real app.
#
# The app's shell state, its daemon's state, and (over ssh) the spawned shells
# all inherit CHIMAERA_HOME, so nothing lands in the shared ~/.chimaera. The real
# $HOME is untouched, so ~/.claude auth still works. State lives under
# ~/.chimaera-dev-app/<worktree> (a short $HOME base — see below). Mirrors
# serve-isolated.sh, for the app instead of the bare daemon. See the develop
# skill.
set -euo pipefail

ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
# The state dir MUST stay short: it holds unix sockets (the askpass relay, the
# ssh ControlMaster) whose full path has to fit the ~104-byte `sun_path` limit.
# A CHIMAERA_HOME *inside* the deep worktree path overshoots and the socket binds
# fail (askpass can't reach the app → ssh auth dies; ssh mux fails). So anchor it
# in $HOME under a per-worktree subdir: short base, still isolated per worktree.
export CHIMAERA_HOME="$HOME/.chimaera-dev-app/$(basename "$ROOT")"
mkdir -p "$CHIMAERA_HOME"

BUILD_BIN="$ROOT/crates/chimaera-app/target/debug/chimaera"
if [ ! -x "$BUILD_BIN" ]; then
  echo "app not built — run first:  cargo build --manifest-path crates/chimaera-app/Cargo.toml" >&2
  exit 1
fi
if [ ! -f "$ROOT/web-ui/dist/index.html" ]; then
  echo "web-ui not built — run first (Node 22):  npm --prefix web-ui ci && npm --prefix web-ui run build" >&2
  exit 1
fi

# Give the isolated GUI process a distinct OS identity. Agents and humans often
# keep the released `chimaera` app open while testing; launching the build under
# that same identity makes Computer Use and Activity Monitor ambiguous. macOS
# keys accessibility apps by bundle id, not merely executable name, so wrap the
# exact Cargo binary in a generated development bundle. Other platforms only
# need the distinct executable name. Hard links keep both paths cheap and exact.
if [ "$(uname -s)" = "Darwin" ]; then
  DEV_APP="$ROOT/crates/chimaera-app/target/debug/chimaera-dev.app"
  DEV_BIN="$DEV_APP/Contents/MacOS/chimaera-dev"
  DEV_PLIST="$DEV_APP/Contents/Info.plist"
  mkdir -p "$DEV_APP/Contents/MacOS"
  ln -f "$BUILD_BIN" "$DEV_BIN"
  plutil -create xml1 "$DEV_PLIST"
  plutil -insert CFBundleDisplayName -string "chimaera-dev" "$DEV_PLIST"
  plutil -insert CFBundleExecutable -string "chimaera-dev" "$DEV_PLIST"
  plutil -insert CFBundleIdentifier -string "dev.chimaera.app.dev" "$DEV_PLIST"
  plutil -insert CFBundleInfoDictionaryVersion -string "6.0" "$DEV_PLIST"
  plutil -insert CFBundleName -string "chimaera-dev" "$DEV_PLIST"
  plutil -insert CFBundlePackageType -string "APPL" "$DEV_PLIST"
  plutil -insert CFBundleShortVersionString -string "0.0.1" "$DEV_PLIST"
  plutil -insert CFBundleVersion -string "0.0.1" "$DEV_PLIST"
  plutil -insert NSHighResolutionCapable -bool true "$DEV_PLIST"
else
  DEV_BIN="$ROOT/crates/chimaera-app/target/debug/chimaera-dev"
  ln -f "$BUILD_BIN" "$DEV_BIN"
fi

echo "launching isolated chimaera-dev app" >&2
echo "  CHIMAERA_HOME = $CHIMAERA_HOME" >&2
echo "  daemon log    = $CHIMAERA_HOME/data/logs/serve.log" >&2
echo "  (a debug daemon reads web-ui/dist from disk — after a UI change, rebuild the UI and reload the window; no app restart)" >&2

# Scrub nested-agent env: launched from inside a Claude Code session (a coding
# agent driving this script), the app → daemon → spawned claudes would inherit
# CLAUDECODE / ANTHROPIC_BASE_URL etc. and every agent session dies on startup.
SCRUB=()
while IFS= read -r var; do SCRUB+=(-u "$var"); done \
  < <(env | grep -iE '^(CLAUDE|ANTHROPIC)[A-Z_]*=' | cut -d= -f1)

# exec so a Ctrl-C / kill reaches the app directly. CHIMAERA_HOME is inherited by
# the app, the daemon it spawns, and the shells that daemon spawns.
# ${SCRUB[@]+…}: macOS ships bash 3.2, where "${SCRUB[@]}" under `set -u`
# dies on an empty array — the normal case for a HUMAN running this script.
exec env ${SCRUB[@]+"${SCRUB[@]}"} "$DEV_BIN"

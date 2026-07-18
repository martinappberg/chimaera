# Chimaera dev tasks. Install just: https://github.com/casey/just

default: check

# Build the web UI into web-ui/dist (required before release builds)
ui:
    cd web-ui && npm install && npm run build

# Format, lint, test
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

fmt:
    cargo fmt --all

# Live agent-protocol smoke tests against the real installed claude/codex
# binaries (needs auth + network, bills a few tiny turns). Run whenever
# chimaera-agent's protocol clients change or the CLIs update.
chat-smoke:
    cargo test -p chimaera-agent --test live -- --ignored --test-threads=1 --nocapture

# Run the daemon locally (foreground)
serve: ui
    cargo run -p chimaera -- serve

# Vite dev server with proxy to a running daemon
dev-ui:
    cd web-ui && npm run dev

# Native shell (standalone cargo workspace; Tauri stays out of the daemon
# workspace). Debug run + fmt/clippy, and the bundled .app/.dmg.
app-dev: ui
    cd crates/chimaera-app && cargo run

# The full app on an ISOLATED state dir (its own daemon of this build + a free
# port, all under ~/.chimaera-dev-app/<worktree> — a short $HOME base so its
# unix sockets fit sun_path) so a dev build never touches your real ~/.chimaera
# — windows, saved hosts, sessions. Worktree-safe; runs alongside your real
# app. See the develop skill.
app-dev-isolated: ui
    cd crates/chimaera-app && cargo build
    bash .claude/skills/develop/run-app-isolated.sh

app-check:
    cd crates/chimaera-app && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test

app-build:
    cd crates/chimaera-app && npm install && npx tauri build

# Static musl builds (requires cargo-zigbuild + zig)
release-linux: ui
    cargo zigbuild --release --target x86_64-unknown-linux-musl -p chimaera
    cargo zigbuild --release --target aarch64-unknown-linux-musl -p chimaera

# Build deployable linux binaries into ~/.chimaera/dist, where `connect`
# (CLI and native shell) looks when auto-installing on a remote host.
dist: release-linux
    mkdir -p ~/.chimaera/dist
    cp target/x86_64-unknown-linux-musl/release/chimaera ~/.chimaera/dist/chimaera-x86_64-linux-musl
    cp target/aarch64-unknown-linux-musl/release/chimaera ~/.chimaera/dist/chimaera-aarch64-linux-musl

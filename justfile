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

# Run the daemon locally (foreground)
serve: ui
    cargo run -p chimaera -- serve

# Vite dev server with proxy to a running daemon
dev-ui:
    cd web-ui && npm run dev

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

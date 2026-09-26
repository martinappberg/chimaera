#!/bin/bash
# Claude Code cloud environment setup script for Chimaera.
#
# NOT run from the repo: paste this file's contents into the cloud environment's
# "Setup script" field (claude.ai/code → environment settings). This copy is the
# versioned source of truth; re-paste it when it changes. Guide:
# docs/agent-guides/cloud-sessions.md.
#
# Runs as root on Ubuntu 24.04 before Claude launches, once per environment cache
# (~7 days, or whenever the script/network settings change); every later session
# starts from the filesystem snapshot it leaves. So it provisions the MACHINE —
# per-clone work (npm ci, web-ui/dist) lives in .claude/hooks/cloud-bootstrap.sh.
# Constraints: must exit 0 (a failure blocks the session), finish in ~5 minutes,
# and not assume the repo is on disk.
set -u

# Pre-install the pinned compiler + musl/WASM targets so they're in the snapshot instead
# of downloaded per session. rust-toolchain.toml stays the one source of truth:
# read it from main (the cache rebuilds weekly, so this tracks bumps); the literal
# is only a fallback. A stale pin costs a per-session download, never correctness —
# rustup still auto-installs whatever rust-toolchain.toml names on first use.
TOOLCHAIN=$(curl -fsSL https://raw.githubusercontent.com/martinappberg/chimaera/main/rust-toolchain.toml 2>/dev/null \
  | sed -n 's/^channel *= *"\([^"]*\)".*/\1/p')
TOOLCHAIN=${TOOLCHAIN:-1.96.0}

if ! command -v rustup >/dev/null 2>&1; then
  # sh.rustup.rs is not on the default Trusted allowlist; static.rust-lang.org is.
  # Set a default toolchain: without one, cargo outside the repo (the `just`
  # fallback below included) fails with "no default toolchain configured".
  curl -sSf https://static.rust-lang.org/rustup/rustup-init.sh \
    | sh -s -- -y --profile minimal --default-toolchain "$TOOLCHAIN" || true
  . "$HOME/.cargo/env" 2>/dev/null || true
fi
rustup toolchain install "$TOOLCHAIN" --profile minimal \
  --component rustfmt --component clippy \
  --target x86_64-unknown-linux-musl --target aarch64-unknown-linux-musl \
  --target wasm32-wasip2 || true

# `just` runs the gate (`just check`). Ubuntu 24.04 packages it; crates.io is the
# fallback if the apt mirror is unreachable.
if ! command -v just >/dev/null 2>&1; then
  { apt-get update -qq && apt-get install -y -qq just; } \
    || cargo install --locked just || true
fi

# Optional: uncomment to build/check the Tauri app (crates/chimaera-app, `just
# app-check`) in the cloud. Tauri v2's webkit2gtk-4.1 set, mirroring app.yml.
# apt-get install -y -qq libwebkit2gtk-4.1-dev build-essential file \
#   libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev || true

exit 0

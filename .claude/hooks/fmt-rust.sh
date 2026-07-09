#!/usr/bin/env bash
# PostToolUse(Edit|Write): format a changed Rust file with the PINNED toolchain,
# matching CI's `cargo fmt --all --check`. Single-file rustfmt keeps the hot path
# fast; clippy is per-crate and slow, so it stays in `just check`, not here.
# Degrades to a no-op if jq or the pinned toolchain is missing.
set -u
command -v jq >/dev/null 2>&1 || exit 0
f=$(jq -r '.tool_input.file_path // .tool_response.filePath // empty' 2>/dev/null)
[ -n "$f" ] || exit 0
[ "${f##*.}" = "rs" ] || exit 0
[ -f "$f" ] || exit 0
command -v rustfmt >/dev/null 2>&1 || exit 0
rustfmt +1.96.0 --edition 2021 "$f" 2>/dev/null || true
exit 0

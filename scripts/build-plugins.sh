#!/usr/bin/env bash
#
# Build every first-party plugin (plugins/*, its own cargo workspace) to a
# WASM component and lay it out for the daemon, which embeds the result like
# web-ui/dist (a debug daemon reads it from disk instead):
#
#   plugins/dist/<id>/{plugin.wasm,plugin.toml}       shipped plugins
#   plugins/dist-test/<id>/{plugin.wasm,plugin.toml}  test-only (crates named test-*)
#
# Run before any build of chimaera-server (CI does; `just plugins`). A plugin
# moving to its own repository becomes a line here that downloads its pinned
# release artifact and checks its checksum instead of building it.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET=wasm32-wasip2
# The one toolchain pin is rust-toolchain.toml.
TOOLCHAIN="$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' "$ROOT/rust-toolchain.toml")"
if [ -z "$TOOLCHAIN" ]; then
  echo "build-plugins: no channel in rust-toolchain.toml" >&2
  exit 1
fi
if ! rustup target list --installed --toolchain "$TOOLCHAIN" 2>/dev/null | grep -qx "$TARGET"; then
  echo "build-plugins: the $TARGET target is not installed for Rust $TOOLCHAIN." >&2
  echo "  rustup target add $TARGET --toolchain $TOOLCHAIN" >&2
  exit 1
fi

cargo +"$TOOLCHAIN" build --manifest-path "$ROOT/plugins/Cargo.toml" --target "$TARGET" --release

OUT="$ROOT/plugins/target/$TARGET/release"
rm -rf "$ROOT/plugins/dist" "$ROOT/plugins/dist-test"
mkdir -p "$ROOT/plugins/dist" "$ROOT/plugins/dist-test"
for manifest in "$ROOT"/plugins/*/plugin.toml; do
  dir="$(dirname "$manifest")"
  crate="$(basename "$dir")"
  id="$(sed -n 's/^id *= *"\([^"]*\)".*/\1/p' "$manifest" | head -n 1)"
  lib="$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$dir/Cargo.toml" | head -n 1 | tr - _)"
  if [ -z "$id" ] || [ -z "$lib" ] || [ ! -f "$OUT/$lib.wasm" ]; then
    echo "build-plugins: $crate: no id, crate name, or $lib.wasm" >&2
    exit 1
  fi
  case "$crate" in
    test-*) dest="$ROOT/plugins/dist-test/$id" ;;
    *) dest="$ROOT/plugins/dist/$id" ;;
  esac
  mkdir -p "$dest"
  cp "$OUT/$lib.wasm" "$dest/plugin.wasm"
  cp "$manifest" "$dest/plugin.toml"
  echo "build-plugins: $id -> ${dest#"$ROOT"/} ($(wc -c <"$dest/plugin.wasm" | tr -d ' ') bytes)"
done

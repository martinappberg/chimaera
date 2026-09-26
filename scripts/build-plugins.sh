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

# The manifest is the one source of truth for a plugin's version and API,
# so it must agree with the crate and the WIT rather than have them injected:
# `version` equals the crate's (resolved) package version, and `api` is the
# WIT package's MAJOR.MINOR that chimaera-plugin-api publishes.
WIT="$ROOT/crates/chimaera-plugin-api/wit/chimaera.wit"
API="$(sed -n 's/^package chimaera:plugin@\([0-9]*\.[0-9]*\)\..*/\1/p' "$WIT" | head -n 1)"
if [ -z "$API" ]; then
  echo "build-plugins: no package version in ${WIT#"$ROOT"/}" >&2
  exit 1
fi
for manifest in "$ROOT"/plugins/*/plugin.toml; do
  dir="$(dirname "$manifest")"
  crate="$(basename "$dir")"
  package="$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$dir/Cargo.toml" | head -n 1)"
  version="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$manifest" | head -n 1)"
  api="$(sed -n 's/^api *= *"\([^"]*\)".*/\1/p' "$manifest" | head -n 1)"
  pkgid="$(cargo +"$TOOLCHAIN" pkgid --manifest-path "$ROOT/plugins/Cargo.toml" -p "$package")"
  crate_version="${pkgid##*@}"
  crate_version="${crate_version##*#}"
  if [ -z "$version" ] || [ "$version" != "$crate_version" ]; then
    echo "build-plugins: $crate: plugin.toml version \"$version\" is not the crate's $crate_version" >&2
    exit 1
  fi
  if [ "$api" != "$API" ]; then
    echo "build-plugins: $crate: plugin.toml api \"$api\" is not the WIT's $API" >&2
    exit 1
  fi
done

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

# The fixture's "next release", for the daemon's update tests: the same
# crate built with its `v2` feature (one more tool) beside its v2 manifest.
# Never embedded as a plugin of its own — the tests serve it from a fake
# releases server.
cargo +"$TOOLCHAIN" build --manifest-path "$ROOT/plugins/Cargo.toml" --target "$TARGET" --release \
  -p chimaera-plugin-test-fixture --features v2
dest="$ROOT/plugins/dist-test/test-fixture-v2"
mkdir -p "$dest"
cp "$OUT/chimaera_plugin_test_fixture.wasm" "$dest/plugin.wasm"
cp "$ROOT/plugins/test-fixture/plugin-v2.toml" "$dest/plugin.toml"
echo "build-plugins: test-fixture 0.2.0 -> ${dest#"$ROOT"/} ($(wc -c <"$dest/plugin.wasm" | tr -d ' ') bytes)"

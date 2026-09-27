#!/usr/bin/env bash
#
# Lay out the plugins the daemon embeds, like web-ui/dist (a debug daemon
# reads them from disk instead):
#
#   plugins/dist/<id>/{plugin.wasm,plugin.toml}       the first-party plugins
#   plugins/dist-test/<id>/{plugin.wasm,plugin.toml}  the host's test fixture
#
# 1. The first-party plugins live in their own repositories, pinned by
#    plugins/plugins.lock. For each [[plugin]] there, the release's
#    plugin.wasm and plugin.toml are downloaded
#    (https://github.com/<repo>/releases/download/v<version>/<file>), both
#    verified against the lock's sha256s, and the manifest's version checked
#    against the lock's. Downloads are kept in plugins/cache/<id>-<version>/
#    (gitignored), so a second build never refetches; a cached file is
#    re-verified against the lock on every run, never trusted.
#    A build without network needs that cache, or an override (below), for
#    every locked plugin.
# 2. A local override, for developing a plugin against the daemon:
#    plugins/plugins.local.toml (gitignored), one table per plugin,
#        [[plugin]]
#        id = "agent-notes"
#        path = "../../chimaera-plugin-agent-notes"   # absolute, ~/..., or relative to plugins/
#    builds that checkout (`cargo build --release --target wasm32-wasip2`, on
#    the checkout's own toolchain) and lays out its component and its
#    plugin.toml instead of the locked release, with no version check against
#    the lock. An id the lock doesn't name is laid out too (a new plugin).
# 3. plugins/test-fixture, the one crate of the plugins/ cargo workspace, is
#    built here into plugins/dist-test, with its v2 variant for the update
#    tests.
#
# Run before any build of chimaera-server (CI and release do; `just plugins`).
# Works with sha256sum (Linux) or shasum (macOS), and bash 3.2.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PLUGINS="$ROOT/plugins"
LOCK="$PLUGINS/plugins.lock"
LOCAL="$PLUGINS/plugins.local.toml"
CACHE="$PLUGINS/cache"
TARGET=wasm32-wasip2
# A component's size cap: the daemon's own (plugins::installed::WASM_MAX).
MAX_BYTES=16777216
SEP=$'\037'

say() { echo "build-plugins: $*"; }
die() {
  echo "build-plugins: $*" >&2
  exit 1
}
rel() { printf '%s\n' "${1#"$ROOT"/}"; }

if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
  die "neither sha256sum nor shasum is installed"
fi

# The one toolchain pin is rust-toolchain.toml.
TOOLCHAIN="$(sed -n 's/^channel *= *"\([^"]*\)".*/\1/p' "$ROOT/rust-toolchain.toml")"
[ -n "$TOOLCHAIN" ] || die "no channel in rust-toolchain.toml"
if ! rustup target list --installed --toolchain "$TOOLCHAIN" 2>/dev/null | grep -qx "$TARGET"; then
  echo "build-plugins: the $TARGET target is not installed for Rust $TOOLCHAIN." >&2
  echo "  rustup target add $TARGET --toolchain $TOOLCHAIN" >&2
  exit 1
fi

# The `[[plugin]]` tables of a lock-shaped file, one line each: the values of
# the named keys in that order, separated by $SEP. Only `[[plugin]]` headers,
# `key = "value"` lines, comments and blank lines are allowed; anything else,
# an unknown or repeated key, or a missing one is an error naming the line.
read_tables() { # file key...
  local file="$1"
  shift
  awk -v keys="$*" -v file="$(rel "$file")" -v sep="$SEP" '
    function fail(msg) { printf "build-plugins: %s: %s\n", file, msg > "/dev/stderr"; failed = 1; exit 1 }
    function flush(   i, out) {
      if (!open) return
      out = ""
      for (i = 1; i <= n; i++) {
        if (!(want[i] in val)) fail("the [[plugin]] at line " start " has no " want[i])
        out = out (i > 1 ? sep : "") val[want[i]]
      }
      print out
      split("", val)
    }
    BEGIN { n = split(keys, want, " ") }
    /^[ \t]*(#.*)?$/ { next }
    /^[ \t]*\[\[plugin\]\][ \t]*(#.*)?$/ { flush(); open = 1; start = NR; next }
    open && /^[ \t]*[a-z0-9_]+[ \t]*=[ \t]*"[^"]*"[ \t]*(#.*)?$/ {
      key = $0; sub(/^[ \t]*/, "", key); sub(/[ \t]*=.*$/, "", key)
      v = $0; sub(/^[^=]*=[ \t]*"/, "", v); sub(/".*$/, "", v)
      known = 0
      for (i = 1; i <= n; i++) if (want[i] == key) known = 1
      if (!known) fail("line " NR ": unknown key " key)
      if (key in val) fail("line " NR ": " key " is set twice")
      val[key] = v
      next
    }
    { fail("line " NR ": not a [[plugin]] header or a key = \"value\" line") }
    END { if (failed) exit 1; flush() }
  ' "$file"
}

# A top-level `key = "value"` of a plugin.toml (before its first table).
toml_top() { sed -n -e '/^[[:space:]]*\[/q' -e "s/^$2 *= *\"\\([^\"]*\\)\".*/\\1/p" "$1"; }
# The manifest's `[release] github`, if it names one.
toml_release() { sed -n -e '/^\[release\]/,/^\[/s/^github *= *"\([^"]*\)".*/\1/p' "$1"; }

ID_RE='^[a-z0-9][a-z0-9-]*$'
VERSION_RE='^[0-9]+\.[0-9]+\.[0-9]+$'
REPO_RE='^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$'
SHA_RE='^[0-9a-f]{64}$'

fetch() { # url dest
  curl -fsSL --proto '=https' --proto-redir '=https' --max-filesize "$MAX_BYTES" \
    --connect-timeout 20 --max-time 300 --retry 3 --retry-delay 2 -o "$2" "$1"
}

size() { wc -c <"$1" | tr -d ' '; }

# A locked plugin: its release's two files, from the cache when they are
# there and still match the lock, else downloaded and verified; then its
# manifest checked against the lock and both laid out in `dest/<id>`.
lay_out_locked() { # id version repo sha256_wasm sha256_toml dest
  local id="$1" version="$2" repo="$3" dest="$6"
  local cache="$CACHE/$id-$version"
  local base="https://github.com/$repo/releases/download/v$version"
  local file want got fetched=0
  mkdir -p "$cache"
  for file in plugin.wasm plugin.toml; do
    case "$file" in
      plugin.wasm) want="$4" ;;
      *) want="$5" ;;
    esac
    if [ -f "$cache/$file" ]; then
      got="$(sha256 "$cache/$file")"
      [ "$got" = "$want" ] && continue
      # Kept until a verified download replaces it: a wrong lock must not
      # cost an offline machine its cache.
      say "$id $version: the cached $file does not match plugins/plugins.lock (expected sha256 $want, got $got); downloading it again"
    fi
    say "$id $version: downloading $base/$file"
    if ! fetch "$base/$file" "$cache/.$file.part"; then
      rm -f "$cache/.$file.part"
      die "$id $version: could not download $base/$file (a build without network needs $(rel "$cache")/ or an override in plugins/plugins.local.toml)"
    fi
    got="$(sha256 "$cache/.$file.part")"
    if [ "$got" != "$want" ]; then
      rm -f "$cache/.$file.part"
      die "$id $version: $file from $base does not match plugins/plugins.lock: expected sha256 $want, got $got"
    fi
    mv "$cache/.$file.part" "$cache/$file"
    fetched=$((fetched + 1))
  done
  local mid mversion mrepo
  mid="$(toml_top "$cache/plugin.toml" id)"
  mversion="$(toml_top "$cache/plugin.toml" version)"
  mrepo="$(toml_release "$cache/plugin.toml")"
  [ "$mid" = "$id" ] || die "$id $version: the release's plugin.toml is for id \"$mid\", not \"$id\""
  [ "$mversion" = "$version" ] ||
    die "$id $version: the release's plugin.toml says version \"$mversion\", plugins/plugins.lock says $version"
  if [ -n "$mrepo" ] && [ "$mrepo" != "$repo" ]; then
    die "$id $version: the release's plugin.toml names [release] github = \"$mrepo\", plugins/plugins.lock says $repo"
  fi
  mkdir -p "$dest/$id"
  cp "$cache/plugin.wasm" "$dest/$id/plugin.wasm"
  cp "$cache/plugin.toml" "$dest/$id/plugin.toml"
  if [ "$fetched" = 0 ]; then
    say "$id $version -> plugins/dist/$id ($(size "$cache/plugin.wasm") bytes; from $(rel "$cache")/, sha256s re-verified against the lock, nothing fetched)"
  else
    say "$id $version -> plugins/dist/$id ($(size "$cache/plugin.wasm") bytes; downloaded from $repo, sha256s verified against the lock)"
  fi
}

# An override: build the checkout at `path` and lay out its component and its
# plugin.toml in `dest/<id>`. `locked` is the lock's version for the id
# (empty when the lock doesn't name it).
lay_out_override() { # id path dest locked
  local id="$1" path="$2" dest="$3" locked="$4"
  case "$path" in
    /*) ;;
    "~/"*) path="$HOME/${path#"~/"}" ;;
    *) path="$PLUGINS/$path" ;;
  esac
  if [ ! -f "$path/Cargo.toml" ] || [ ! -f "$path/plugin.toml" ]; then
    die "$id: the override's path $path has no Cargo.toml and plugin.toml (plugins/plugins.local.toml)"
  fi
  say "$id: building the local override at $path (plugins/plugins.local.toml), not the locked release"
  local out wasm mid mversion
  # The checkout's own directory, so rustup picks its rust-toolchain.toml.
  out="$(cd "$path" && cargo build --release --target "$TARGET" --message-format=json-render-diagnostics)" ||
    die "$id: cargo build failed in $path"
  wasm="$(printf '%s\n' "$out" | grep '"reason":"compiler-artifact"' | grep -o '"[^"]*\.wasm"' | tail -n 1 | tr -d '"' || true)"
  if [ -z "$wasm" ] || [ ! -f "$wasm" ]; then
    die "$id: the build in $path produced no .wasm (a plugin crate is a cdylib)"
  fi
  mid="$(toml_top "$path/plugin.toml" id)"
  mversion="$(toml_top "$path/plugin.toml" version)"
  [ "$mid" = "$id" ] || die "$id: $path/plugin.toml is for id \"$mid\", not \"$id\""
  mkdir -p "$dest/$id"
  cp "$wasm" "$dest/$id/plugin.wasm"
  cp "$path/plugin.toml" "$dest/$id/plugin.toml"
  say "$id $mversion -> plugins/dist/$id ($(size "$wasm") bytes; built from $path)"
  if [ -n "$locked" ]; then
    say "$id: an override: the version check against plugins/plugins.lock ($locked) is skipped"
  else
    say "$id: not in plugins/plugins.lock: laid out from the override only"
  fi
}

# Staged, then swapped in at the end: a failed run keeps the last good layout.
STAGE="$CACHE/.stage"
rm -rf "$STAGE"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/dist" "$STAGE/dist-test"

[ -f "$LOCK" ] || die "plugins/plugins.lock is missing"
LOCKED="$(read_tables "$LOCK" id version repo sha256_wasm sha256_toml)"
OVERRIDES=""
if [ -f "$LOCAL" ]; then
  OVERRIDES="$(read_tables "$LOCAL" id path)"
fi

# The override for `id`, if plugins.local.toml names one.
override_for() {
  local oid opath
  while IFS="$SEP" read -r oid opath; do
    if [ -n "$oid" ] && [ "$oid" = "$1" ]; then
      printf '%s\n' "$opath"
      return 0
    fi
  done <<<"$OVERRIDES"
  return 1
}

seen=" "
while IFS="$SEP" read -r id version repo sha_wasm sha_toml <&3; do
  [ -n "$id" ] || continue
  sha_wasm="$(printf '%s' "$sha_wasm" | tr 'A-F' 'a-f')"
  sha_toml="$(printf '%s' "$sha_toml" | tr 'A-F' 'a-f')"
  [[ $id =~ $ID_RE ]] || die "plugins/plugins.lock: id \"$id\" is not lowercase letters, digits and dashes"
  [[ $version =~ $VERSION_RE ]] || die "plugins/plugins.lock: $id: version \"$version\" is not MAJOR.MINOR.PATCH"
  [[ $repo =~ $REPO_RE ]] || die "plugins/plugins.lock: $id: repo \"$repo\" is not owner/name"
  [[ $sha_wasm =~ $SHA_RE ]] || die "plugins/plugins.lock: $id: sha256_wasm is not 64 hex digits"
  [[ $sha_toml =~ $SHA_RE ]] || die "plugins/plugins.lock: $id: sha256_toml is not 64 hex digits"
  case "$seen" in *" $id "*) die "plugins/plugins.lock names $id twice" ;; esac
  seen="$seen$id "
  if opath="$(override_for "$id")"; then
    lay_out_override "$id" "$opath" "$STAGE/dist" "$version"
  else
    lay_out_locked "$id" "$version" "$repo" "$sha_wasm" "$sha_toml" "$STAGE/dist"
  fi
done 3<<<"$LOCKED"

overridden=" "
while IFS="$SEP" read -r id opath <&3; do
  [ -n "$id" ] || continue
  [[ $id =~ $ID_RE ]] || die "plugins/plugins.local.toml: id \"$id\" is not lowercase letters, digits and dashes"
  case "$overridden" in *" $id "*) die "plugins/plugins.local.toml names $id twice" ;; esac
  overridden="$overridden$id "
  case "$seen" in *" $id "*) continue ;; esac
  lay_out_override "$id" "$opath" "$STAGE/dist" ""
done 3<<<"$OVERRIDES"

# The test fixture. Its manifest is the one source of truth for its version
# and API, so it must agree with the crate and the WIT rather than have them
# injected: `version` equals the crate's (resolved) package version, and
# `api` is the WIT package's MAJOR.MINOR that chimaera-plugin-api publishes.
WIT="$ROOT/crates/chimaera-plugin-api/wit/chimaera.wit"
API="$(sed -n 's/^package chimaera:plugin@\([0-9]*\.[0-9]*\)\..*/\1/p' "$WIT")"
[ -n "$API" ] || die "no package version in $(rel "$WIT")"
for manifest in "$PLUGINS"/*/plugin.toml; do
  dir="$(dirname "$manifest")"
  crate="$(basename "$dir")"
  case "$crate" in
    test-*) ;;
    *) die "plugins/$crate is not a test crate: a first-party plugin ships from its own repository through plugins/plugins.lock" ;;
  esac
  package="$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$dir/Cargo.toml")"
  version="$(toml_top "$manifest" version)"
  api="$(toml_top "$manifest" api)"
  pkgid="$(cargo +"$TOOLCHAIN" pkgid --manifest-path "$PLUGINS/Cargo.toml" -p "$package")"
  crate_version="${pkgid##*@}"
  crate_version="${crate_version##*#}"
  if [ -z "$version" ] || [ "$version" != "$crate_version" ]; then
    die "$crate: plugin.toml version \"$version\" is not the crate's $crate_version"
  fi
  [ "$api" = "$API" ] || die "$crate: plugin.toml api \"$api\" is not the WIT's $API"
done

cargo +"$TOOLCHAIN" build --manifest-path "$PLUGINS/Cargo.toml" --target "$TARGET" --release

OUT="$PLUGINS/target/$TARGET/release"
for manifest in "$PLUGINS"/*/plugin.toml; do
  dir="$(dirname "$manifest")"
  crate="$(basename "$dir")"
  id="$(toml_top "$manifest" id)"
  lib="$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$dir/Cargo.toml" | tr - _)"
  if [ -z "$id" ] || [ -z "$lib" ] || [ ! -f "$OUT/$lib.wasm" ]; then
    die "$crate: no id, crate name, or $lib.wasm"
  fi
  mkdir -p "$STAGE/dist-test/$id"
  cp "$OUT/$lib.wasm" "$STAGE/dist-test/$id/plugin.wasm"
  cp "$manifest" "$STAGE/dist-test/$id/plugin.toml"
  say "$id -> plugins/dist-test/$id ($(size "$OUT/$lib.wasm") bytes)"
done

# The fixture's "next release", for the daemon's update tests: the same
# crate built with its `v2` feature (one more tool) beside its v2 manifest.
# Never embedded as a plugin of its own — the tests serve it from a fake
# releases server.
cargo +"$TOOLCHAIN" build --manifest-path "$PLUGINS/Cargo.toml" --target "$TARGET" --release \
  -p chimaera-plugin-test-fixture --features v2
mkdir -p "$STAGE/dist-test/test-fixture-v2"
cp "$OUT/chimaera_plugin_test_fixture.wasm" "$STAGE/dist-test/test-fixture-v2/plugin.wasm"
cp "$PLUGINS/test-fixture/plugin-v2.toml" "$STAGE/dist-test/test-fixture-v2/plugin.toml"
say "test-fixture 0.2.0 -> plugins/dist-test/test-fixture-v2 ($(size "$OUT/chimaera_plugin_test_fixture.wasm") bytes)"

rm -rf "$PLUGINS/dist" "$PLUGINS/dist-test"
mv "$STAGE/dist" "$PLUGINS/dist"
mv "$STAGE/dist-test" "$PLUGINS/dist-test"

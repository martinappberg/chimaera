#!/usr/bin/env bash
#
# Lay out what the daemon's TESTS need in plugins/dist-test (gitignored).
# Nothing here goes into a chimaera binary: the daemon carries no plugin
# bytes (it embeds only plugins/plugins.lock) and installs plugins at
# runtime. Building or running the daemon does not need this script.
#
#   plugins/dist-test/<id>/{plugin.wasm,plugin.toml,SHA256SUMS}
#       each first-party release plugins/plugins.lock pins, downloaded from
#       https://github.com/<repo>/releases/download/v<version>/<file>, both
#       files verified against the lock's sha256s, the manifest's id,
#       version, name and [release] github checked against the lock. Tests
#       install these by path, as `chimaera plugin add --path` would. A
#       second run re-verifies what is already there and fetches nothing;
#       a run without network needs them there already.
#   plugins/dist-test/test-fixture/{plugin.wasm,plugin.toml}
#   plugins/dist-test/test-fixture-v2/{plugin.wasm,plugin.toml}
#       plugins/test-fixture (a 0.1 plugin, on the frozen plugins/api-0.1)
#       and its `v2` build (the "next release" the update tests serve).
#   plugins/dist-test/test-platform/{plugin.wasm,plugin.toml}
#       plugins/test-platform, the 0.2 platform fixture.
#   plugins/dist-test/test-privileged/{plugin.wasm,plugin.toml}
#       plugins/test-privileged, the programs-and-tools fixture (its tool's
#       archive stays in its crate; the tests serve it from a fake host).
#
# Run before `cargo test` / `cargo clippy --all-targets` of chimaera-server
# (its test build embeds plugins/dist-test; CI and `just check` do).
# Works with sha256sum (Linux) or shasum (macOS), and bash 3.2.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PLUGINS="$ROOT/plugins"
LOCK="$PLUGINS/plugins.lock"
DIST="$PLUGINS/dist-test"
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

# The sha256 a SHA256SUMS file lists for `name` (empty when it lists none).
listed() { # sums-file name
  awk -v f="$2" '{ n = $2; sub(/^\*/, "", n); sub(/^\.\//, "", n); if (n == f) { print tolower($1); exit } }' "$1"; }

# A locked release into `stage/<id>`: plugin.wasm, plugin.toml and
# SHA256SUMS, taken from the last run's plugins/dist-test/<id>/ when they
# still match the lock, else downloaded; then verified and checked.
lay_out_locked() { # id name version repo sha256_wasm sha256_toml stage
  local id="$1" name="$2" version="$3" repo="$4" stage="$7/$1"
  local base="https://github.com/$repo/releases/download/v$version"
  local file want got fetched=0
  mkdir -p "$stage"
  for file in plugin.wasm plugin.toml SHA256SUMS; do
    case "$file" in
      plugin.wasm) want="$5" ;;
      plugin.toml) want="$6" ;;
      *) want="" ;;
    esac
    if [ -f "$DIST/$id/$file" ]; then
      if [ -z "$want" ]; then
        # The lock doesn't pin SHA256SUMS itself: it must list the pinned
        # sha256 of both files.
        if [ "$(listed "$DIST/$id/$file" plugin.wasm)" = "$5" ] &&
          [ "$(listed "$DIST/$id/$file" plugin.toml)" = "$6" ]; then
          cp "$DIST/$id/$file" "$stage/$file"
          continue
        fi
      elif [ "$(sha256 "$DIST/$id/$file")" = "$want" ]; then
        cp "$DIST/$id/$file" "$stage/$file"
        continue
      fi
    fi
    say "$id $version: downloading $base/$file"
    if ! fetch "$base/$file" "$stage/$file"; then
      rm -f "$stage/$file"
      die "$id $version: could not download $base/$file (a run without network needs $(rel "$DIST")/$id/ from an earlier run)"
    fi
    fetched=$((fetched + 1))
    if [ -n "$want" ]; then
      got="$(sha256 "$stage/$file")"
      [ "$got" = "$want" ] ||
        die "$id $version: $file from $base does not match plugins/plugins.lock: expected sha256 $want, got $got"
    fi
  done
  [ "$(listed "$stage/SHA256SUMS" plugin.wasm)" = "$5" ] && [ "$(listed "$stage/SHA256SUMS" plugin.toml)" = "$6" ] ||
    die "$id $version: the release's SHA256SUMS does not list the sha256s plugins/plugins.lock pins"
  local mid mversion mname mrepo
  mid="$(toml_top "$stage/plugin.toml" id)"
  mversion="$(toml_top "$stage/plugin.toml" version)"
  mname="$(toml_top "$stage/plugin.toml" name)"
  mrepo="$(toml_release "$stage/plugin.toml")"
  [ "$mid" = "$id" ] || die "$id $version: the release's plugin.toml is for id \"$mid\", not \"$id\""
  [ "$mversion" = "$version" ] ||
    die "$id $version: the release's plugin.toml says version \"$mversion\", plugins/plugins.lock says $version"
  [ "$mname" = "$name" ] ||
    die "$id $version: the release's plugin.toml names it \"$mname\", plugins/plugins.lock \"$name\""
  [ "$mrepo" = "$repo" ] ||
    die "$id $version: the release's plugin.toml names [release] github = \"$mrepo\", plugins/plugins.lock says $repo"
  if [ "$fetched" = 0 ]; then
    say "$id $version -> $(rel "$DIST")/$id ($(size "$stage/plugin.wasm") bytes; already there, re-verified against the lock)"
  else
    say "$id $version -> $(rel "$DIST")/$id ($(size "$stage/plugin.wasm") bytes; downloaded from $repo, verified against the lock)"
  fi
}

# Staged, then swapped in at the end: a failed run keeps the last good layout.
STAGE="$PLUGINS/.dist-test.stage"
rm -rf "$STAGE"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE"

# Build outputs of this script before the daemon stopped embedding plugins.
for old in "$PLUGINS/dist" "$PLUGINS/cache"; do
  if [ -d "$old" ]; then
    say "removing $(rel "$old") (the daemon no longer embeds plugins)"
    rm -rf "$old"
  fi
done

[ -f "$LOCK" ] || die "plugins/plugins.lock is missing"
LOCKED="$(read_tables "$LOCK" id name summary version repo sha256_wasm sha256_toml tier caps)"

seen=" "
while IFS="$SEP" read -r id name _summary version repo sha_wasm sha_toml _tier _caps <&3; do
  [ -n "$id" ] || continue
  [[ $id =~ $ID_RE ]] || die "plugins/plugins.lock: id \"$id\" is not lowercase letters, digits and dashes"
  [[ $version =~ $VERSION_RE ]] || die "plugins/plugins.lock: $id: version \"$version\" is not MAJOR.MINOR.PATCH"
  [[ $repo =~ $REPO_RE ]] || die "plugins/plugins.lock: $id: repo \"$repo\" is not owner/name"
  [[ $sha_wasm =~ $SHA_RE ]] || die "plugins/plugins.lock: $id: sha256_wasm is not 64 lowercase hex digits"
  [[ $sha_toml =~ $SHA_RE ]] || die "plugins/plugins.lock: $id: sha256_toml is not 64 lowercase hex digits"
  case "$id" in test-*) die "plugins/plugins.lock: $id: test-* ids are the fixture's" ;; esac
  case "$seen" in *" $id "*) die "plugins/plugins.lock names $id twice" ;; esac
  seen="$seen$id "
  lay_out_locked "$id" "$name" "$version" "$repo" "$sha_wasm" "$sha_toml" "$STAGE"
done 3<<<"$LOCKED"

# The test fixtures. A manifest is the one source of truth for its version
# and API, so it must agree with the crate and the WIT rather than have them
# injected: `version` equals the crate's (resolved) package version, and
# `api` is the MAJOR.MINOR of a WIT package the host serves — the current
# one (wit/, what chimaera-plugin-api publishes) or a frozen one it still
# serves beside it (wit-0.1/, what plugins/api-0.1 publishes).
APIS=" "
for WIT in "$ROOT"/crates/chimaera-plugin-api/wit/chimaera.wit "$ROOT"/crates/chimaera-plugin-api/wit-*/chimaera.wit; do
  [ -f "$WIT" ] || continue
  one="$(sed -n 's/^package chimaera:plugin@\([0-9]*\.[0-9]*\)\..*/\1/p' "$WIT")"
  [ -n "$one" ] || die "no package version in $(rel "$WIT")"
  APIS="$APIS$one "
done
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
  case "$APIS" in
    *" $api "*) ;;
    *) die "$crate: plugin.toml api \"$api\" is not a WIT version the host serves:$APIS" ;;
  esac
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
  mkdir -p "$STAGE/$id"
  cp "$OUT/$lib.wasm" "$STAGE/$id/plugin.wasm"
  cp "$manifest" "$STAGE/$id/plugin.toml"
  say "$id -> plugins/dist-test/$id ($(size "$OUT/$lib.wasm") bytes)"
done

# The fixture's "next release", for the daemon's update tests: the same
# crate built with its `v2` feature (one more tool) beside its v2 manifest.
# Never embedded as a plugin of its own — the tests serve it from a fake
# releases server.
cargo +"$TOOLCHAIN" build --manifest-path "$PLUGINS/Cargo.toml" --target "$TARGET" --release \
  -p chimaera-plugin-test-fixture --features v2
mkdir -p "$STAGE/test-fixture-v2"
cp "$OUT/chimaera_plugin_test_fixture.wasm" "$STAGE/test-fixture-v2/plugin.wasm"
cp "$PLUGINS/test-fixture/plugin-v2.toml" "$STAGE/test-fixture-v2/plugin.toml"
say "test-fixture 0.2.0 -> plugins/dist-test/test-fixture-v2 ($(size "$OUT/chimaera_plugin_test_fixture.wasm") bytes)"

rm -rf "$DIST"
mv "$STAGE" "$DIST"

#!/usr/bin/env bash
# A/B one scroll scenario across two built UIs on the same (debug) daemon,
# which serves web-ui/dist from disk: each build is copied into place in
# turn, then restored to BUILD_B. See README.md.
#
#   URL='http://127.0.0.1:<port>/?scrollSession=<name>#token=<token>' \
#   BUILD_A=/path/to/old-dist BUILD_B=/path/to/new-dist \
#   RUNNER=wk|cdp REPORT=page/jumps.js RUNS=2 \
#   bash scripts/perf/transcript-scroll/ab.sh '<scenario>'
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIST="$(git -C "$HERE" rev-parse --show-toplevel)/web-ui/dist"
SCENARIO="$1"
RUNNER="${RUNNER:-wk}"
REPORT="${REPORT:-page/jumps.js}"

# The WebKit driver compiles outside the repo, rebuilt when its source changes.
WK="${TMPDIR:-/tmp}/chimaera-wkscroll"
if [ "$RUNNER" = wk ] && { [ ! -x "$WK" ] || [ "$HERE/wkscroll.swift" -nt "$WK" ]; }; then
  swiftc -O "$HERE/wkscroll.swift" -o "$WK"
fi

for label in A B; do
  build_var="BUILD_$label"
  rm -rf "$DIST" && cp -R "${!build_var}" "$DIST"
  for i in $(seq 1 "${RUNS:-2}"); do
    printf '%s #%s ' "$label" "$i"
    if [ "$RUNNER" = wk ]; then
      # An unbundled WebKit host logs sandbox-cache noise on stderr; drop only that.
      "$WK" "$URL" "$HERE/page/setup.js" "$HERE/$REPORT" "$SCENARIO" \
        2> >(grep -v "sandbox extension" >&2)
    else
      node "$HERE/cdp-scroll.mjs" "$URL" "$HERE/page/setup.js" "$HERE/$REPORT" "$SCENARIO"
    fi
  done
done
rm -rf "$DIST" && cp -R "$BUILD_B" "$DIST"

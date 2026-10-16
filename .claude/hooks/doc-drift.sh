#!/usr/bin/env bash
# Stop: WARN (never block) when code in an area or feature changed in the working tree
# but its map/page wasn't touched. A nudge to keep docs honest — it emits a
# systemMessage and always exits 0. Pairs with the "verify, then trust, then update"
# loop in AGENTS.md.
#
# The area→map and feature→page lists live in docs/features/registry.json (globs; see
# scripts/check-feature-registry.mjs, which validates it). Override the file with
# DOC_DRIFT_REGISTRY (tests). A missing or unparsable registry is silent: a hook
# must never block.
set -u
command -v jq >/dev/null 2>&1 || exit 0
root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$root" 2>/dev/null || exit 0
reg="${DOC_DRIFT_REGISTRY:-docs/features/registry.json}"
[ -r "$reg" ] || exit 0

# Working-tree changes (staged + unstaged), path column only.
changed=$(git status --porcelain 2>/dev/null | sed 's/^...//; s/.* -> //')
[ -n "$changed" ] || exit 0

# Registry glob -> anchored ERE. `**/` -> (.*/)?, trailing `/**` -> (/.*)?, bare `**`
# -> .*, `*` -> [^/]*, `?` -> [^/]; everything else literal, `[...]` passes through.
# Placeholders keep the `*`/`?` rewrites from re-reading the operators we insert.
g2re() {
  printf '%s' "$1" | sed -e 's/[.+(){}|^$\\]/\\&/g' \
    -e 's#\*\*/#@A@#g' -e 's#/\*\*$#@B@#' -e 's#\*\*#@C@#g' \
    -e 's#\*#[^/]*#g' -e 's#?#[^/]#g' \
    -e 's#@A@#(.*/)?#g' -e 's#@B@#(/.*)?#' -e 's#@C@#.*#g'
}

rows=$(jq -r '(.areas[]? | .paths[]? as $p | [.id, $p, .map]),
              (.features[]? | .paths[]? as $p | ["feature: " + .name, $p, .page])
              | @tsv' "$reg" 2>/dev/null) || exit 0

warn=""
while IFS=$'\t' read -r label glob doc; do
  [ -n "$glob" ] && [ -n "$doc" ] || continue
  if printf '%s\n' "$changed" | grep -qE "^($(g2re "$glob"))\$" \
     && ! printf '%s\n' "$changed" | grep -qxF "$doc"; then
    warn="${warn}
- ${label}: code under ${glob} changed, but ${doc} wasn't updated."
  fi
done <<EOF2
$rows
EOF2

[ -n "$warn" ] || exit 0
msg="Doc-drift check (warning — not blocking):${warn}
If the change is user- or contract-visible, update the area's AGENTS.md/DESIGN or the named
docs/features/ page (see the document-feature skill); otherwise it's fine to skip."
jq -cn --arg m "$msg" '{systemMessage:$m}'
exit 0

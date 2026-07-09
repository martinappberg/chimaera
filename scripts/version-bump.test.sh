#!/usr/bin/env bash
# Characterization matrix for version-bump.sh — the release semver decision is the
# most behavior-critical, most-often-changed piece of the release pipeline, so it
# is pinned here and run in CI. Run: bash scripts/version-bump.test.sh
set -u
here=$(cd "$(dirname "$0")" && pwd)
S="$here/version-bump.sh"
pass=0; fail=0
t() { # <latest> <subject> <expected>
  local got; got=$(bash "$S" "$1" "$2")
  if [ "$got" = "$3" ]; then pass=$((pass + 1))
  else fail=$((fail + 1)); printf 'FAIL  latest=%-8s subject=%-40s exp=%s got=%s\n' "$1" "$2" "$3" "$got"; fi
}

# First release (no tag) -> 0.1.0 for anything that releases.
t ""       "feat: first"                 "0.1.0"
t ""       "fix: first"                  "0.1.0"
t ""       "feat!: first breaking"       "0.1.0"

# feat -> minor ; fix/perf/revert -> patch ; ! -> major.
t "v0.3.2" "feat: new capability"        "0.4.0"
t "v0.3.2" "feat(ui): scoped feature"    "0.4.0"
t "v0.3.2" "fix: a bug"                   "0.3.3"
t "v0.3.2" "fix(server): scoped fix"     "0.3.3"
t "v0.3.2" "perf: faster preview"        "0.3.3"
t "v0.3.2" "revert: bad change"          "0.3.3"
t "v0.3.2" "feat!: breaking feature"     "1.0.0"
t "v0.3.2" "fix!: breaking fix"          "1.0.0"
t "v1.4.5" "feat: x"                      "1.5.0"
t "v0.10.9" "fix: multi-digit"           "0.10.10"

# refactor/chore/docs/test/ci/build/style -> NO release.
t "v0.3.2" "refactor: split lib.rs"      "skip"
t "v0.3.2" "chore: bump deps"            "skip"
t "v0.3.2" "docs: update readme"         "skip"
t "v0.3.2" "test: add coverage"          "skip"
t "v0.3.2" "ci: tweak workflow"          "skip"
t "v0.3.2" "build: adjust"               "skip"
t "v0.3.2" "style: reformat"             "skip"
t "v0.3.2" "refactor(pty): move"         "skip"

# [skip release] wins even over feat.
t "v0.3.2" "feat: new thing [skip release]" "skip"
t "v0.3.2" "docs: x [skip release]"      "skip"

# Case-insensitive type; unknown/no-prefix -> patch (never a silent skip).
t "v0.3.2" "FIX: uppercase"              "0.3.3"
t "v0.3.2" "Feat: capitalized"           "0.4.0"
t "v0.3.2" "random subject no prefix"    "0.3.3"
t "v0.3.2" "Merge branch main"           "0.3.3"

# The whole point: a NOISY body can't reach us — only the subject is passed. A
# feat subject stays minor even if the (unseen) body mentions a breaking change.
t "v0.3.2" "feat: mentions git reset --hard safely" "0.4.0"

echo "version-bump.test.sh: pass=$pass fail=$fail"
[ "$fail" -eq 0 ]

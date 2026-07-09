#!/usr/bin/env bash
# Stop: WARN (never block) when code in an area changed in the working tree but the
# area's map/design doc wasn't touched. A nudge to keep docs honest — it emits a
# systemMessage and always exits 0. Pairs with the "verify, then trust, then update"
# loop in CLAUDE.md.
set -u
command -v jq >/dev/null 2>&1 || exit 0
root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$root" 2>/dev/null || exit 0

# Working-tree changes (staged + unstaged), path column only.
changed=$(git status --porcelain 2>/dev/null | sed 's/^...//; s/.* -> //')
[ -n "$changed" ] || exit 0

warn=""
check() { # $1 = code path prefix, $2 = the doc that should move with it, $3 = label
  if printf '%s\n' "$changed" | grep -qE "^$1" \
     && ! printf '%s\n' "$changed" | grep -qxF "$2"; then
    warn="${warn}
- ${3}: code under ${1} changed, but ${2} wasn't updated."
  fi
}
check 'crates/chimaera-server/src/'   'crates/chimaera-server/CLAUDE.md' 'chimaera-server'
check 'crates/chimaera-agent/src/'    'crates/chimaera-agent/CLAUDE.md'  'chimaera-agent'
check 'crates/chimaera-pty/src/'      'crates/chimaera-pty/CLAUDE.md'    'chimaera-pty'
check 'crates/chimaera-app/src/'      'crates/chimaera-app/CLAUDE.md'    'chimaera-app'
check 'web-ui/src/lib/chat/'          'web-ui/src/lib/chat/CLAUDE.md'     'chat UI'

[ -n "$warn" ] || exit 0
msg="Doc-drift check (warning — not blocking):${warn}
If the change is user- or contract-visible, update the area's CLAUDE.md/DESIGN; otherwise it's fine to skip."
jq -cn --arg m "$msg" '{systemMessage:$m}'
exit 0

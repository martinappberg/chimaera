#!/usr/bin/env bash
# Stop: WARN (never block) when code in an area changed in the working tree but the
# area's map/design doc wasn't touched. A nudge to keep docs honest — it emits a
# systemMessage and always exits 0. Pairs with the "verify, then trust, then update"
# loop in AGENTS.md.
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

# Area maps — the nested AGENTS.md that describes a crate/UI area's structure.
check 'crates/chimaera-server/src/'   'crates/chimaera-server/AGENTS.md' 'chimaera-server'
check 'crates/chimaera-agent/src/'    'crates/chimaera-agent/AGENTS.md'  'chimaera-agent'
check 'crates/chimaera-pty/src/'      'crates/chimaera-pty/AGENTS.md'    'chimaera-pty'
check 'crates/chimaera-app/src/'      'crates/chimaera-app/AGENTS.md'    'chimaera-app'
check 'web-ui/src/lib/chat/'          'web-ui/src/lib/chat/AGENTS.md'     'chat UI'

# Feature pages — docs/features/<page> describes what a capability DOES. Prefixes are
# a feature's entry points; a change there usually wants its page updated too. Warn-only.
check 'web-ui/src/lib/layout/'        'docs/features/workbench.md'                 'feature: workbench'
check 'crates/chimaera-pty/src/'      'docs/features/terminals.md'                 'feature: terminals'
check 'web-ui/src/lib/terminal/'      'docs/features/terminals.md'                 'feature: terminals'
check 'crates/chimaera-agent/src/'    'docs/features/chat-mode.md'                 'feature: chat mode'
check 'web-ui/src/lib/chat/'          'docs/features/chat-mode.md'                 'feature: chat mode'
check 'web-ui/src/lib/previews/'      'docs/features/files-and-previews.md'        'feature: files & previews'
check 'crates/chimaera-server/src/fs.rs' 'docs/features/files-and-previews.md'     'feature: files & previews'
check 'crates/chimaera-server/src/git/'  'docs/features/git.md'                    'feature: git'
check 'crates/chimaera-server/src/mcp.rs'   'docs/features/linked-terminals.md'    'feature: linked terminals'
check 'crates/chimaera-server/src/links.rs' 'docs/features/linked-terminals.md'    'feature: linked terminals'
check 'crates/chimaera-server/src/launcher.rs' 'docs/features/agents.md'           'feature: agents'
check 'crates/chimaera-server/src/runtimes.rs' 'docs/features/agents.md'           'feature: agents'
check 'crates/chimaera-remote/src/'   'docs/features/remote-connect.md'            'feature: remote connect'
check 'crates/chimaera-app/src/'      'docs/features/native-app.md'                'feature: native app'
check 'crates/chimaera-server/src/ledger.rs' 'docs/features/lifecycle-and-persistence.md' 'feature: lifecycle'
check 'web-ui/src/lib/settings/'      'docs/features/settings.md'                  'feature: settings'
check 'crates/chimaera/src/'          'docs/features/cli.md'                       'feature: CLI'

[ -n "$warn" ] || exit 0
msg="Doc-drift check (warning — not blocking):${warn}
If the change is user- or contract-visible, update the area's AGENTS.md/DESIGN or the named
docs/features/ page (see the document-feature skill); otherwise it's fine to skip."
jq -cn --arg m "$msg" '{systemMessage:$m}'
exit 0

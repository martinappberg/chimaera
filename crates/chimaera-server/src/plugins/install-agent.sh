#!/bin/bash
# Invoked with positional arguments by install_requirement. Keep all plugin
# metadata out of shell source; this also makes the exact program testable.
plugin_name=$1
agent_name=$2
agent_bin=$3
marketplace=$4
plugin_id=$5
install_verb=$6
completion_file=$7
# The directory of the git chimaera resolved (the Git binary path setting, or
# the login shell's), empty when it found none new enough. First on PATH so
# the agent's marketplace clone uses it, not an old system git.
git_dir=${8:-}
if [ -n "$git_dir" ]; then
    PATH="$git_dir:$PATH"
    export PATH
fi

finish() {
    install_status=$?
    trap - EXIT
    printf '\n'
    if [ "$install_status" -eq 0 ]; then
        printf '%s\n' 'Installed. Return to Extensions to review hooks and set up this workspace.'
        printf '%s\n' 'Start a new agent session to load the installed plugin.'
    else
        printf 'Install failed (exit %s). Review the output above, then retry from Extensions.\n' "$install_status"
    fi
    # Ordinary PTYs disappear on exit. Keep this operation's result visible
    # until acknowledged, including failures that finish before UI attachment.
    if [ -t 0 ]; then
        printf '\nPress Enter to close this terminal.'
    fi
    # The title is presentation only. Probe invalidation uses a private file,
    # because plugin names and CLI output can contain arbitrary OSC sequences.
    printf '\033]2;Plugin installation finished\007'
    printf '1' > "$completion_file"
    if [ -t 0 ]; then
        IFS= read -r acknowledgement
    fi
    exit "$install_status"
}
trap finish EXIT

printf 'Installing %s for %s with the agent plugin manager.\n\n' "$plugin_name" "$agent_name"
explain_claude_git_failure() {
    # Check only after a failed fetch: local or already-cached marketplaces
    # can install successfully without modern Git. Codex has its own fetcher.
    [ "$agent_name" = claude ] || return 0
    if ! command -v git >/dev/null 2>&1; then
        printf '%s\n' 'Claude needs Git to fetch this marketplace, but git is not on PATH.'
        printf '%s\n' 'Set Settings > Git binary path (or add your Git setup command in Settings > Environment), then retry.'
        return 0
    fi
    git_help=$(LC_ALL=C git clone -h 2>&1)
    case "$git_help" in
        *shallow-submodules*) ;;
        *)
            printf 'Claude needs a Git that supports --shallow-submodules.\nFound: %s (%s).\n' "$(git --version 2>&1)" "$(command -v git)"
            printf '%s\n' 'Set Settings > Git binary path to a newer git (or load one in Settings > Environment), then retry.'
            ;;
    esac
}
printf '$ %s plugin marketplace add %s\n' "$agent_name" "$marketplace"
if ! "$agent_bin" plugin marketplace add "$marketplace"; then
    explain_claude_git_failure
    printf '%s\n' 'Marketplace registration failed; trying the existing local marketplace, if any.'
fi
printf '\n$ %s plugin %s %s\n' "$agent_name" "$install_verb" "$plugin_id"
"$agent_bin" plugin "$install_verb" "$plugin_id"
exit $?

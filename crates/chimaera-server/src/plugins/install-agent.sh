#!/bin/bash
# Invoked with positional arguments by install_requirement. Keep all plugin
# metadata out of shell source; this also makes the exact program testable.
plugin_name=$1
agent_name=$2
agent_bin=$3
marketplace=$4
plugin_id=$5
install_verb=$6
finished_title=$7

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
    # The daemon can refresh plugin probes before this terminal is closed.
    printf '\033]2;%s\007' "$finished_title"
    # Ordinary PTYs disappear on exit. Keep this operation's result visible
    # until acknowledged, including failures that finish before UI attachment.
    if [ -t 0 ]; then
        printf '\nPress Enter to close this terminal.'
        IFS= read -r acknowledgement
    fi
    exit "$install_status"
}
trap finish EXIT

printf 'Installing %s for %s with the agent plugin manager.\n\n' "$plugin_name" "$agent_name"
printf '$ %s plugin marketplace add %s\n' "$agent_name" "$marketplace"
"$agent_bin" plugin marketplace add "$marketplace" || printf '%s\n' '(marketplace already added or unavailable — trying the install)'
printf '\n$ %s plugin %s %s\n' "$agent_name" "$install_verb" "$plugin_id"
"$agent_bin" plugin "$install_verb" "$plugin_id"
exit $?

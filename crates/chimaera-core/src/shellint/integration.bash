# Chimaera shell integration for bash (3.2+): emits OSC 133 semantic-prompt
# marks (A=prompt, C=output start, D;exit=done), OSC 633;E command-line
# reports, and OSC 7 cwd reports, so the chimaera daemon can keep a
# per-session command journal and know when this shell is at its prompt.
#
# Safe to source more than once; chains any existing DEBUG trap and
# PROMPT_COMMAND instead of clobbering them; indexed prompt-command arrays
# retain every original entry in order, with our arm always last.

if [ -n "${CHIMAERA_INTEGRATION:-}" ]; then
    return 0
fi
CHIMAERA_INTEGRATION=1

__chimaera_in_command=0
__chimaera_armed=0

__chimaera_escape() {
    local s=${1//\\/\\\\}
    s=${s//;/\\x3b}
    printf '%s' "$s"
}

__chimaera_urlencode() {
    local s="$1" out='' c i
    for (( i = 0; i < ${#s}; i++ )); do
        c=${s:$i:1}
        case "$c" in
            [a-zA-Z0-9/._~-]) out+="$c" ;;
            *) out+=$(printf '%%%02X' "'$c") ;;
        esac
    done
    printf '%s' "$out"
}

# DEBUG fires for every simple command; only the first one after the prompt
# was re-armed is the command line the user (or a linked agent) submitted.
__chimaera_preexec() {
    [ "$__chimaera_armed" = 1 ] || return 0
    [ -n "${COMP_LINE:-}" ] && return 0
    __chimaera_armed=0
    __chimaera_in_command=1
    local cmd
    cmd=$(HISTTIMEFORMAT='' builtin history 1 2>/dev/null | sed 's/^ *[0-9]* *//')
    printf '\033]633;E;%s\007' "$(__chimaera_escape "$cmd")"
    printf '\033]133;C\007'
    return 0
}

__chimaera_precmd() {
    local __chimaera_status=$?
    if [ "$__chimaera_in_command" = 1 ]; then
        printf '\033]133;D;%s\007' "$__chimaera_status"
        __chimaera_in_command=0
    fi
    printf '\033]7;file://%s%s\007' "${HOSTNAME:-}" "$(__chimaera_urlencode "$PWD")"
    printf '\033]133;A\007'
    return $__chimaera_status
}

# Arming happens as the LAST prompt-command step, so DEBUG traps fired by
# other PROMPT_COMMAND components never look like user commands.
__chimaera_arm() {
    __chimaera_armed=1
}

# On bash < 4.4 this capture can come back empty when the pre-existing
# trap's handler calls functions (the subshell sees the trap as unset); the
# chain then degrades to ours alone. Sites that set such traps (HPC audit
# shells) keep their PROMPT_COMMAND-based logging — we preserve that below.
__chimaera_prev_debug=$(trap -p DEBUG)
if [ -n "$__chimaera_prev_debug" ]; then
    __chimaera_prev_debug=${__chimaera_prev_debug#trap -- \'}
    __chimaera_prev_debug=${__chimaera_prev_debug%\' DEBUG}
    __chimaera_prev_debug=${__chimaera_prev_debug//\'\\\'\'/\'}
    __chimaera_debug_chain="__chimaera_preexec; ${__chimaera_prev_debug}"
else
    __chimaera_debug_chain='__chimaera_preexec'
fi
unset __chimaera_prev_debug
trap "$__chimaera_debug_chain" DEBUG

# The trap must ALSO be re-armed at every prompt, inline at top level: on
# bash < 4.4, when a pre-existing DEBUG trap's handler calls shell functions
# (HPC audit shells, e.g. a site's user-audit trap), bash reverts DEBUG-trap
# changes made while an rc file is being sourced — the install above is
# silently undone by the first prompt. A bare `trap` run from the
# PROMPT_COMMAND string itself executes at top level and sticks. It also
# wins back the hook if another tool re-traps DEBUG at prompt time.
# A scalar assignment to an indexed array replaces only element zero and
# leaves later prompt commands running AFTER the arm. Their DEBUG trap would
# then report a prompt command as user output, before PS1 emits its B mark.
# Normalize all entries in execution order before unsetting the array. Every
# piece is joined with a literal newline, scalar included: a `; ` separator
# after an entry that ends in `;` is a syntax error at every prompt, and one
# after a trailing `# comment` is swallowed with the arm. Works on Bash 3.2.
__chimaera_prompt_chain='__chimaera_precmd'
if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a'* ]]; then
    for __chimaera_prompt_entry in "${PROMPT_COMMAND[@]}"; do
        __chimaera_prompt_chain+=$'\n'"$__chimaera_prompt_entry"
    done
    unset PROMPT_COMMAND __chimaera_prompt_entry
elif [ -n "${PROMPT_COMMAND:-}" ]; then
    __chimaera_prompt_chain+=$'\n'"$PROMPT_COMMAND"
fi
PROMPT_COMMAND="$__chimaera_prompt_chain"$'\n''trap "$__chimaera_debug_chain" DEBUG; __chimaera_arm'
unset __chimaera_prompt_chain
PS1="$PS1"'\[\e]133;B\a\]'

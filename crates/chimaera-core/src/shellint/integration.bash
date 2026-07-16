# Chimaera shell integration for bash (3.2+): emits OSC 133 semantic-prompt
# marks (A=prompt, C=output start, D;exit=done), OSC 633;E command-line
# reports, and OSC 7 cwd reports, so the chimaera daemon can keep a
# per-session command journal and know when this shell is at its prompt.
#
# Safe to source more than once; chains any existing DEBUG trap and
# PROMPT_COMMAND instead of clobbering them. (If your rc turns
# PROMPT_COMMAND into an array, source this afterwards.)

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
# (HPC audit shells, e.g. Sherlock's user_audit), bash reverts DEBUG-trap
# changes made while an rc file is being sourced — the install above is
# silently undone by the first prompt. A bare `trap` run from the
# PROMPT_COMMAND string itself executes at top level and sticks. It also
# wins back the hook if another tool re-traps DEBUG at prompt time.
PROMPT_COMMAND="__chimaera_precmd${PROMPT_COMMAND:+; ${PROMPT_COMMAND}}; "'trap "$__chimaera_debug_chain" DEBUG; __chimaera_arm'
PS1="$PS1"'\[\e]133;B\a\]'

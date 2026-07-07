# Chimaera shell integration for zsh: emits OSC 133 semantic-prompt marks
# (A=prompt, C=output start, D;exit=done), OSC 633;E command-line reports,
# and OSC 7 cwd reports, so the chimaera daemon can keep a per-session
# command journal and know when this shell is at its prompt.
#
# Safe to source more than once; registers via add-zsh-hook so existing
# precmd/preexec hooks (themes, starship, p10k) keep working.

if [ -n "${CHIMAERA_INTEGRATION:-}" ]; then
    return 0
fi
CHIMAERA_INTEGRATION=1

autoload -Uz add-zsh-hook

typeset -g __chimaera_in_command=0

__chimaera_urlencode() {
    emulate -L zsh
    local s=$1 out='' c
    for c in ${(s::)s}; do
        if [[ $c == [a-zA-Z0-9/._~-] ]]; then
            out+=$c
        else
            out+=$(printf '%%%02X' "'$c")
        fi
    done
    print -rn -- $out
}

__chimaera_preexec() {
    local cmd=${1//\\/\\\\}
    cmd=${cmd//;/\\x3b}
    printf '\033]633;E;%s\007' "$cmd"
    printf '\033]133;C\007'
    __chimaera_in_command=1
}

__chimaera_precmd() {
    local st=$?
    if (( __chimaera_in_command )); then
        printf '\033]133;D;%s\007' $st
        __chimaera_in_command=0
    fi
    printf '\033]7;file://%s%s\007' "${HOST:-}" "$(__chimaera_urlencode $PWD)"
    printf '\033]133;A\007'
}

add-zsh-hook precmd __chimaera_precmd
add-zsh-hook preexec __chimaera_preexec
PS1="${PS1}%{$(printf '\033]133;B\007')%}"

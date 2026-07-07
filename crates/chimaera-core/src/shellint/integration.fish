# Chimaera shell integration for fish: emits OSC 133 semantic-prompt marks,
# OSC 633;E command-line reports, and OSC 7 cwd reports, so the chimaera
# daemon can keep a per-session command journal and know when this shell is
# at its prompt.

if status is-interactive; and not set -q CHIMAERA_INTEGRATION
    set -g CHIMAERA_INTEGRATION 1

    function __chimaera_preexec --on-event fish_preexec
        set -l cmd (string replace -a -- '\\' '\\\\' $argv[1] | string replace -a -- ';' '\\x3b')
        printf '\033]633;E;%s\007' "$cmd"
        printf '\033]133;C\007'
    end

    function __chimaera_postexec --on-event fish_postexec
        printf '\033]133;D;%s\007' $status
    end

    function __chimaera_prompt --on-event fish_prompt
        printf '\033]7;file://%s%s\007' (hostname) (string escape --style=url -- $PWD | string replace -a '%2F' '/')
        printf '\033]133;A\007'
    end
end

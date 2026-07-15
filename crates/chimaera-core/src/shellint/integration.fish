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

    # Environment prelude: CHIMAERA_PRELUDE points at a POSIX script of
    # user-configured startup commands (module load, conda activate, …).
    # fish can't source POSIX, so run it in bash and import the resulting
    # environment — env vars transfer, bash functions/aliases don't (the
    # documented tradeoff; every module/conda-style tool works by mutating
    # env). Deferred to the first prompt because vendor_conf.d loads BEFORE
    # the user's config.fish, and the prelude must run after their rc; the
    # handler erases itself so it runs exactly once.
    if set -q CHIMAERA_PRELUDE; and not set -q CHIMAERA_PRELUDE_DONE; and test -r "$CHIMAERA_PRELUDE"
        function __chimaera_prelude --on-event fish_prompt
            functions -e __chimaera_prelude
            set -gx CHIMAERA_PRELUDE_DONE 1
            command -sq bash; or return
            # Prelude stdout goes to stderr so it still shows in the
            # terminal without corrupting the NUL-delimited env capture.
            bash -c 'source "$CHIMAERA_PRELUDE" 1>&2; command env -0' | while read -lz __chim_kv
                set -l __chim_pair (string split -m 1 = -- $__chim_kv)
                test (count $__chim_pair) -eq 2; or continue
                set -l __chim_k $__chim_pair[1]
                # Only names fish can hold (drops BASH_FUNC_x%% exports and
                # other exotica), minus vars fish owns / bash-run artifacts.
                string match -qr '^[A-Za-z_][A-Za-z0-9_]*$' -- $__chim_k; or continue
                string match -qr '^(_|SHLVL|PWD|OLDPWD|SHELL|IFS|PS1|BASH.*)$' -- $__chim_k; and continue
                if test "$__chim_k" = PATH
                    set -gx PATH (string split : -- $__chim_pair[2])
                else
                    set -gx $__chim_k $__chim_pair[2]
                end
            end
        end
    end
end

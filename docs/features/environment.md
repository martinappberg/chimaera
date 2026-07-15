# Environment preludes

User-configured startup commands (`module load bcftools`, `micromamba activate env`,
`export FOO=bar`) that run **once per new session** — after the user's own rc files, before
the shell prompt or agent takes over. The daemon never parses the text: it is opaque POSIX
shell run in the user's own login shell, which is why every env tool (lmod, conda, spack,
venv, nix) works with zero tool-specific code. Slice 1 of the M5 HPC layer's *environment
axis* — deep design in the
[architecture guide](../agent-guides/architecture.md#environment-prelude-compute-node-sessions).

**Where it lives (shared):** daemon `crates/chimaera-server/src/environment.rs` (store +
route + per-session materialization) and `crates/chimaera-core/src/shellint.rs`
(`PRELUDE_SNIPPET_POSIX` + the rc/fish hooks). UI
`web-ui/src/lib/settings/EnvironmentSettings.svelte` + `environment.ts`. Wire:
`GET/PUT /api/v1/environment` (bearer-authed) and the optional `prelude` field on
`POST /api/v1/sessions`.

## The prelude model

- **What & when.** Three scopes that **concatenate** (never override), in order: **host**
  ("always `ml bcftools` on this machine") ⊕ **workspace** ("this project also
  `conda activate hello`") ⊕ **launch** (one-off text on a single session create).
  Commands run in sequence, which is the HPC mental model; env vars merge last-wins.
- **How it's used.** Settings → **Environment**: a multiline editor for this machine's
  prelude and one for the active workspace's, explicit Save (no autosave), whole-map
  fetch-merge-put so one window's save can't clobber another workspace's entry. The launch
  scope rides `POST /api/v1/sessions` `{"prelude": "..."}` (wire only in slice 1 — no
  launcher UI yet).
- **Where it lives.** Storage: `~/.config/chimaera/env-profiles.json` (`EnvPreludeStore` —
  atomic writes, mtime re-stat so vim-over-ssh hand-edits surface without a restart, same
  never-brick-on-corrupt rule as settings). Injection: `environment::materialize_prelude`
  writes the effective text to `runtime_dir()/preludes/<session-id>.sh` and the spawn env
  carries `CHIMAERA_PRELUDE`; the shell-integration rc (bash `--init-file`, zsh ZDOTDIR
  shim) sources it after the user's rc, and the agent login-wrapper
  (`launcher::wrap_login_shell`) sources it before `exec`ing the agent. All spawn surfaces
  funnel through it: PTY shells and agent TUIs (`spawn.rs`), chat drivers + the
  degrade-to-PTY respawn (`chat.rs`), each via `api::session_env`.
- **Key behaviors / constraints.**
  - **Once per real spawn, never on reconnect** — reattach reuses the live PTY; the
    `CHIMAERA_PRELUDE_DONE` guard additionally stops nested shells from re-running it.
    A daemon started *inside* a chimaera terminal scrubs both vars from spawn env
    (`api::spawn_env_remove`) so children start with a clean slate; the remove-list is
    kept disjoint from the add-list because the two spawn layers apply them in opposite
    orders.
  - **Respawns re-materialize.** View-switch/rewind/degrade re-run the current config (the
    `ChatRecipe` carries `workspace_id` + the launch text); daemon-restart resurrection
    re-runs the durable scopes only (launch text is not in the ledger — deliberate).
  - **Caps:** 32 KB per scope (413), 256 KB whole store; NUL bytes rejected (400). Empty
    text deletes the entry. Deleting a workspace prunes its entry (explicit delete only —
    no boot sweep, so a transiently unreadable `workspaces.json` can't wipe preludes).
  - **Zero delta when unused:** no prelude → no env var, byte-identical spawns.
  - **fish:** preludes are POSIX by contract. Fish terminals import the resulting *env*
    via a one-shot bash capture on first prompt (functions/aliases don't transfer); agent
    spawns under a fish login shell exec through a bash trampoline (full semantics). No
    bash on the host → the prelude is skipped, never a broken shell. Syntax-reviewed and
    guard-tested; not yet exercised against a live fish install.
  - **Trust boundary:** a prelude is the user's own commands (same privilege as their rc),
    entering only via the bearer-authed route. Any future "read a prelude from a
    checked-in workspace file" needs an explicit confirmation gate (supply-chain vector).
  - A hanging prelude hangs session startup exactly like a hanging rc would — accepted,
    same trust model.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

_pending_

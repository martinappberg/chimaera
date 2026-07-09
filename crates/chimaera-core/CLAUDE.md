# chimaera-core — the shared foundation crate

Orientation for coding agents. The leaf crate every other crate depends on:
on-disk lifecycle records, build/version identity, per-user directory resolution,
login-shell resolution, the token/id generator, and the shell-integration scripts.
Parent map: repo-root [CLAUDE.md](../../CLAUDE.md).

## The invariant that defines this crate

**core depends on NOTHING internal, and nothing transport/UI-shaped.** No axum,
no tokio runtime, no websocket, no server/pty/agent/remote. Verified: the only
"transport" strings in here are prose in doc comments. If you're tempted to put a
thing here, it must be genuinely shared *and* free of the daemon's machinery.

**core is a path-dep of TWO workspaces** — the root daemon workspace AND the
standalone `crates/chimaera-app` (Tauri) workspace. Any change to core's
`Cargo.toml`/features must keep it building in **both** (the app pins concrete
versions). Run `cargo test` in the app workspace too when you touch core deps.

## File map

| File | What it owns |
|---|---|
| `lib.rs` | `Manifest` + `Handoff` (the daemon's on-disk lifecycle records), `VERSION`/`REPOSITORY`/`BUILD_ID` + build-match helpers (`builds_match`, `build_ref`, `parse_version`, `release_is_newer`), `data_dir`/`config_dir`/`runtime_dir` (honoring `CHIMAERA_HOME`), `login_shell` (+ pure `resolve_login_shell`), `generate_token`. |
| `shellint.rs` + `shellint/` | The shell-integration subsystem: materialize OSC 133/633/7 scripts, compose per-shell launch argv/env, and the remote-install snippet. |

## Invariants / gotchas

- **`generate_token` is the general-purpose random-hex source**, not just auth: the
  server slices it (`[..8]`/`[..16]`/`[..32]`) for session/ticket/workspace ids and
  agent keys. Don't narrow its contract to "tokens."
- **`Manifest` is atomic + 0600.** Written tmp+rename at mode 0600 because it carries
  the bearer token. `Manifest.build` serde-defaults to an ancient sentinel so an
  old manifest still parses.
- **`Handoff` is consume-once, ~120s fresh, 0600**, and is written by the **daemon
  on its own graceful shutdown** (a crash leaves none) — not by "the app/connect".
- **`CHIMAERA_HOME` moves only the daemon's bookkeeping dirs.** Spawned shells/agents
  keep the real `$HOME`, so `~/.claude` auth still works under an isolated daemon.
- Dir resolvers are **best-effort**: on `create_dir_all` failure they `warn!` and
  still return the path (callers fail on the later read/write). Don't `panic!` in a
  dir resolver.

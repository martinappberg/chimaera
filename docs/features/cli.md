# CLI — the `chimaera` binary

The `chimaera` executable is a thin clap dispatch that delegates to the sibling crates. The
same static binary is the daemon, the remote-connect client, and the operator's control surface.

**Where it lives:** `crates/chimaera/src/` (`main.rs` clap defs + dispatch, `connect.rs`,
`status.rs`, `kill.rs`, `doctor.rs`); shell-integration snippet in `chimaera-core/shellint`. Map:
[chimaera/CLAUDE.md](../../crates/chimaera/CLAUDE.md). Rules:
[rules/daemon.md](../../.claude/rules/daemon.md).

## Subcommands

| Command | Invocation | What it does |
|---|---|---|
| `serve` | `chimaera serve [--port N]` | Run the daemon in the foreground. **Load-bearing string** — `chimaera-remote` runs `…/chimaera serve` over ssh, so don't rename it. |
| `status` | `chimaera status [host]` | Local: read the `Manifest`. Remote: go through `chimaera-remote`. Prints running / stale / not-running. |
| `kill` | `chimaera kill` | Stop a running local daemon. |
| `connect` | `chimaera connect <host> [--local-port N] [--binary PATH] [--no-open] [--update-daemon]` | Stand up + tunnel to a remote daemon — see [remote-connect.md](remote-connect.md). |
| `doctor` | `chimaera doctor` | Probe write access to the data/runtime dirs and whether `ssh` / `claude` are on PATH. |
| `shell-integration` | `chimaera shell-integration` | Print the shell-integration snippet (for a remote host's rc file). |

## Key behaviors

- **Port precedence:** explicit `--port` > `$PORT` env > an OS-assigned free port.
- **The manifest is the single source of truth** for "is a local daemon running"
  (`~/.chimaera/manifest.json`, mode 0600, carries the bearer token). It's written/removed by
  `chimaera_server::run`; `status`/`kill` only read it (cleaning up when the pid is dead).
- **`kill` never SIGKILLs and never removes a live daemon's manifest.** It SIGTERMs the manifest pid,
  polls `is_alive()` ~5s, and removes the manifest **only** once the daemon is confirmed dead — so a
  daemon that survives (it still holds its port) never leaves clients reading "not running".
- **Layering is strictly one-way:** the binary → server / remote → core. The binary crate is
  delegation-only.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

_No intent captured yet — pending the next `feat:` in this area._

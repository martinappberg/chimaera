# chimaera — the binary + CLI front-end

Orientation for coding agents. This crate is the `chimaera` executable: the clap
command tree and `main()` dispatch, and **almost nothing else**. Every subcommand
is a thin delegation to a sibling library crate. Parent map: repo-root
[AGENTS.md](../../AGENTS.md).

## What lives here (and what does NOT)

- **IS**: argument parsing, the global allocator + tracing init, and per-command
  orchestration/output. ~350 LoC of straight-line dispatch. Keep it that way — do
  not grow a command-abstraction layer for six flat subcommands.
- **IS NOT**: the daemon. `serve` is a 3-line delegation to
  `chimaera_server::run`. The **daemon lifecycle you're probably looking for**
  (manifest write/remove, SIGINT/SIGTERM, graceful shutdown, restart handoff,
  ledger snapshot) lives in `chimaera-server/src/lib.rs::run()`, NOT here.

## File map

| File | Command / role |
|---|---|
| `main.rs` | clap `Cli`/`Command` defs, all flags, `main()` dispatch, `parse_port` (`$PORT` fallback), `#[global_allocator]` mimalloc, tracing→stderr. The crate's only tests (CLI parse assertions). |
| `connect.rs` | `connect <host>`: calls `chimaera_remote::connect` with a progress closure, records the host, opens the tunnel URL, holds until Ctrl-C. |
| `daemonize.rs` | `serve --daemonize`: fork + `setsid` + re-exec so the daemon outlives its launching shell/ssh channel; re-points non-regular-file stdio at `/dev/null` (a caller's log redirect is kept). |
| `status.rs` | `status [host]`: local reads `chimaera_core::Manifest`; remote goes through `chimaera_remote`. |
| `kill.rs` | `kill`: SIGTERM the manifest pid, poll `is_alive()` ~5s, remove the manifest. |
| `doctor.rs` | `doctor`: probe write access to data/runtime dirs + ssh/claude on PATH. |

(`shell-integration` prints `chimaera_core::shellint::snippet()` — handled inline in `main.rs`.)

## Invariants (breaking these bites elsewhere)

- **`chimaera serve` is a load-bearing string.** `chimaera-remote` runs
  `.../chimaera serve` over ssh to start the daemon on a remote host. Renaming the
  subcommand, or making `serve` require args, silently breaks `connect`.
- **The manifest is the single source of truth for "is a local daemon running."**
  It is written/removed by `chimaera_server::run`; `status`/`kill` only read it (and
  clean it up when the pid is dead). It is 0600 (carries the bearer token).
- **Port precedence:** explicit `--port` > `$PORT` env > OS-assigned free port.

Layering is clean and one-way: binary → `chimaera-server` / `chimaera-remote` →
`chimaera-core`. Don't introduce a back-edge.

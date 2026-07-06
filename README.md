# Chimaera

**An agent workbench, not an IDE.** A single static Rust binary runs as a tmux-grade session
daemon on whatever host owns the work — a remote server, an HPC login node, or your laptop —
and serves a workspace-oriented UI where the primary objects are *agent sessions* living
inside *folders*, surrounded by the file previews, terminals, and git state that show what
those agents actually produced.

Sessions run the real interactive `claude` TUI in daemon-owned PTYs (same integration mode as
a VS Code integrated terminal), survive disconnects tmux-style, auto-name themselves, and
surface which ones need you.

**Status: M0 — walking skeleton.** See [DESIGN.md](DESIGN.md) for the full design.

## Quickstart (local)

```sh
just serve            # builds the web UI, runs the daemon in the foreground
chimaera status       # show the running daemon
chimaera kill         # stop it
```

## Quickstart (remote)

```sh
# on the remote host (one-time): put the linux-musl binary at ~/.chimaera/bin/chimaera
chimaera connect myhost    # start-or-attach the remote daemon, tunnel, open the UI
```

`connect` shells out to your system `ssh`, so `ProxyJump`, `ControlMaster`, and 2FA from
`~/.ssh/config` all just work.

## Development

- `just check` — fmt + clippy + tests
- `just dev-ui` — Vite dev server (proxies `/api` to a running daemon)
- `just release-linux` — static musl builds via cargo-zigbuild

License: TBD.

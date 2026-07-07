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

## Quickstart (native app, macOS)

```sh
just app-build        # bundles Chimaera.app + .dmg (crates/chimaera-app/target/release/bundle)
```

The app is self-contained: it starts (or attaches to) the local daemon, opens a **home
screen** of your workspaces and remote hosts, and gives every workspace a real window.
Quitting the app never kills the daemon — sessions are tmux-grade and keep running.

Remote hosts live on the home screen: add your cluster's ssh alias and chimaera installs
its own daemon into `~/.chimaera` on the host over ssh (no root), starts it, tunnels, and
lists that host's workspaces. Run `just dist` once to stock `~/.chimaera/dist/` with the
linux-musl binaries the auto-install deploys.

## Quickstart (local, browser)

```sh
just serve            # builds the web UI, runs the daemon in the foreground
chimaera status       # show the running daemon
chimaera kill         # stop it
```

## Quickstart (remote, CLI)

```sh
just dist                  # one-time: build deployable musl binaries into ~/.chimaera/dist
chimaera connect myhost    # install-if-missing, start-or-attach, tunnel, open the UI
```

`connect` shells out to your system `ssh`, so `ProxyJump`, `ControlMaster`, and 2FA from
`~/.ssh/config` all just work.

## Development

- `just check` — fmt + clippy + tests (daemon workspace)
- `just dev-ui` — Vite dev server (proxies `/api` to a running daemon)
- `just app-dev` / `just app-check` / `just app-build` — the native shell (a standalone
  cargo workspace in `crates/chimaera-app`, so the Tauri stack stays out of the daemon
  builds)
- `just release-linux` — static musl builds via cargo-zigbuild

License: TBD.

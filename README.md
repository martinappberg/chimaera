# Chimaera

**An agent workbench, not an IDE.** Chimaera runs your agent sessions as persistent,
daemon-owned processes on whatever host owns the work — your laptop, a remote server, or an
HPC login node — and gives you a workspace-first UI around them: the file previews, terminals,
and git state that show what those agents actually produced. One static Rust binary is the
whole server; the client is a web UI it serves itself, plus a native app that wraps the same
UI in real windows.

Agents run as the real interactive TUIs (`claude` and friends) in daemon-owned PTYs, so they
look, behave, and bill exactly like they do in any terminal. The daemon owns the sessions;
windows are just views. Close the laptop mid-run and nothing happens.

## Why

The usual way to run coding agents on a remote machine is a two-tool split: code-server in a
browser tab for looking at files, and tmux over SSH for keeping the agents alive — with all
the misery of nesting one inside the other. Browser terminals die on reload; tmux renders
nothing. The deliverable of an agent session is usually *files* — reports, plots, tables —
not the conversation, and no existing tool puts the sessions and their outputs in one window
that survives disconnection. Chimaera replaces that whole stack.

## Quickstart

Build from source. You need stable Rust and Node.

```sh
git clone https://github.com/martinappberg/chimaera
cd chimaera
npm --prefix web-ui install
npm --prefix web-ui run build      # the daemon embeds web-ui/dist
cargo build --release -p chimaera
```

Run it locally:

```sh
chimaera serve                     # starts the daemon, prints the UI URL
```

Run it on a remote host (an HPC login node, a dev server — anything you can ssh to):

```sh
just dist                          # one-time: build linux-musl binaries into ~/.chimaera/dist
chimaera connect <host>            # install-if-missing, start-or-attach, tunnel, open the UI
```

`connect` shells out to your system `ssh`, so `ProxyJump`, `ControlMaster`, and 2FA from
`~/.ssh/config` all just work. The daemon installs into `~/.chimaera` on the host — no root,
no containers, nothing else to set up.

If you use [just](https://github.com/casey/just), `just serve` does the UI build + daemon run
in one step, and `just app-build` bundles the native app.

## Features

- **Sessions that survive disconnect.** tmux-grade ownership: the daemon holds every PTY with
  full server-side terminal state. Reload, reconnect, or reattach from another machine and you
  get the identical screen back — no lost scrollback, no broken reconnect tokens.
- **Multi-agent launcher.** Claude Code, Codex, Gemini CLI, and Antigravity CLI in one
  launcher: detected if installed, resumable per workspace, and installable/updatable from
  official sources through the UI — binaries live under `~/.chimaera`, credentials stay
  entirely yours.
- **Linked terminals.** Hand an agent a leash to a live shell — the one with your environment
  already loaded — instead of paying setup cost per command. Links are user-granted, scoped,
  and audited in the terminal's own scrollback.
- **File previews.** Images, Markdown, CSV/TSV (gzip included), PDF, and sandboxed
  self-contained HTML reports — the MultiQC/FastQC class of scientific outputs that normally
  forces a code-server install. Server extracts, client renders, whole files are never loaded.
- **A real workbench layout.** Split panes, tabs, drag-and-drop, focus mode, a session strip
  that always says where you are, and attention states that tell you which agent needs you.
- **Themes.** Curated light and dark schemes, applied to the UI and injected into the agents'
  own TUIs so everything starts matching.
- **Native app.** A Tauri shell wraps the same UI: a window per workspace, a home screen of
  workspaces and remote hosts with one-click connect. Quitting the app kills nothing.

## Platforms

macOS and Linux. The daemon cross-compiles to fully static musl binaries (x86_64 and aarch64)
that run on old-glibc HPC systems — verified on a cluster running glibc 2.17, no root
required. Windows is untested.

## Staying current

Two things update themselves, so a running setup keeps pace with releases:

- **The daemon** is replaced on connect when it is older than the client — silently if it
  has no live sessions, otherwise behind an explicit "update" action that spells out what
  it ends (an idle shell holding a `module load`/conda environment is never killed without
  asking). Works the same for the local daemon and remote ones over ssh.
- **The native app** checks GitHub releases on launch and offers a one-click "update &
  restart"; the download is signature-verified before it installs.

## Status

Early and pre-release, moving fast. Interfaces, storage formats, and the wire protocol all
change without notice. See [DESIGN.md](DESIGN.md) for the full design and roadmap.

## License

Chimaera is licensed under the [GNU AGPL-3.0](LICENSE) — free for everyone to use, self-host,
and modify. If you want to build a closed-source product or service on Chimaera, a commercial
license is available: contact mkjberg@gmail.com.

Contributions require a CLA (see [CONTRIBUTING.md](CONTRIBUTING.md)).

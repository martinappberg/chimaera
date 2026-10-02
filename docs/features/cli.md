# CLI — the `chimaera` binary

The `chimaera` executable is a thin clap dispatch that delegates to the sibling crates. The
same static binary is the daemon, the remote-connect client, and the operator's control surface.

**Where it lives:** `crates/chimaera/src/` (`main.rs` clap defs + dispatch, `connect.rs`,
`status.rs`, `kill.rs`, `doctor.rs`, `plugin.rs`); shell-integration snippet in `chimaera-core/shellint`. Map:
[chimaera/AGENTS.md](../../crates/chimaera/AGENTS.md). Rules:
[rules/daemon.md](../../.claude/rules/daemon.md).

## Subcommands

| Command | Invocation | What it does |
|---|---|---|
| `serve` | `chimaera serve [--port N]` | Run the daemon in the foreground. **Load-bearing string** — `chimaera-remote` runs `…/chimaera serve` over ssh, so don't rename it. |
| `status` | `chimaera status [host]` | Local: read the `Manifest`. Remote: go through `chimaera-remote`. A dev build reports the isolated dev daemon (`~/.chimaera-dev`) on both ends — dev-ness is the build's property, not a flag. Prints running / stale / not-running. |
| `kill` | `chimaera kill` | Stop a running local daemon. |
| `connect` | `chimaera connect <host> [--local-port N] [--binary PATH] [--no-open] [--update-daemon] [--login-node]` | Stand up + tunnel to a remote daemon — see [remote-connect.md](remote-connect.md). A dev build always targets the isolated `~/.chimaera-dev` daemon (deploying your `just dist` build, never a release download). On a cluster (a host whose login shell reaches a batch scheduler) it starts nothing and explains `chimaera compute` instead, unless `--login-node` (the warned per-host override, remembered in `hosts.json`). A host the app was told isn't a cluster (`hosts.json` `not_cluster`) connects like any remote. |
| `compute` | `chimaera compute jobs <host>` · `add <host> <path> [--name N]` · `start <host> --time T [--open ws,… --name N --partition --account --qos --constraint --cpus --mem --gpus --startup --attached --save-as]` · `open <host> <workspace> [--job J] [--no-open]` · `close <host> <workspace>` · `move <host> <workspace> --to J` · `continue <host> <job> [--time T]` · `stop <host> <job>` | Slurm jobs on a cluster, with workspaces open inside them — short ssh commands, nothing on the login node. `open` holds the `ssh -L` forward until Ctrl-C; `--attached` holds an interactive-only partition's job in this terminal. See [compute.md](compute.md). Hidden: `job-host --job-dir D` (a job's main process, on its compute node) and `browse --state\|--dir P [--cluster-dir C]` (read-only JSON for the app, exits at once). |
| `doctor` | `chimaera doctor` | Probe write access to the data/runtime dirs and whether `ssh` / `claude` are on PATH. |
| `shell-integration` | `chimaera shell-integration` | Print the shell-integration snippet (for a remote host's rc file). |
| `plugin` | `chimaera plugin list` · `add <id>` · `add <owner/repo> [--version x]` · `add --path <dir>` · `update <id>` · `remove <id>` | Workbench plugins on the daemon running on this node, through the daemon's own routes (the token and body on `curl`'s stdin, never argv). `list` prints one line per entry: a leading `✓` for Chimaera's own plugins (in `plugins/plugins.lock`, installed or not), id, version, then `installed` (· `local build`; · `previous x`; · `chimaera pins x` when a first-party copy runs another version) or, for a lock entry with nothing installed, `available · install with: chimaera plugin add mycelium`; then any update and fault lines. `add` takes an id from `plugins/plugins.lock` (installs the pinned release; `--version` is refused there; a plugin built into Chimaera now, `agent-notes`, is refused with the daemon's words: "Built into Chimaera now: Agent communication (Settings → Agents). Remove this copy."), `owner/repo` or its `https://github.com/owner/repo` URL (the latest release, or `--version`); `add --path <dir>` installs a local build from a directory holding `plugin.wasm`, `plugin.toml` and optionally `SHA256SUMS` (the same version again replaces it). `update` installs the newest release from the plugin's repository; `remove` deletes the installed copies ("removed mycelium — `chimaera plugin add mycelium` installs it again"). Installs and updates print one line — "installed mycelium 0.2.1", "installed dev 0.2.0 (local build)", "updated mycelium 0.2.2 (was 0.2.1)" — never a hash: the daemon verifies every download and refuses a mismatch. See [plugins.md](plugins.md#versions-installs--updates). |

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

### Why the CLI is shaped this way
_Captured 2026-07-09 — drafted from DESIGN.md + code, confirmed live with the maintainer._

- **Problem it solves.** One static binary is the daemon, the connect-client, and the operator
  surface — the no-root deployment toolkit. The point is you can **run it from anywhere** — a login
  node, a dev box, even a Slurm compute node.
- **Deliberate.** `chimaera serve` is a load-bearing string (remote drives it over ssh); the manifest
  is the single source of truth for "is a daemon running"; `kill` never SIGKILLs and never removes a
  live daemon's manifest; `doctor` diagnoses the HPC "policy roulette" (some sites simply won't allow
  it).
- **Do not change:** the `serve` string; `kill`'s SIGTERM-only + remove-manifest-only-when-dead.

# chimaera-remote — SSH orchestration for `connect`

Orientation for coding agents. This crate drives the system `ssh`, `scp` and `curl`
to reach a Chimaera daemon on another host. It probes the host, deploys the binary,
starts or reuses the daemon, and opens port-forward tunnels. On a cluster it runs
the short Slurm commands and opens the second hop to a job's compute node.
Parent map: repo-root [AGENTS.md](../../AGENTS.md). Behavior: the
[remote connect](../../docs/features/remote-connect.md) and
[clusters](../../docs/features/compute.md) feature pages.

**Scope.** Two clients share this library: the CLI (`crates/chimaera/src/connect.rs`,
`status.rs`, `compute.rs`) and the native app (`crates/chimaera-app/src/shell/`,
`askpass.rs`, `wsl.rs`). Both speak the same protocol to a host.

**Not owned here.** The daemon and `chimaera job-host` live in `chimaera-server`
([map](../chimaera-server/AGENTS.md)). The Slurm parsers, `LaunchSpec`, the job
script and the cluster-folder records live in `chimaera-core` (`slurm.rs`,
`cluster.rs`; [map](../chimaera-core/AGENTS.md)). The askpass UI and window
management live in the app ([map](../chimaera-app/AGENTS.md)). The browser-pane
proxy's compute-node hop lives in the server's `proxy.rs`.

## File map

| File | What it owns |
|---|---|
| `hosts.rs` | `HostEntry` + `HostsStore`: saved aliases in `~/.chimaera/hosts.json`. Each mutation is a locked read-modify-write (`hosts.json.lock`, shared by the CLI and the app). `normalize_alias` strips a typed `ssh ` prefix and refuses whitespace or a leading `-`. |
| `cluster.rs` | Cluster jobs and workspaces from the client: `overview`, `facts`, the cluster folder (`read_config`/`write_config`/`add_workspace`/`list_dir`), startup commands, `start_job`/`stop_job`/`record_end`, setups and agent rules, `ensure_cluster_binary`, and the job-host client (`host_status`/`host_open`/`host_close`). Sections follow its `// ---` headers. |
| `lib.rs` | Everything else, in the sections below. Unit tests start at `mod tests` (about line 3600). |

`lib.rs` sections, in file order:

| Section | Main items |
|---|---|
| Transport | `ASKPASS_ALIAS_ENV`, `ASKPASS_CANCELLED`, `WslTransport` + `set_wsl_transport`, `transport_command` (+ the test-only `FAKE_TRANSPORT`), bounded child output (`CHILD_STDOUT_MAX_BYTES`, `output_bounded`), `control_path`/`control_dir`/`prepare_control_dir`, `ssh_opts`, `learn_ssh_config`, `ssh_cmd`/`scp_cmd`, `interactive_ssh_argv`. |
| `// --- Round-robin login nodes` | `Route` (`Alias`/`Node`/`NodeViaAlias`), the `ROUTES` map, `route_of`. |
| Connect types | `Phase`, `ConnectOpts`, `ClusterHost`, `LoginDaemon`, `RemoteHome`, `Decision` + `update_decision`, `TunnelPhaseError`, `Tunnel`. |
| Master health | `cancel_master_forward`, `clear_wedged_master`, `bounded_mux_ssh`, `http_alive`, `http_alive_authed`. |
| Decision phase | `RemoteOps` (trait) + `SshOps`, `locate`, `resolve_daemon`, `connect`, `locate_daemon`. |
| Probe | The framed-manifest scripts, `Probe`, `ProbeFailure`, `fatal_ssh_failure`, `remote_probe`, `detect_scheduler`, `scheduler_of`, `remote_sessions_count`, `stop_remote`. |
| Binary | `remote_target`, `dist_dir`, `release_triple`, `parse_release_meta`, `fetch_release_binary`, `resolve_local_binary`, `deploy_binary` + `install_staged_script`, `ensure_remote_binary` + `installed_binary_action`, `start_line`, `start_remote`. |
| Tunnel | `pick_local_port`, `Forward` + `spawn_forward` + `StderrTail`, `spawn_tunnel`, `open_proven_tunnel`, `prove_forward`, `wait_for_port`, `ssh_run`, `SSH_ONESHOT_SECS`. |
| `// --- Workspace jobs` | `ComputeRung` (`Direct`/`SshAdopt`), `ComputeTunnel`, `master_proxy_command`, `node_ssh_base`, `connect_compute_node`, `tunnel_proven`. |

## State owners

All state is per process. A relaunch relearns it.

- **ControlMaster sockets.** `control_path` puts them at `data_dir()/cm/%C`. If that path is too long for `sun_path`, `control_dir` uses a short `/tmp/chimaera-<hash>/cm` instead. Both levels of that fallback must be ours and 0700 (`chimaera_core::ensure_private_dir`; a 0755 one we own is tightened); otherwise ssh runs with `ControlPath=none`.
- **`HOST_KEY_STRICT`.** Per alias, whether the user's `ssh -G` says `StrictHostKeyChecking yes`. Learned by `learn_ssh_config` at each public entry that dials; unlearned = TOFU.
- **`ROUTES`.** The learned `Route` per alias. Tunnels, stops, session counts, compute tunnels and wedge checks all read it.
- **`SCHEDULERS`.** The scheduler the last detecting probe found per host.
- **`WSL_TRANSPORT`.** Set by the app on Windows. `None` on unix.
- **`HostsStore`.** The host list. Every mutation re-reads the file under `hosts.json.lock` (5 s bounded wait, then unlocked). It refuses to save when the file existed but could not be read.
- **`cluster.rs` caches.** `QUEUES` (queue per host, also saved to `data_dir()/cluster-queues/` so the 60 s floor holds across processes), `FLIGHTS` (one exec per host at a time), `FACTS` (discovery, `FACTS_TTL` = 24 h).
- **`Tunnel` / `ComputeTunnel`.** Each owns its ssh child. A forward that the master holds stays bound until `close()` cancels it with `ssh -O cancel`.

## Invariants (breaking these is a review failure)

- **Never name a real cluster or site.** No cluster, site or institution names go in code, comments, tests or docs. Use generic examples (`cluster`, `login.example.edu`).
- **One ControlMaster per destination.** Every ssh and scp call goes through `ssh_opts` (`ControlMaster=auto`, `ControlPersist=10m`, `StrictHostKeyChecking=accept-new` unless the user's own config is stricter). Do not add an ssh call that bypasses it. A new master can ask the user for a password or 2FA again.
- **A failed probe decides nothing.** Only a probe that ran may read the cached scheduler or conclude "nothing running". A refused host key, a refused sign-in, or too many keys (`fatal_ssh_failure`) ends the connect at once with a plain message; chimaera never edits `known_hosts`.
- **Never reimplement the ssh client.** The user's `~/.ssh/config` (ProxyJump, keys, 2FA) applies because this crate runs the system `ssh`. On Windows, ssh runs inside the WSL distro, because Win32-OpenSSH has no ControlMaster.
- **Set the askpass alias per child.** `ASKPASS_ALIAS_ENV` goes on each ssh/scp process. A process-global value would show one host's prompt in another host's window.
- **`connect` is the CLI's and the app's shared path.** Change it once, here. `RemoteOps` methods take `&impl Fn(Phase)`, not `&dyn`. A `dyn` makes the `connect` future `!Send` and breaks the app's `spawn`.
- **Resolve and upload the binary before you stop a daemon.** A failed fetch or upload must not leave a host without a daemon (the running one keeps its inode across the rename). `stop_remote` sends SIGTERM only and never SIGKILL.
- **Auto-deploy.** A release fetches the matching release asset and verifies its sha256 before running it. `deploy_binary` copies to `bin/chimaera.new` and renames it only when its size matches and it answers `--version` as chimaera. A running daemon or job keeps its old inode.
- **Dev builds never touch the real home.** `RemoteHome::current()` comes from the build (`is_dev_build`). A dev build uses `~/.chimaera-dev` and never deploys a release binary.
- **`chimaera serve` is a literal string.** A dev home is an env prefix (`serve_env`), never a flag.
- **Prove a tunnel with an authenticated HTTP response.** A bare TCP accept is not proof: after sleep, a dead forward still accepts. `open_proven_tunnel` and `tunnel_proven` require an authenticated 200.
- **Judge a manifest only on its own node.** In a login-node pool, `locate` routes to the manifest's node. If that node cannot be reached, it returns an error, not "dead". A second daemon would resume the same sessions.
- **Clusters run nothing on the login node.** Without `login_serve`, `connect` returns `ClusterHost` and starts nothing. `cluster.rs` sends each script on stdin (`sh -s`), never in argv: argv is visible to every user of a shared node. Query `squeue` at most once per `SQUEUE_FLOOR` (60 s) per host.
- **The job-host second hop.** `connect_compute_node` tries `Direct` first: the login master forwards to `node:port`. Then it tries `SshAdopt`: ssh to the node, with the first leg relayed through the login master by `-W`. The `SshAdopt` leg uses `ControlPath=none`. If both fail, it returns an error and the job keeps running.
- **Bound every child.** Each exec has a wall-clock deadline and capped stdout/stderr, and stdin is `/dev/null` unless it feeds a script. A forward's stderr is drained into a 4 KiB tail. The login tunnel runs in `BatchMode`; a refused sign-in on it is a plain error, never a `TunnelPhaseError` (those are retried on a fresh port).
- **Remote command lines are POSIX `sh`.** The login shell may be tcsh or fish, so every script, the start line included, goes through `sh_wrap`.
- **Shared-home writes are unique-temp + `mv`; compare-and-swaps run inside a `mkdir` lock; `job.json` changes field by field (`update_job`), never from a stale copy.**

## Checks

- `cargo test -p chimaera-remote`. The decision phase is tested through a fake `RemoteOps`. The transport is tested through `FakeSsh` (tests only): a script stands in for ssh/scp, logs each argv and answers with canned stderr and exit codes. `stop_script`, `probe_script`, `start_wait_script`, `start_line` and `install_staged_script` run under every installed login shell.
- CI has no remote host. To verify a runtime change, connect a dev build to a real host: `just app-dev-isolated`, or the CLI's `connect <host>`. A dev build uses `~/.chimaera-dev` on both ends. Report what you ran and observed. See the [develop skill](../../.claude/skills/develop/SKILL.md).

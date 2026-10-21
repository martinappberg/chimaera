# chimaera-core — the shared foundation crate

Orientation for coding agents. The leaf crate every other crate depends on:
on-disk lifecycle records, build/version identity, per-user directory resolution,
login-shell resolution, the token/id generator, and the shell-integration scripts.
Parent map: repo-root [AGENTS.md](../../AGENTS.md).

## The invariant that defines this crate

**core depends on NOTHING internal and owns no transport implementation.** No
axum, no tokio runtime, no websocket, no server/pty/agent/remote dependencies.
It does contain shared serializable cluster and job-host API types in
`cluster.rs`; their wire shapes are public contracts. A type belongs here when
it is shared and can stay free of the daemon's machinery.

**core is a path-dep of TWO workspaces** — the root daemon workspace AND the
standalone `crates/chimaera-app` (Tauri) workspace. Any change to core's
`Cargo.toml`/features must keep it building in **both** (the app pins concrete
versions). Run `cargo test` in the app workspace too when you touch core deps.

## File map

| File | What it owns |
|---|---|
| `lib.rs` | `Manifest` + `Handoff` (the daemon's on-disk lifecycle records), `VERSION`/`REPOSITORY`/`BUILD_ID` + build-match helpers (`builds_match`, `build_ref`, `parse_version`, `release_is_newer`), `managed_agents_dir` (user-scoped, ignores `CHIMAERA_DATA_DIR`) + `data_dir`/`config_dir`/`runtime_dir` (honoring `CHIMAERA_HOME`, and `CHIMAERA_DATA_DIR`/`CHIMAERA_RUNTIME_DIR` for a cluster workspace job's daemon), `login_shell` (+ pure `resolve_login_shell`), `generate_token`, `ensure_private_dir` (create 0700, or prove an existing dir is a real directory owned by the euid with no group/other bits; never chmods — for predictable names under a shared `/tmp`). |
| `proc.rs` | Cross-platform process control: `ProcessTree` (a process group on Unix, a Job Object on Windows; a terminal's tree ends with its session via kill-on-close), `JobLimits` (per-process memory, CPU time, below-normal priority; no per-file size limit on Windows), `pid_alive`, `terminate_pid`/`kill_pid`, `spawn_detached` (the daemon launch: `DETACHED_PROCESS` + new process group + breakaway on Windows), `end_with_owner`, `take_ctrl_c` (Windows: a new process group ignores Ctrl+C and passes that on; the PTY clears it before each spawn). There is no graceful SIGTERM on Windows: the daemon's `POST /api/v1/stop` stands in. |
| `paths.rs` | Path vocabulary shared by the daemon and clients: `wire_path` (forward slashes, no `\\?\` prefix, upper-case drive `C:/x`, UNC `//server/share/`; Unix paths unchanged), `canonicalize`, `home_dir`, `expand_tilde`, `find_on_path` + `is_executable` (PATH + PATHEXT on Windows). |
| `shellint.rs` + `shellint/` | The shell-integration subsystem (builds on Windows too; `integration.ps1` is the PowerShell script — OSC 133/633;E/7): materialize OSC 133/633/7 scripts, compose per-shell launch argv/env, and the remote-install snippet. Bash folds an indexed `PROMPT_COMMAND` array into one newline-joined string, every entry in order, so the DEBUG re-arm always runs last. |
| `slurm.rs` | The scheduler vocabulary, pure: `Scheduler`, the `squeue`/`sinfo` format strings + parsers (`Job`, `Partition`; `%S` rides `Job.start`, kept off the daemon's `/compute` wire; rows without it still parse), Slurm's duration grammar (capped at `MAX_DURATION_DAYS`, never overflowing), `clean_tool_stderr`, partition access from `sacctmgr` associations + `scontrol show partition` (`usable_partitions`), refusal classification, `LaunchSpec` (validation, `sbatch`/`srun` argv), job names. Shared by the daemon and every client; nothing site-specific, ever. |
| `cluster.rs` | A cluster's own folder on its shared home (`cluster/`): `ClusterConfig` (`cluster.json`: workspaces, saved setups, the last setup, rules for agents, learned facts), `ClusterFacts`, a job's records (`j/<id>/`: `JobRecord` `job.json`, `HostRecord` `host.json`, `HostingRecord` `workspaces.json`), `WorkspaceSeed`, job-host's API types (`HostedState`/`HostedWorkspace`/`JobHostStatus`/`HeldElsewhere`), the job script (`job_script`: `exec`s `chimaera job-host`), `browse_state` / `browse_dir` (what `chimaera browse` prints — read-only, bounded: `--state` stats every job folder but reads the JSON of only the newest `BROWSE_MAX_JOBS`), `expand_path` (no shell), id makers/validators, and the env names job-host gives each workspace chimaera (`CHIMAERA_DATA_DIR`, `CHIMAERA_RUNTIME_DIR`, `CHIMAERA_CLUSTER_WORKSPACE`, `CHIMAERA_HOST_PRELUDE_FILE`, `CHIMAERA_AGENT_RULES_FILE`, `CHIMAERA_CLUSTER_FACTS_FILE`). |

## Invariants / gotchas

- **`generate_token` is the general-purpose random-hex source**, not just auth: the
  server slices it (`[..8]`/`[..16]`/`[..32]`) for session/ticket/workspace ids and
  agent keys. Don't narrow its contract to "tokens."
- **`Manifest` is atomic + 0600.** Written through a unique `create_new` temp opened
  at mode 0600, then renamed, because it carries the bearer token. `Manifest.build`
  serde-defaults to an ancient sentinel so an
  old manifest still parses.
  `Manifest.runtime_leases` defaults false: old daemons do not protect managed
  agent packages, so automatic package cleanup defers while their manifest is live.
- **A manifest's pid means something only on the node that wrote it.** HPC login
  nodes share `$HOME`, so every node reads the same file: check `written_here()`
  (`this_node()` vs `hostname`, via `same_node`) before trusting `is_alive()`, and
  remove with `remove_if_owned()` (same node + pid + start) so a daemon never unlinks
  another node's live record.
- **`Handoff` is consume-once, ~120s fresh, 0600**, and is written only by the
  **daemon itself** — at listen time, refreshed every `HANDOFF_REFRESH_SECS` (30 s)
  while it serves, and at its graceful shutdown — not by "the app/connect". So a
  crash restart within the window keeps port + token like a planned one; a daemon
  dead longer than the window leaves a stale record and its successor mints a
  fresh token — so a crash no longer rotates the token (at rest the manifest beside
  it already holds the same token at 0600). Keep the refresh period at most half the
  window (a const assert). `Manifest::load_at`/`Handoff::write_at`/`consume_at` take
  explicit paths for tests.
- **`CHIMAERA_HOME` moves only the daemon's bookkeeping dirs.** Spawned shells/agents
  keep the real `$HOME`, so `~/.claude` auth still works under an isolated daemon.
  Unstamped dev builds default to `~/.chimaera-dev`; release builds use
  `~/.chimaera`. `CHIMAERA_DATA_DIR` / `CHIMAERA_RUNTIME_DIR` override the
  corresponding paths for cluster workspace daemons.
- Dir resolvers are **best-effort**: on `create_dir_all` failure they `warn!` and
  still return the path (callers fail on the later read/write). Don't `panic!` in a
  dir resolver.

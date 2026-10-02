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
| `lib.rs` | `Manifest` + `Handoff` (the daemon's on-disk lifecycle records), `VERSION`/`REPOSITORY`/`BUILD_ID` + build-match helpers (`builds_match`, `build_ref`, `parse_version`, `release_is_newer`), `data_dir`/`config_dir`/`runtime_dir` (honoring `CHIMAERA_HOME`, and `CHIMAERA_DATA_DIR`/`CHIMAERA_RUNTIME_DIR` for a cluster workspace job's daemon), `login_shell` (+ pure `resolve_login_shell`), `generate_token`. |
| `cloud_providers.rs` + `cloud-providers.json` | Shared provider identities and exact browser authentication origins; adapters stay in the daemon. The UI imports the same JSON. A catalog entry alone does not enable a provider; follow the [extension checklist](../chimaera-server/src/cloud/providers/AGENTS.md#adding-a-provider). |
| `shellint.rs` + `shellint/` | The shell-integration subsystem: materialize OSC 133/633/7 scripts, compose per-shell launch argv/env, and the remote-install snippet. |
| `slurm.rs` | The scheduler vocabulary, pure: `Scheduler`, the `squeue`/`sinfo` format strings + parsers (`Job`, `Partition`), Slurm's duration grammar, `clean_tool_stderr`, partition access from `sacctmgr` associations + `scontrol show partition` (`usable_partitions`), refusal classification, `LaunchSpec` (validation, `sbatch`/`srun` argv), job names. Shared by the daemon and every client; nothing site-specific, ever. |
| `cluster.rs` | A cluster's own folder on its shared home (`cluster/`): `ClusterConfig` (`cluster.json`: workspaces, saved setups, the last setup, rules for agents, learned facts), `ClusterFacts`, a job's records (`j/<id>/`: `JobRecord` `job.json`, `HostRecord` `host.json`, `HostingRecord` `workspaces.json`), `WorkspaceSeed`, job-host's API types (`HostedState`/`HostedWorkspace`/`JobHostStatus`/`HeldElsewhere`), the job script (`job_script`: `exec`s `chimaera job-host`), `browse_state` / `browse_dir` (what `chimaera browse` prints — read-only, bounded), `expand_path` (no shell), id makers/validators, and the env names job-host gives each workspace chimaera (`CHIMAERA_DATA_DIR`, `CHIMAERA_RUNTIME_DIR`, `CHIMAERA_CLUSTER_WORKSPACE`, `CHIMAERA_HOST_PRELUDE_FILE`, `CHIMAERA_AGENT_RULES_FILE`, `CHIMAERA_CLUSTER_FACTS_FILE`). |

## Invariants / gotchas

- **`generate_token` is the general-purpose random-hex source**, not just auth: the
  server slices it (`[..8]`/`[..16]`/`[..32]`) for session/ticket/workspace ids and
  agent keys. Don't narrow its contract to "tokens."
- **`Manifest` is atomic + 0600.** Written through a unique `create_new` temp opened
  at mode 0600, then renamed, because it carries the bearer token. `Manifest.build`
  serde-defaults to an ancient sentinel so an
  old manifest still parses.
- **A manifest's pid means something only on the node that wrote it.** HPC login
  nodes share `$HOME`, so every node reads the same file: check `written_here()`
  (`this_node()` vs `hostname`, via `same_node`) before trusting `is_alive()`, and
  remove with `remove_if_owned()` (same node + pid + start) so a daemon never unlinks
  another node's live record.
- **`Handoff` is consume-once, ~120s fresh, 0600**, and is written by the **daemon
  on its own graceful shutdown** (a crash leaves none) — not by "the app/connect".
- **`CHIMAERA_HOME` moves only the daemon's bookkeeping dirs.** Spawned shells/agents
  keep the real `$HOME`, so `~/.claude` auth still works under an isolated daemon.
  Unstamped dev builds default to `~/.chimaera-dev`; release builds use
  `~/.chimaera`. `CHIMAERA_DATA_DIR` / `CHIMAERA_RUNTIME_DIR` override the
  corresponding paths for cluster workspace daemons.
- Dir resolvers are **best-effort**: on `create_dir_all` failure they `warn!` and
  still return the path (callers fail on the later read/write). Don't `panic!` in a
  dir resolver.

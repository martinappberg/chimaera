# Compute (Slurm awareness)

Scheduler detection + a slim queue strip: when the host a daemon runs on has Slurm, the
workbench shows it — a compute chip in the rail's bottom bar with the user's queued/running
job count, and a popover listing their jobs and the cluster's partitions. On a laptop (no
scheduler) the whole surface is invisible. First slice of the M5 HPC layer's *placement
axis*; the deep design (Mode 1 login-node job control, Mode 2 compute-node sessions) lives
in the [architecture guide](../agent-guides/architecture.md#environment-prelude-compute-node-sessions).

**Where it lives (shared):** daemon `crates/chimaera-server/src/compute.rs` (detection +
snapshot service + route). UI `web-ui/src/lib/workspace/compute.ts` (store/poller) and the
chip + popover in `web-ui/src/App.svelte` (the `.daemon` bottom bar, beside the git chip).
Wire: `GET /api/v1/compute` (bearer-authed; `?refresh=true` re-detects).

## Detection & the snapshot

- **What & when.** "Is this an HPC, and what do I have running?" — answered by the daemon
  about its *own* host (a remote daemon detects its own cluster; nothing is probed over ssh
  at connect time). Detection = `command -v sbatch` through the user's login shell (same
  reasoning as git resolution: tools often arrive via a profile-managed PATH), cached for
  the daemon's lifetime; `?refresh=true` re-detects (e.g. after a `module load slurm`).
- **How it's used.** The UI fetches `GET /api/v1/compute` at boot; if `scheduler` is
  `"none"` it never asks again that page-load. On a Slurm host it refetches every 60s while
  the tab is visible. The chip appears only when `scheduler == "slurm"`; clicking it opens
  the popover (jobs + partitions + a manual refresh).
- **Where it lives.** `ComputeService` (detection + 30s snapshot cache, single-flight —
  concurrent requests coalesce on one refresh); `parse_squeue`/`parse_sinfo` are pure and
  unit-tested against Sherlock-shaped fixtures.
- **Key behaviors / constraints.**
  - **Resource discipline is the design** (sysadmins ban tools that hammer `squeue`):
    every child process gets a 5s kill-on-timeout and an output cap; jobs/partitions are
    capped at 50 rows with a `truncated` flag; snapshots cache ~30s; nothing persists.
  - **Format strings, not `--json`**: `squeue -u <user> --noheader -o "%i|%j|%P|%T|%L|%N"`
    and `sinfo --noheader -o "%P|%a|%D"` work on the old Slurm versions real clusters run,
    and bound the output by construction. `-u <user>` rather than `--me` (older Slurm).
  - **Degrade, never 500**: a wedged controller or parse noise yields an empty-but-tagged
    snapshot — the strip is orientation, not ground truth. Unparseable lines are skipped
    (Slurm warning banners precede output on some clusters).
  - The default partition carries sinfo's `*` suffix → surfaced as `"default": true`;
    duplicate (partition, avail) rows are merged, "up" wins.
  - **Test knob:** `CHIMAERA_SLURM_BINDIR` points at a directory of stand-in
    `sbatch`/`squeue`/`sinfo` so the whole surface can be driven live without a cluster
    (the `CHIMAERA_RELEASES_API` pattern).
- **Status: partial (by design).** Detection + the strip only. Planned on this seam, per
  the architecture spec: job↔session linking, the login-node Slurm skill (Mode 1), and
  sbatch-launched compute-node sessions (Mode 2).

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Slurm awareness — why it exists
_Captured 2026-07-15 (from the maintainer; drafted from his design-session words, confirmed by him)._

- **Problem it solves:** the cluster should be visible in the workbench — detect Slurm,
  show your queue. First step of the placement axis: Mode 1 (login-node job control via an
  agent skill) and Mode 2 (own the full session on a compute node — chosen as second-hop
  daemon over srun-prefixing for isolation and ownership) build on this seam, toward the
  premium synced-workspace vision.
- **How settled (intent grade: the invariants are core to this feature; the rest is
  addition):** promises — cluster behavior is **probed per cluster, never assumed**
  ("not all environments are the same"; "no shame in saying not supported"), and the
  compute surface stays invisible/quiet off-cluster. The chip/popover design, polling
  cadence, caps, and wire shape are mechanics, improvable.
- **Non-obvious decision / deliberately left out:** the staging is deliberate — detection
  first, then job↔session linking, the Mode 1 skill, and Mode 2 compute-node sessions.
  "Not supported on this cluster" is an acceptable, honest end state where the tunnel
  ladder finds no path.
- **Do not change:** the probe-and-degrade-honestly posture, and hidden-off-cluster.
  Everything else is an addition, open to improvement.

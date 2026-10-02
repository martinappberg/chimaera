<script lang="ts">
  import { onMount } from "svelte";
  import HomeNavigation from "./HomeNavigation.svelte";
  import HomeActions from "./HomeActions.svelte";
  import { isMac } from "../shared/keys";
  import { paidPlan, proOffered } from "../net/plan";
  import { gatewayWorkspace, isBrowserGateway } from "../net/base";
  import { projectWhere, projectWhereLabel } from "../net/placement";
  import type ClusterPage from "./ClusterPage.svelte";
  import { keyHint } from "../shared/keybindings";
  import { isBusy, needsApproval, type Session, type Workspace } from "./sessions";
  import {
    addHost,
    beginUpdate,
    checkAppUpdate,
    connectHost,
    closeThisWindow,
    disconnectHost,
    endHostSessions,
    isNativeShell,
    listHosts,
    localDaemonState,
    navigateHome,
    onClusterChanged,
    onConnectProgress,
    onHostStatus,
    onProChanged,
    openWindow,
    proOpenCloudProject,
    remoteWorkspaces,
    removeHost,
    setNotCluster,
    setHostDirectSsh,
    shutdownHost,
    updateLocalDaemon,
    type ConnectProgress,
    type HostState,
    type HostStatusEvent,
    type LocalDaemonState,
  } from "../net/native";
  import { computeStatus } from "./compute";
  import ComputeBanner from "./ComputeBanner.svelte";
  import { clusterNow, clusterOverviews } from "./clusterStore.svelte";
  import { anyJobRunning, hostSummary, schedulerLabel } from "./clusterRow";
  import { getJobContext, isHomeHub, type Health } from "../net/api";
  import { asyncDisposer } from "../shared/asyncDisposer";
  import { pageVisible } from "../shared/visibility";
  import { fetchOwnershipHints } from "./placementHints";
  import { relativeAge } from "./launcher";
  import { checkForUpdates, MANAGED_UPDATES, updateState } from "./update.svelte";

  interface Props {
    workspaces: Workspace[];
    sessions: Session[];
    hostLabel: string;
    health: Health | null;
    /** Authenticated HTTP health or the authenticated events socket is up. */
    daemonReachable: boolean;
    /** Open `w` in THIS window. */
    onOpen: (w: Workspace) => void;
    /** Remove `w` from the daemon's registry (files untouched). */
    onRemove: (w: Workspace) => void;
    /** End every live session in `w` (the registration itself is untouched). */
    onStop: (w: Workspace) => void;
    /** Open the folder picker (browse/register a new folder). */
    onOpenFolder: () => void;
    onSettings: () => void;
    onPro: () => void;
  }

  let {
    workspaces,
    sessions,
    hostLabel,
    health,
    daemonReachable,
    onOpen,
    onRemove,
    onStop,
    onOpenFolder,
    onSettings,
    onPro,
  }: Props = $props();

  const native = isNativeShell();

  /** The daemon THIS home screen belongs to — null for the local daemon, the
   *  host alias for a remote window. Opening one of this screen's own
   *  workspaces in a new window must target this same daemon: a remote
   *  window's home screen lists the REMOTE daemon's workspaces, so passing the
   *  local `null` would open a local window carrying a remote workspace id the
   *  local daemon doesn't have — which lands right back on the launcher (the
   *  "can't open a second workspace on a remote" bug). */
  const ownAlias = $derived(hostLabel === "local" ? null : hostLabel);
  /** Every native remote detail needs a route back to local Home. The normal
   *  in-place flow navigates its hub, while a persisted pre-hub window uses
   *  the compatibility path below to open Home and retire itself. */
  const showBackToHome = $derived(native && ownAlias !== null);

  const sorted = $derived(
    [...workspaces].sort((a, b) => (b.last_opened_at ?? 0) - (a.last_opened_at ?? 0)),
  );

  /** Live rollup per workspace: total live sessions + how many need you. */
  const liveByWs = $derived.by(() => {
    const map = new Map<string, { live: number; attn: number }>();
    for (const s of sessions) {
      const entry = map.get(s.workspace_id) ?? { live: 0, attn: 0 };
      if (s.alive) entry.live += 1;
      // Counts are approvals only (needsApproval — the same predicate as
      // App's needsYou pill/title and the Dock badge). Alive-gated: a crashed
      // chat driver stays registered (alive:false) until deleted, and only a
      // LIVE ask is something the user can act on.
      if (s.alive && needsApproval(s)) entry.attn += 1;
      map.set(s.workspace_id, entry);
    }
    return map;
  });

  /** Where a project's work runs when that is not (only) here — "In the
   *  cloud", "Coming home…" — from this daemon's own Pro ownership answer. A
   *  project the cloud holds otherwise looks idle here. Empty without Pro.
   *  Read while the page shows (and re-read while Pro answers), never on a
   *  remote host's Home or in a project view, which have no ownership here. */
  let placeHints = $state(new Map<string, string>());
  $effect(() => {
    if (ownAlias !== null || isBrowserGateway() || !$pageVisible) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const read = async (): Promise<void> => {
      const answer = await fetchOwnershipHints();
      if (stopped) return;
      // An unreadable answer keeps what was known (a blip drops no hint).
      if (answer !== null) placeHints = answer.hints;
      // A daemon without Pro has nothing to poll for; a return-to-visible
      // re-runs this effect and asks once more.
      if (answer === null || answer.configured) timer = setTimeout(() => void read(), answer === null ? 30_000 : 15_000);
    };
    void read();
    return () => {
      stopped = true;
      if (timer !== null) clearTimeout(timer);
    };
  });

  /** Confirm target for workspace removal (one at a time, Escape cancels). */
  let confirmRemoveId = $state<string | null>(null);
  /** Confirm target for ending a workspace's live sessions. */
  let confirmStopId = $state<string | null>(null);

  // --- remote hosts (native shell only) --------------------------------------

  let hosts = $state<HostState[]>([]);
  /** Remote workspace lists per connected alias. */
  let remoteWs = $state<Map<string, Workspace[]>>(new Map());
  /** Human line under a host while its connect flow runs. */
  let phases = $state<Map<string, string>>(new Map());
  let hostErrors = $state<Map<string, string>>(new Map());
  let savingDirect = $state(new Set<string>());
  let addOpen = $state(false);
  let addAlias = $state("");
  let addError = $state<string | null>(null);
  let confirmForget = $state<string | null>(null);
  /** Host pending a "end all sessions" / "shut down" confirm (alias). */
  let confirmEnd = $state<string | null>(null);
  let confirmShutdown = $state<string | null>(null);

  // --- clusters (hosts whose login shell reaches a batch scheduler) ----------
  // Nothing of ours runs on a cluster's login node: its row reads the
  // cluster's workspaces through short ssh commands, and clicking it opens
  // the cluster page in place (the job is a detail of each workspace there).

  /** The cluster page shown in place of the host list (its alias), or null. */
  let clusterView = $state<string | null>(null);
  /** The cluster page's code — its own chunk, loaded the first time a
   *  cluster opens (most homes never show one). */
  let ClusterPageView = $state<typeof ClusterPage | null>(null);

  function showCluster(alias: string): void {
    if (ClusterPageView === null) {
      import("./ClusterPage.svelte").then(
        (m) => {
          ClusterPageView = m.default;
        },
        (err: unknown) => {
          // Stale assets after an update: say so on the row instead of a
          // blank page with no way back.
          clusterView = null;
          const why = err instanceof Error ? err.message : String(err);
          hostErrors = new Map(hostErrors).set(alias, `couldn't open the cluster page: ${why}`);
        },
      );
    }
    clusterView = alias;
  }

  /** A host the shell knows as a cluster (this session's probe, or a hint). */
  function isCluster(h: HostState): boolean {
    return h.cluster !== null || h.status === "cluster";
  }

  /** A cluster host with an ssh connection this session — the only kind the
   *  row may read from (an unconnected one could raise an auth prompt). */
  function clusterReadable(h: HostState): boolean {
    return isCluster(h) && (h.status === "cluster" || h.status === "connected");
  }

  /** The cluster row's one line, from the shared overview cache. */
  function clusterLine(h: HostState): string | null {
    const entry = clusterOverviews.entry(h.alias);
    if (entry === undefined) return null;
    if (entry.overview === null) return entry.loading ? "reading the cluster…" : null;
    return hostSummary(entry.overview, clusterNow(entry));
  }

  /** Whether any of a cluster's jobs is running (the row's dot). */
  function clusterRunning(alias: string): boolean {
    const ov = clusterOverviews.entry(alias)?.overview;
    return ov !== null && ov !== undefined && anyJobRunning(ov);
  }

  /** This home screen sits on a compute-node daemon (a workspace's job):
   *  either the window's own `job=` params or the daemon's `/compute` `self`
   *  block. Its workspace ids were minted on the JOB daemon —
   *  `openWindow(ownAlias, …)` targets the bare cluster alias, where those
   *  ids don't exist — so cross-window opening isn't offered here. */
  const jobScoped = $derived(
    getJobContext() !== null || ($computeStatus?.self ?? null) !== null,
  );
  /** A cluster workspace's window: its home is the cluster page (its own
   *  chimaera knows only this one workspace; the page knows them all). */
  const clusterWs = $derived(native && ownAlias !== null ? (getJobContext()?.cws ?? null) : null);

  // --- local daemon build parity (native shell, local window only) ------------

  let localState = $state<LocalDaemonState | null>(null);
  let localUpdating = $state(false);
  let localError = $state<string | null>(null);

  // --- the version stamp: which daemon, and is it current? --------------------

  /** A newer release the daemon knows of (its own check), shown on the stamp. */
  const stampNewer = $derived(
    updateState.daemon?.state === "available" ? (updateState.daemon.latest?.version ?? null) : null,
  );
  /** Read when the pointer arrives, so "checked 2h ago" is true on hover
   *  without a ticking clock on the home screen. */
  let stampNow = $state(Date.now());

  const stampTitle = $derived.by(() => {
    const v =
      health?.version === "0.0.1"
        ? `development daemon (build ${health.build ?? "unknown"})`
        : `daemon v${health?.version ?? "?"}`;
    const d = updateState.daemon;
    if (d === null) return `${v}. Click to check for updates.`;
    // The account's cloud: nothing to check (a click shows the same line).
    if (d.managed) return `${v}. ${MANAGED_UPDATES}`;
    if (d.dev) return `${v}: release updates don't apply. Click to check anyway.`;
    const checked = d.checked_at === null ? null : relativeAge(d.checked_at, stampNow);
    const when = checked === null ? "" : checked === "now" ? " (checked just now)" : ` (checked ${checked} ago)`;
    switch (d.state) {
      case "available":
        return `${v}: ${d.latest?.version ?? "a newer release"} is available. Click for details.`;
      case "failed":
        return `${v}: couldn't check for updates${when}: ${d.error ?? "unknown error"}. Click to try again.`;
      case "unchecked":
        return `${v}: not checked for updates yet. Click to check.`;
      case "current":
        return `${v}: up to date${when}. Click to check again.`;
      case "managed":
        return `${v}. ${MANAGED_UPDATES}`;
    }
  });

  // --- app self-update (native shell only) -----------------------------------

  /** A newer signed app build is available on GitHub (its version), or null. */
  let appUpdate = $state<string | null>(null);
  let appUpdating = $state(false);
  let appUpdateError = $state<string | null>(null);

  async function installApp(): Promise<void> {
    appUpdateError = null;
    appUpdating = true;
    try {
      // The full chain: app bundle now, then the relaunched process updates
      // the local daemon (windows and sessions restore via the shell's
      // window registry + the daemon's ledger). Never returns on success.
      await beginUpdate();
    } catch (e) {
      appUpdateError = e instanceof Error ? e.message : String(e);
      appUpdating = false;
    }
  }

  /** Sessions doing ACTIVE work right now (a shell running a command, an agent
   *  mid-turn). Idle sessions restore cleanly across a stateful daemon
   *  restart, so only these are worth warning about before an update. */
  const busyNow = $derived(sessions.filter(isBusy).length);

  const PHASE_LABEL: Record<ConnectProgress["phase"], string> = {
    probing: "probing for a running daemon…",
    routing: "reaching the daemon's login node…",
    updating: "updating the daemon…",
    downloading: "downloading chimaera…",
    installing: "installing chimaera…",
    starting: "starting the daemon…",
    tunneling: "bringing the tunnel up…",
    done: "",
  };

  onMount(() => {
    if (clusterWs !== null && ownAlias !== null) {
      showCluster(ownAlias);
      void refreshHosts();
      return;
    }
    // A remote window's home has no remote-hosts machinery (that section is
    // the LOCAL first screen's); a job window's compute UI is its banner.
    if (!native || ownAlias !== null) return;
    void refreshHosts();
    // Every native window asks for the shell state: the outdated note renders
    // only on the local window, but the dev-build flag drives the host-row
    // dev badges everywhere.
    void localDaemonState().then((s) => (localState = s));
    // Only the local window quietly asks GitHub whether a newer signed app
    // build exists.
    if (hostLabel === "local") {
      void checkAppUpdate().then((v) => (appUpdate = v));
    }
    const unlisteners: Array<() => void> = [asyncDisposer(onProChanged(() => { if (document.visibilityState === "visible") void refreshHosts(); }))];
    const onVis = (): void => {
      if (document.visibilityState === "visible") void refreshHosts();
    };
    document.addEventListener("visibilitychange", onVis);
    unlisteners.push(() => document.removeEventListener("visibilitychange", onVis));
    // A cluster's workspaces changed (a start, stop, or handoff — from this
    // window or another): refresh its row. The cluster page, when open,
    // listens for itself and shares the same cache.
    unlisteners.push(
      asyncDisposer(
        onClusterChanged((alias) => {
          if (clusterView !== alias) void clusterOverviews.refresh(alias, 0);
        }),
      ),
    );
    unlisteners.push(
      asyncDisposer(
        onConnectProgress((p) => {
          if (p.phase === "done") {
            phases = mapWithout(phases, p.alias);
            return;
          }
          const label =
            p.phase === "routing" && p.node !== undefined
              ? `reaching the daemon on ${p.node}…`
              : (PHASE_LABEL[p.phase] ?? p.phase);
          phases = new Map(phases).set(p.alias, label);
        }),
      ),
    );
    // Keep host rows live: the shell's health monitor reports a dropped or
    // recovered tunnel, and connect flights report their outcome — including
    // ones this window didn't start (startup restore, another window's
    // reconnect), which otherwise leave the row "connecting" forever.
    unlisteners.push(
      asyncDisposer(
        onHostStatus((e) => {
          // Job-window events belong to the cluster overview, never a host row.
          const job = e.alias.indexOf("#job");
          if (job !== -1) {
            if (e.status === "ended") void clusterOverviews.refresh(e.alias.slice(0, job), 0);
            return;
          }
          if (hosts.some((host) => host.alias === e.alias)) {
            applyHostStatus(e);
            return;
          }
          // Not listed yet: startup restore can report "connected" before the
          // first list resolves. Re-list and apply the latest event only if the
          // alias is a real row — managed cloud connections share this bus but
          // never appear in the list, so they still never fetch workspaces.
          unlistedStatus.set(e.alias, e);
          void refreshHosts().then(() => {
            const latest = unlistedStatus.get(e.alias);
            if (latest === undefined) return;
            unlistedStatus.delete(e.alias);
            if (hosts.some((host) => host.alias === e.alias)) applyHostStatus(latest);
          });
        }),
      ),
    );
    return () => unlisteners.forEach((u) => u());
  });

  /** The latest status event per alias that arrived before its row was listed. */
  const unlistedStatus = new Map<string, HostStatusEvent>();

  function applyHostStatus(e: HostStatusEvent): void {
    const row = hosts.find((h) => h.alias === e.alias);
    if (row === undefined) return;
    const plainCluster = row.status === "cluster" && row.cluster?.login_serve !== true;
    if (!plainCluster || e.status === "error") hosts = hosts.map((h) =>
      h.alias === e.alias
        ? {
            ...h,
            status: e.status === "connected" ? "connected" : "disconnected",
            local_port: e.status === "connected" ? (e.local_port ?? h.local_port) : null,
            // Authoritative on every connected event: a reconnect this
            // window didn't start may have re-routed the alias.
            node: e.status === "connected" ? (e.node ?? null) : h.node,
          }
        : h,
    );
    // Any terminal transition ends the phase line, whoever ran the connect.
    phases = mapWithout(phases, e.alias);
    if (e.status === "down") {
      remoteWs = mapWithout(remoteWs, e.alias);
    }
    if (e.status === "error" && e.error !== undefined) {
      hostErrors = new Map(hostErrors).set(e.alias, e.error);
    } else if (e.status === "connected" && !plainCluster) {
      hostErrors = mapWithout(hostErrors, e.alias);
      // A connect this window didn't run (startup restore, another
      // window) still gets its workspace list, so the row is browsable.
      if (!remoteWs.has(e.alias)) {
        void remoteWorkspaces(e.alias)
          .then((list) => {
            remoteWs = new Map(remoteWs).set(
              e.alias,
              [...list].sort(
                (a, b) => (b.last_opened_at ?? 0) - (a.last_opened_at ?? 0),
              ),
            );
          })
          .catch(() => {
            // dropped again in between; the next transition retries
          });
      }
    }
  }

  async function refreshHosts(): Promise<void> {
    try {
      hosts = await listHosts();
    } catch {
      // shell unavailable mid-teardown; leave the list as-is
    }
  }

  async function connect(alias: string, updateDaemon = false): Promise<void> {
    hostErrors = mapWithout(hostErrors, alias);
    hosts = hosts.map((h) => (h.alias === alias ? { ...h, status: "connecting" } : h));
    if (updateDaemon) phases = new Map(phases).set(alias, PHASE_LABEL.updating);
    try {
      const state = await connectHost(alias, updateDaemon);
      hosts = hosts.map((h) => (h.alias === alias ? state : h));
      if (state.status === "cluster") {
        // No tunnel, no daemon on the login node: the cluster page opens in
        // place, right here in the local home.
        showCluster(alias);
        return;
      }
      // Host browsing is navigation inside the singleton Home hub. A workspace
      // click continues in that window; only an explicit new-window gesture
      // creates another native workbench.
      await navigateHome(alias);
    } catch (e) {
      hostErrors = new Map(hostErrors).set(alias, e instanceof Error ? e.message : String(e));
      void refreshHosts();
    } finally {
      phases = mapWithout(phases, alias);
    }
  }

  async function navigateHost(alias: string, wsId: string | null = null): Promise<void> {
    hostErrors = mapWithout(hostErrors, alias);
    try {
      await navigateHome(alias, wsId);
    } catch (error) {
      hostErrors = new Map(hostErrors).set(
        alias,
        error instanceof Error ? error.message : String(error),
      );
    }
  }

  async function disconnect(alias: string): Promise<void> {
    await disconnectHost(alias);
    remoteWs = mapWithout(remoteWs, alias);
    void refreshHosts();
  }

  async function setDirect(host: HostState, input: HTMLInputElement): Promise<void> {
    if (savingDirect.has(host.alias)) return;
    savingDirect = new Set(savingDirect).add(host.alias);
    hostErrors = mapWithout(hostErrors, host.alias);
    try {
      const updated = await setHostDirectSsh(host.alias, input.checked);
      hosts = hosts.map(row => row.alias === host.alias ? updated : row);
    } catch {
      input.checked = host.direct_ssh === true;
      hostErrors = new Map(hostErrors).set(host.alias, "This connection preference couldn't be saved. Try again.");
    } finally {
      const pending = new Set(savingDirect); pending.delete(host.alias); savingDirect = pending;
    }
  }

  async function forget(alias: string): Promise<void> {
    confirmForget = null;
    await removeHost(alias);
    remoteWs = mapWithout(remoteWs, alias);
    clusterOverviews.forget(alias);
    void refreshHosts();
  }

  /** A host the user said isn't a cluster is one after all: drop its tunnel
   *  (a daemon on it keeps running, as on any disconnect), then connect,
   *  which lands on its cluster page. */
  async function backToCluster(alias: string): Promise<void> {
    hostErrors = mapWithout(hostErrors, alias);
    try {
      if (hosts.find((h) => h.alias === alias)?.status === "connected") await disconnectHost(alias);
      const state = await setNotCluster(alias, false);
      hosts = hosts.map((h) => (h.alias === alias ? state : h));
      await connect(alias);
    } catch (e) {
      hostErrors = new Map(hostErrors).set(alias, e instanceof Error ? e.message : String(e));
    }
  }

  /** End all sessions on a host; its daemon and the tunnel stay up. */
  async function endSessions(alias: string): Promise<void> {
    confirmEnd = null;
    hostErrors = mapWithout(hostErrors, alias);
    try {
      await endHostSessions(alias);
    } catch (e) {
      hostErrors = new Map(hostErrors).set(alias, e instanceof Error ? e.message : String(e));
    }
  }

  /** Shut a host down: end all sessions AND stop its daemon, drop the tunnel. */
  async function shutdown(alias: string): Promise<void> {
    confirmShutdown = null;
    hostErrors = mapWithout(hostErrors, alias);
    try {
      await shutdownHost(alias);
      remoteWs = mapWithout(remoteWs, alias);
    } catch (e) {
      hostErrors = new Map(hostErrors).set(alias, e instanceof Error ? e.message : String(e));
    }
    void refreshHosts();
  }

  // Cluster rows read their overview while the host list is on screen: at
  // most once a minute per host (the shell's own floor too), paused while
  // hidden — the effect re-runs on return, and the floor absorbs a quick
  // hide/show. The cluster page, when open, polls for itself.
  $effect(() => {
    if (!native || ownAlias !== null || clusterView !== null || !$pageVisible) return;
    const aliases = hosts.filter(clusterReadable).map((h) => h.alias);
    if (aliases.length === 0) return;
    const ask = (): void => {
      for (const alias of aliases) void clusterOverviews.refresh(alias, 55_000);
    };
    ask();
    const t = setInterval(ask, 60_000);
    return () => clearInterval(t);
  });

  async function submitAdd(): Promise<void> {
    const alias = addAlias.trim();
    if (alias === "") return;
    addError = null;
    try {
      await addHost(alias);
      addAlias = "";
      addOpen = false;
      await refreshHosts();
      void connect(alias);
    } catch (e) {
      addError = e instanceof Error ? e.message : String(e);
    }
  }

  async function updateLocal(): Promise<void> {
    localError = null;
    localUpdating = true;
    try {
      await updateLocalDaemon();
      // Success: the shell broadcasts local-daemon-updated and this window
      // re-homes itself to the fresh daemon (App-level listener).
    } catch (e) {
      localError = e instanceof Error ? e.message : String(e);
      localUpdating = false;
    }
  }

  /** The source part of a build id ("ff52221-dirty.1783438290" → "ff52221-dirty"). */
  function shortBuild(build: string | null): string {
    if (build === null) return "an old build";
    const dot = build.lastIndexOf(".");
    return dot === -1 ? build : build.slice(0, dot);
  }

  /**
   * Plain-English tooltip for the outdated-daemon note. The raw build id
   * ("a9cdd60-dirty") is developer shorthand — surface it on hover, but spell
   * out what it means so the visible line reads clearly to anyone.
   */
  function buildNote(build: string | null): string {
    const dirty = build?.includes("-dirty")
      ? ' The "-dirty" tag means it was compiled from a working tree with uncommitted changes.'
      : "";
    return `Running daemon build: ${shortBuild(build)}.${dirty} Updating restarts the daemon on a matching current build; its live sessions end first.`;
  }

  /** What clicking "update" ends, spelled out next to the action. */
  function endsLabel(n: number | null): string {
    if (n === null) return " (session count unknown)";
    if (n === 0) return "";
    return ` (ends ${n} session${n === 1 ? "" : "s"})`;
  }

  /** The note beside the LOCAL daemon update button. A stateful restart brings
   *  every session back, so idle ones aren't a warning — only genuinely BUSY
   *  work (a command running, an agent mid-turn) is interrupted. */
  function updateNote(busy: number): string {
    return busy === 0 ? "" : ` (${busy} busy will restart)`;
  }

  function mapWithout<K, V>(map: Map<K, V>, key: K): Map<K, V> {
    const next = new Map(map);
    next.delete(key);
    return next;
  }

  /** "just now" · "5m ago" · "3h ago" · "4d ago" · "2026-05-12". */
  function ago(unixSecs: number | null | undefined): string {
    if (!unixSecs) return "";
    const secs = Math.max(0, Math.floor(Date.now() / 1000) - unixSecs);
    if (secs < 60) return "just now";
    if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
    if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`;
    if (secs < 14 * 86400) return `${Math.floor(secs / 86400)}d ago`;
    return new Date(unixSecs * 1000).toISOString().slice(0, 10);
  }

  /** A login node's first label — "login-a" for "login-a.cluster.example". */
  function shortNode(node: string): string {
    return node.split(".")[0] || node;
  }

  /** Shorten an absolute path with ~ for scanability. */
  function tildify(path: string): string {
    const m = path.match(/^\/(?:home|Users)\/[^/]+(\/.*)?$/);
    return m ? `~${m[1] ?? ""}` : path;
  }

  let copyingProject = $state<string | null>(null);
  let copyError = $state<string | null>(null);
  async function openRow(e: Pick<MouseEvent, "metaKey" | "ctrlKey">, w: Workspace): Promise<void> {
    if (copyingProject !== null) return;
    let target = w;
    if (w.local_copy !== undefined && ownAlias === null && native) {
      copyingProject = w.id; copyError = null;
      try {
        const copied = await proOpenCloudProject(w.id);
        if (copied === null) return;
        target = { ...w, id: copied.workspace_id, root: copied.root, name: copied.name, local_copy: copied.local_copy };
      } catch (reason) {
        copyError = await import("../pro/projectCopy").then(({ projectCopyError }) => projectCopyError(reason), () => "The local copy couldn’t refresh. Try Open again.");
        return;
      }
      finally { copyingProject = null; }
    }
    if ((e.metaKey || e.ctrlKey) && !jobScoped) {
      await openWindow(ownAlias, target.id, true);
    } else {
      onOpen(target);
    }
  }

  async function backToHome(): Promise<void> {
    const legacyWindow = !isHomeHub();
    try {
      await navigateHome(null);
    } catch (error) {
      // Compatibility for a host-detail window persisted by an older build:
      // return to the singleton hub, then retire that legacy extra window.
      // An actual hub must remain open on transient navigation failures:
      // openWindow would focus that same window and the close below would
      // otherwise immediately destroy the successfully restored Home.
      if (
        !legacyWindow ||
        !String(error).includes("this window is not the Home navigation hub")
      ) {
        if (ownAlias !== null) {
          hostErrors = new Map(hostErrors).set(ownAlias, String(error));
        }
        return;
      }
      await openWindow(null, null);
      closeThisWindow();
    }
  }
</script>

{#snippet directPreference(host: HostState)}
  {#if host.direct_ssh !== undefined && ($paidPlan !== null || host.direct_ssh === true)}
    <details class="host-advanced">
      <summary>Advanced</summary>
      <label><input type="checkbox" checked={host.direct_ssh} disabled={savingDirect.has(host.alias)} onchange={event => void setDirect(host, event.currentTarget)} />Connect directly from this computer</label>
      <p>Uses this computer’s SSH settings on the next connection. Existing connections stay as they are until you reconnect. Running jobs and other computers are unaffected.</p>
    </details>
  {/if}
{/snippet}

{#snippet jobsRow(alias: string)}
  <div class="rowwrap" role="presentation">
    <button
      class="row sub jobs"
      title="Start Slurm jobs on {alias} and open workspaces inside them"
      onclick={() => showCluster(alias)}
    >
      <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
        <rect x="2" y="2" width="5" height="5" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.4" />
        <rect x="9" y="2" width="5" height="5" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.4" />
        <rect x="2" y="9" width="5" height="5" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.4" />
        <rect x="9" y="9" width="5" height="5" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.4" />
      </svg>
      <span class="name">Jobs</span>
    </button>
  </div>
{/snippet}

<div class="home">
  {#if health !== null}
    <!-- The mark identifies the DAEMON serving this window (the daemon
         outlives app reinstalls by design, so this is the version that
         actually matters — and a dev daemon must say so instead of posing
         as an ordinary "v0.0.1"). A click is an explicit update check: the
         toast answers it, "up to date" or "development build" included. -->
    <button
      class="version-mark"
      class:newer={stampNewer !== null}
      title={stampTitle}
      onpointerenter={() => (stampNow = Date.now())}
      onclick={() => void checkForUpdates(true)}
    >
      {#if health.version === "0.0.1"}daemon dev·{(health.build ?? "unknown").split(".")[0]}{:else}v{health.version}{/if}{#if stampNewer !== null}<span class="newer-tag"
          >{` · ${stampNewer} available`}</span
        >{/if}
    </button>
  {/if}
  <HomeNavigation active="workspaces" plan={$paidPlan} showPro={isBrowserGateway() || (native && $proOffered === true)}
    onHome={() => {
      if (showBackToHome) void backToHome();
      else clusterView = null;
    }} {onPro} {onSettings} />
  {#if clusterView !== null}
    {@const alias = clusterView}
    <div class="cluster-surface">
    {#if ClusterPageView !== null}
    <ClusterPageView
      {alias}
      host={hosts.find((h) => h.alias === alias) ?? null}
      onBack={clusterWs !== null ? () => void backToHome() : () => (clusterView = null)}
      here={clusterWs}
      onHere={() => {
        const own = workspaces.find((w) => w.id === clusterWs);
        if (own !== undefined) onOpen(own);
      }}
      onHostState={(state) => {
        hosts = hosts.map((h) => (h.alias === state.alias ? state : h));
      }}
      onHostsChanged={() => void refreshHosts()}
      onNotCluster={clusterWs === null
        ? (state) => {
            hosts = hosts.map((h) => (h.alias === state.alias ? state : h));
            clusterView = null;
            void connect(state.alias);
          }
        : undefined}
    />
    {/if}
    </div>
  {:else}
  <div class="inner">
    <header class="masthead">
      <div class="masthead-leading">
        {#if showBackToHome}
          <button class="back-home" aria-label="Back to Home" title="Back to Home" onclick={() => void backToHome()}>
            <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
              <path
                d="M10.5 3.5 6 8l4.5 4.5M6.5 8H14"
                fill="none"
                stroke="currentColor"
                stroke-width="1.4"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
            <span>Home</span>
          </button>
        {/if}
        <div class="welcome">
          <!-- A project view follows its project: name where it runs now. -->
          <h1>{ownAlias === null ? "Workspaces" : gatewayWorkspace() !== null ? projectWhereLabel($projectWhere) : hostLabel}</h1>
          <p>{ownAlias === null ? "Pick up where you left off." : "Workspaces and sessions on this machine."}</p>
        </div>
      </div>
      <button class="cta open-folder" onclick={onOpenFolder}>
        <svg viewBox="0 0 20 20" width="17" height="17" aria-hidden="true"><path d="M2.5 5.5h5l2 2h8v9h-15zM2.5 5.5v-2h5l2 2h6v2" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" /></svg>
        Open folder <kbd>{keyHint("picker")}</kbd>
      </button>
      {#if native && hostLabel === "local" && localState?.outdated}
        <div class="update-line" title={buildNote(localState.build)}>
          <span>daemon is an older build —</span>
          <button class="update-act" disabled={localUpdating} onclick={() => void updateLocal()}>
            {localUpdating ? "updating…" : `update${updateNote(busyNow)}`}
          </button>
        </div>
        {#if localError !== null}
          <div class="err-line masthead-err">{localError}</div>
        {/if}
      {/if}
      {#if native && hostLabel === "local" && appUpdate !== null}
        <div class="update-line">
          <span>Chimaera {appUpdate} available —</span>
          <button class="update-act" disabled={appUpdating} onclick={() => void installApp()}>
            {appUpdating ? "updating…" : "update & restart"}
          </button>
        </div>
        {#if appUpdateError !== null}
          <div class="err-line masthead-err">{appUpdateError}</div>
        {/if}
      {/if}
    </header>

    {#if $computeStatus?.self}
      <!-- This whole daemon lives inside a Slurm allocation: say so before
           anything else — node, partition, job, resources, and the live
           walltime countdown (the maintainer: "we need to be MUCH clearer
           that it is a compute node"). -->
      <ComputeBanner self={$computeStatus.self} receivedAt={$computeStatus.received_at_ms} />
    {/if}

    <section class="workspaces" aria-label="Workspaces on this machine">
      <div class="sec-head">
        <h2 class="sec-title">{ownAlias === null ? (native && isMac ? "This Mac" : "This computer") : "On this machine"}</h2>
        <div class="where" title={health?.hostname}>
          {#if health !== null}<span class="hostname">{health.hostname}</span>{/if}
          <span class="remote-status" class:online={daemonReachable} role="status">
            <span class="daemon-dot" class:ok={daemonReachable} aria-hidden="true"></span>
            {daemonReachable ? "Online" : "Offline"}
          </span>
        </div>
      </div>
      {#if copyError}<p class="err-line" role="alert">{copyError}</p>{/if}
      {#if copyingProject !== null}<p class="hint" role="status">Updating the local project copy…</p>{/if}
      {#if !daemonReachable}<p class="offline-note" role="status">Connection interrupted. Your workspaces will reconnect when this machine is available.</p>{/if}
      {#if sorted.length === 0}
        <div class="blank">
          <h3>A folder is your workspace.</h3>
          <p>Open a folder to start terminals and agents together. Your files stay where they are.</p>
          <button class="cta" onclick={onOpenFolder}>Open folder</button>
        </div>
      {:else}
        <div class="rows">
          {#each sorted as w (w.id)}
            {@const live = liveByWs.get(w.id)}
            {@const placeHint = placeHints.get(w.id)}
            {@const wsState = !daemonReachable ? "" : live && live.attn > 0 ? "attn" : live && live.live > 0 ? "alive" : ""}
            {#if confirmStopId === w.id}
              <div class="row confirm" role="alertdialog" aria-label="end sessions?">
                <span class="name">{w.name}</span>
                <span class="confirm-label"
                  >end {live?.live} running session{live?.live === 1 ? "" : "s"}?</span
                >
                <button
                  class="confirm-yes"
                  onclick={() => {
                    confirmStopId = null;
                    onStop(w);
                  }}>End sessions</button
                >
                <button class="confirm-no" onclick={() => (confirmStopId = null)}>cancel</button>
              </div>
            {:else if confirmRemoveId === w.id}
              <div class="row confirm" role="alertdialog" aria-label="remove workspace?">
                <span class="name">{w.name}</span>
                <span class="confirm-label">remove from this list?</span>
                <button
                  class="confirm-yes"
                  onclick={() => {
                    confirmRemoveId = null;
                    onRemove(w);
                  }}>remove</button
                >
                <button class="confirm-no" onclick={() => (confirmRemoveId = null)}>cancel</button>
              </div>
            {:else}
              <div class="rowwrap workspace-row" role="presentation" class:live={wsState === "alive"} class:attn={wsState === "attn"}>
                <button class="row" title={w.root} disabled={copyingProject !== null} onclick={(e) => void openRow(e, w)}>
                  <span
                    class="dot {wsState}"
                    title={!daemonReachable ? "Last known session state — this machine is offline" : wsState === "attn"
                      ? `${live?.attn} awaiting approval`
                      : wsState === "alive"
                        ? `${live?.live} live session${live?.live === 1 ? "" : "s"}`
                        : "no live sessions"}
                  ></span>
                  <span class="workspace-label"><span class="name">{w.name}</span><span class="path">{tildify(w.root)}</span></span>
                  <span class="workspace-meta">
                    {#if live !== undefined && live.attn > 0}
                      <span class="session-state" class:attention={daemonReachable} class:stale={!daemonReachable}>{live.attn} {daemonReachable ? "awaiting approval" : `approval${live.attn === 1 ? "" : "s"} last seen`}</span>
                    {:else if live !== undefined && live.live > 0}
                      <span class="session-state" class:stale={!daemonReachable}>{live.live} {daemonReachable ? "live " : ""}session{live.live === 1 ? "" : "s"}{daemonReachable ? "" : " last seen"}</span>
                    {/if}
                    <span class="when">{#if placeHint !== undefined}{placeHint} · {/if}{ago(w.last_opened_at)}</span>
                  </span>
                </button>
                {#if live !== undefined && live.live > 0}
                  <button
                    class="side stop shown"
                    title="end this workspace's {live.live} running session{live.live === 1
                      ? ''
                      : 's'}"
                    onclick={() => (confirmStopId = w.id)}>End sessions</button
                  >
                {/if}
                <HomeActions label={`Actions for ${w.name}`}>
                  {#if !jobScoped}
                    <button
                      class="side"
                      title="open in a new window"
                      disabled={copyingProject !== null} onclick={() => void openRow({ metaKey: true, ctrlKey: false }, w)}>Open in new window</button
                    >
                  {/if}
                  <button
                    class="side x"
                    title="remove from this list (folder untouched)"
                    onclick={() => (confirmRemoveId = w.id)}>Remove from list</button
                  >
                </HomeActions>
              </div>
            {/if}
          {/each}
        </div>
      {/if}
    </section>

    {#if native && ownAlias === null && $paidPlan !== null}
      <!-- The cloud projects list (and the Pro presentation copy behind it)
           loads only for a paid plan: it stays out of the always-loaded
           entry, whose budget the shell is close to. -->
      {#await import("../pro/CloudProjects.svelte") then { default: CloudProjects }}
        <CloudProjects onOpen={onOpen} knownIds={workspaces.map(workspace => workspace.id)} />
      {/await}
    {/if}

    {#if ownAlias === null}
      <section class="remotes" aria-label="Remote machines">
        <div class="sec-head">
          <h2 class="sec-title">Remote machines</h2>
          {#if native}
            <button
              class="ghost"
              onclick={() => {
                addOpen = !addOpen;
                addError = null;
              }}>Add machine</button
            >
          {/if}
        </div>

        {#if !native}
          <p class="hint">
            Remote hosts connect from the chimaera app — or run
            <code>chimaera connect &lt;host&gt;</code> in a terminal and open the printed URL.
          </p>
        {:else}
          {#if addOpen}
            <form
              class="add"
              onsubmit={(e) => {
                e.preventDefault();
                void submitAdd();
              }}
            >
              <!-- svelte-ignore a11y_autofocus -->
              <input
                class="add-input"
                bind:value={addAlias}
                aria-label="SSH alias or user at host"
                placeholder="SSH alias or user@host"
                spellcheck="false"
                autocomplete="off"
                autofocus
                onkeydown={(e) => {
                  if (e.key === "Escape") {
                    e.preventDefault();
                    addOpen = false;
                  }
                }}
              />
              <button class="cta small" type="submit" disabled={addAlias.trim() === ""}
                >Connect</button
              >
            </form>
            {#if addError !== null}
              <div class="err-line">{addError}</div>
            {/if}
          {/if}

          {#if hosts.length === 0 && !addOpen}
            <p class="hint">
              No remotes yet. Add a server's or a cluster's ssh alias — chimaera installs itself in
              <code>~/.chimaera{localState?.dev_build ? "-dev" : ""}</code> over ssh, no root needed. On a
              cluster it runs only inside your jobs, never on the login node.
            </p>
          {:else}
            <div class="rows">
              {#each hosts as h (h.alias)}
                {@const phase = phases.get(h.alias)}
                {@const err = hostErrors.get(h.alias)}
                {@const ws = remoteWs.get(h.alias)}
                {@const cluster = isCluster(h)}
                {@const loginServe = h.cluster?.login_serve === true}
                <div class="host-card">
                {#if confirmShutdown === h.alias}
                  <div class="row confirm strong" role="alertdialog" aria-label="shut down host?">
                    <span class="name">{h.alias}</span>
                    <span class="confirm-label"
                      >shut down — end {h.live_sessions ?? 0} session{h.live_sessions === 1
                        ? ""
                        : "s"} and stop the daemon?</span
                    >
                    <button class="confirm-yes" onclick={() => void shutdown(h.alias)}>Shut down</button
                    >
                    <button class="confirm-no" onclick={() => (confirmShutdown = null)}>cancel</button>
                  </div>
                {:else if confirmEnd === h.alias}
                  <div class="row confirm" role="alertdialog" aria-label="end sessions?">
                    <span class="name">{h.alias}</span>
                    <span class="confirm-label"
                      >end {h.live_sessions ?? 0} running session{h.live_sessions === 1
                        ? ""
                        : "s"}? (the daemon keeps running)</span
                    >
                    <button class="confirm-yes" onclick={() => void endSessions(h.alias)}
                      >End sessions</button
                    >
                    <button class="confirm-no" onclick={() => (confirmEnd = null)}>cancel</button>
                  </div>
                {:else if confirmForget === h.alias}
                  <div class="row confirm" role="alertdialog" aria-label="forget host?">
                    <span class="name">{h.alias}</span>
                    <span class="confirm-label">forget this host?</span>
                    <button class="confirm-yes" onclick={() => void forget(h.alias)}>forget</button>
                    <button class="confirm-no" onclick={() => (confirmForget = null)}>cancel</button>
                  </div>
                {:else if cluster && !loginServe}
                  <!-- A cluster: chimaera never runs on its login node, so
                       there is nothing to tunnel to — the row reads the
                       cluster's workspaces and opens its page in place. -->
                  {@const line = clusterReadable(h) ? clusterLine(h) : null}
                  {@const running = clusterRunning(h.alias)}
                  <div class="rowwrap" role="presentation" class:connected={running}>
                    <button
                      class="row"
                      title="{h.alias}'s jobs and workspaces"
                      disabled={h.status === "connecting"}
                      onclick={() => void connect(h.alias)}
                    >
                      <span
                        class="dot {h.status === 'connecting' ? 'starting' : running ? 'alive' : ''}"
                        title={h.status === "connecting"
                          ? "connecting…"
                          : running
                            ? "a job is running"
                            : "no job running"}
                      ></span>
                      <span class="name">{h.alias}</span>
                      <span
                        class="pill-sched"
                        title="{h.alias} has a batch scheduler: Chimaera runs inside jobs you start there, never on the login node"
                        >{schedulerLabel(h.cluster?.scheduler)}</span
                      >
                      {#if localState?.dev_build}
                        <span
                          class="pill-dev"
                          title="dev build — every connection targets this machine's own build in ~/.chimaera-dev on {h.alias}; the real daemon there is untouched"
                          >dev</span
                        >
                      {/if}
                      {#if phase !== undefined}
                        <span class="phase">{phase === PHASE_LABEL.probing ? "connecting…" : phase}</span>
                      {:else if line !== null}
                        <span class="phase quiet">{line}</span>
                      {:else}
                        <span class="when">{ago(h.last_connected_at)}</span>
                      {/if}
                    </button>
                    {#if h.direct_ssh !== undefined && ($paidPlan !== null || h.direct_ssh === true)}
                      <HomeActions label={`Actions for ${h.alias}`}>
                        <button class="side x" title="forget host" onclick={() => (confirmForget = h.alias)}>Forget machine</button>
                        {@render directPreference(h)}
                      </HomeActions>
                    {:else}
                      <button class="side x" title="forget host" onclick={() => (confirmForget = h.alias)}>&times;</button>
                    {/if}
                  </div>
                  {#if err !== undefined}
                    <div class="err-line">{err}</div>
                  {/if}
                  {#if h.cluster !== null && h.cluster.login_daemon !== null && phase === undefined}
                    <div class="note-line warn">
                      a chimaera server from before is still running on {shortNode(h.cluster.login_daemon.node)} —
                      <button class="update-act" onclick={() => void connect(h.alias)}>open to shut it down</button>
                    </div>
                  {/if}
                {:else}
                  <div class="rowwrap host-row" role="presentation" class:connected={h.status === "connected"}>
                    <button
                      class="row"
                      title={h.status === "connected"
                        ? `browse ${h.alias}`
                        : `connect to ${h.alias}`}
                      disabled={h.status === "connecting"}
                      onclick={() =>
                        h.status === "connected"
                          ? void navigateHost(h.alias)
                          : void connect(h.alias)}
                    >
                      <span
                        class="dot {h.status === 'connected'
                          ? 'alive'
                          : h.status === 'connecting'
                            ? 'starting'
                            : ''}"
                        title={h.status === "connected"
                          ? "connected"
                          : h.status === "connecting"
                            ? "connecting…"
                            : "not connected"}
                      ></span>
                      <span class="workspace-label">
                        <span class="host-name"><span class="name">{h.alias}</span>{#if h.via_pro}<span class="via-pro" title="Connected through Chimaera Pro">via Pro</span>{/if}{#if cluster}<span class="pill-sched">{schedulerLabel(h.cluster?.scheduler)}</span><span class="pill-dev" title="chimaera runs on {h.alias}'s login node (turned on in the cluster page's … menu)">login node</span>{/if}{#if localState?.dev_build}<span
                          class="pill-dev"
                          title="dev build — every connection targets this machine's own build in ~/.chimaera-dev on {h.alias}; the real daemon there is untouched"
                          >dev</span
                        >{/if}</span>
                        <span
                          class="phase quiet"
                          title={phase === undefined && h.status === "connected" && h.node
                            ? `${h.alias} spans several login nodes; its daemon runs on ${h.node}, so this connection is pinned there`
                            : undefined}
                        >
                          {#if phase !== undefined}{phase}
                          {:else if h.status === "connected"}Connected{#if h.node} · {shortNode(h.node)}{/if}{#if h.local_port !== null} · 127.0.0.1:{h.local_port}{/if}{#if (h.live_sessions ?? 0) > 0} · {h.live_sessions} live session{h.live_sessions === 1 ? "" : "s"}{/if}
                          {:else if h.status === "connecting"}Connecting…
                          {:else}Not connected{#if h.last_connected_at} · Last connected {ago(h.last_connected_at)}{/if}{/if}
                        </span>
                      </span>
                      <span class="host-open">{h.status === "connected" ? "Open" : h.status === "connecting" ? "" : "Connect"}<span aria-hidden="true"> →</span></span>
                    </button>
                    <HomeActions label={`Actions for ${h.alias}`}>
                      {#if h.status === "connected"}
                        {#if (h.live_sessions ?? 0) > 0}
                          <button
                            class="side"
                            title="end all sessions on {h.alias} — the daemon keeps running"
                            onclick={() => (confirmEnd = h.alias)}>End sessions</button
                          >
                        {/if}
                        <button
                          class="side"
                          title="close the tunnel — sessions keep running on {h.alias}"
                          onclick={() => void disconnect(h.alias)}>Disconnect</button
                        >
                        <button
                          class="side stop"
                          title="shut down {h.alias} — end all sessions and stop the daemon"
                          onclick={() => (confirmShutdown = h.alias)}>Shut down</button
                        >
                      {/if}
                      <button class="side x" title="forget host" onclick={() => (confirmForget = h.alias)}
                        >Forget machine</button
                      >
                      {#if h.not_cluster}
                        <button
                          class="side"
                          title="you said {h.alias} isn't a cluster — treat it as one again (Chimaera then runs only inside jobs there)"
                          onclick={() => void backToCluster(h.alias)}>it's a cluster</button
                        >
                      {/if}
                      {@render directPreference(h)}
                    </HomeActions>
                  </div>
                  {#if err !== undefined}
                    <div class="err-line">{err}</div>
                  {/if}
                  {#if h.status === "connected" && h.outdated && phase === undefined}
                    <div class="note-line" title={buildNote(h.remote_build)}>
                      daemon is an older build —
                      <button class="update-act" onclick={() => void connect(h.alias, true)}>
                        update{endsLabel(h.live_sessions)}
                      </button>
                    </div>
                  {/if}
                  {#if h.status === "connected" && ws !== undefined}
                    <div class="remote-ws">
                      {#each ws as rw (rw.id)}
                        <div class="rowwrap" role="presentation">
                          <button
                            class="row sub"
                            title={rw.root}
                            onclick={() => void navigateHost(h.alias, rw.id)}
                          >
                            <span class="name">{rw.name}</span>
                            <span class="path">{tildify(rw.root)}</span>
                            <span class="when">{ago(rw.last_opened_at)}</span>
                          </button>
                        </div>
                      {/each}
                      {#if cluster}{@render jobsRow(h.alias)}{/if}
                      <div class="rowwrap" role="presentation">
                        <button class="row sub browse" onclick={() => void navigateHost(h.alias)}>
                          <span class="name">Open {h.alias}</span><span aria-hidden="true">→</span>
                        </button>
                      </div>
                    </div>
                  {:else if cluster}
                    <!-- The login-node override is on, so the row connects to
                         the login daemon as before; the cluster page (jobs)
                         stays one click away. -->
                    <div class="remote-ws">{@render jobsRow(h.alias)}</div>
                  {/if}
                {/if}
                </div>
              {/each}
            </div>
          {/if}
        {/if}
      </section>
    {/if}
  </div>
  {/if}

</div>

<style>
  .host-advanced { max-width: 320px; padding: 8px 10px; color: var(--fg); font-size: var(--text-sm); }
  .host-advanced summary { cursor: pointer; color: var(--muted); }
  .host-advanced label { display: flex; align-items: flex-start; gap: 8px; margin-top: 10px; }
  .host-advanced input { margin-top: 3px; flex: none; }
  .host-advanced p { white-space: normal; color: var(--muted); font-size: var(--text-xs); line-height: 1.5; margin: 8px 0 0; }
  .via-pro {
    flex: none;
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
  }

  .home { position: absolute; inset: 0; display: flex; overflow: hidden; background: var(--bg); }
  .cluster-surface { flex: 1; min-width: 0; min-height: 0; overflow-y: auto; }
  .inner { flex: 1; min-width: 0; overflow-y: auto; padding: 56px clamp(24px, 4vw, 64px) 24px; display: flex; flex-direction: column; gap: 36px; }
  .inner > :global(*) { width: 100%; max-width: 860px; margin-left: auto; margin-right: auto; box-sizing: border-box; }
  .masthead { display: flex; align-items: center; justify-content: space-between; gap: 20px; flex-wrap: wrap; margin-bottom: 8px; }
  .masthead-leading { display: flex; flex-direction: column; align-items: flex-start; gap: 18px; min-width: 0; }
  .welcome p { margin: 8px 0 0; color: var(--muted); font-size: var(--text-md); }
  .open-folder { display: inline-flex; align-items: center; gap: 9px; white-space: nowrap; }
  .open-folder kbd { margin-left: 8px; }
  .back-home {
    appearance: none;
    border: none;
    background: none;
    color: var(--muted);
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-sm);
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 5px 7px 5px 4px;
    border-radius: 5px;
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }

  .back-home:hover {
    color: var(--fg);
    background: var(--row-hover);
  }

  /* Quiet build-parity note: a second masthead row, right-aligned under
     the host label, mono + muted like the rest of the meta text. */
  .update-line {
    flex-basis: 100%;
    display: flex;
    justify-content: flex-end;
    align-items: baseline;
    gap: 5px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .update-act {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--warn);
    cursor: pointer;
    padding: 0 2px;
    border-radius: 3px;
  }

  .update-act:hover {
    text-decoration: underline;
  }

  .update-act:disabled {
    opacity: 0.6;
    cursor: default;
    text-decoration: none;
  }

  .masthead-err {
    flex-basis: 100%;
    text-align: right;
    padding: 0;
  }

  /* Same quiet register for the outdated-daemon note under a host row. */
  .note-line {
    padding: 0 10px 6px 27px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
  }

  /* Quiet running-version stamp, pinned to the home screen's corner — a
     button (a click checks for updates) reset to plain text. */
  .version-mark {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    cursor: pointer;
    position: fixed;
    bottom: 12px;
    right: 16px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    letter-spacing: 0.01em;
    opacity: 0.5;
    user-select: none;
    transition: opacity 0.15s ease;
  }

  .version-mark:hover {
    opacity: 0.9;
  }

  /* A known newer release lifts the stamp out of its whisper. */
  .version-mark.newer {
    opacity: 0.85;
  }

  .newer-tag {
    color: var(--accent);
  }

  h1 {
    margin: 0;
    font-size: clamp(24px, 3vw, 30px);
    font-weight: 550;
    letter-spacing: -0.035em;
  }

  .where {
    display: flex;
    align-items: center;
    gap: 7px;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--muted);
    min-width: 0;
  }

  .daemon-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--muted);
    opacity: 0.5;
    flex: none;
  }

  .daemon-dot.ok {
    background: var(--accent);
    opacity: 1;
  }

  .remote-status {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 2px 7px;
    border: 1px solid color-mix(in srgb, var(--muted) 26%, transparent);
    border-radius: 999px;
    color: var(--muted);
    font-size: var(--text-xs);
    white-space: nowrap;
  }

  .remote-status.online {
    border-color: color-mix(in srgb, var(--accent) 35%, transparent);
    color: var(--accent);
  }



  .hostname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    opacity: 0.7;
  }

  section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .sec-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 0 0 10px;
  }

  .sec-title {
    margin: 0;
    font-size: var(--text-md);
    font-weight: 550;
    color: var(--fg);
    letter-spacing: -0.01em;
  }

  .ghost {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 2px 4px;
    border-radius: 4px;
  }

  .ghost:hover {
    color: var(--fg);
  }

  kbd {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 3px;
    padding: 0 3px;
    margin-left: 2px;
  }

  .blank {
    border: 1px solid var(--edge);
    border-radius: 10px;
    padding: 44px 24px;
    text-align: center;
    color: var(--muted);
    font-size: var(--text-md);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 14px;
  }

  .blank p {
    margin: 0;
    max-width: 40ch;
  }

  .cta {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-md);
    padding: 7px 14px;
    border-radius: 6px;
    cursor: pointer;
    transition: border-color 0.12s ease;
  }

  .cta:hover {
    border-color: var(--accent);
  }

  .cta:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .cta.small {
    padding: 5px 12px;
    font-size: var(--text-sm);
  }

  .rows {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }

  .rowwrap {
    display: flex;
    align-items: center;
    border-radius: 6px;
    transition: background-color 0.12s ease;
  }

  .rowwrap:hover {
    background: var(--row-hover);
  }

  /* Running workspaces and connected hosts carry a faint accent wash so they
     cluster apart from the dormant rows; attention pulls toward amber. Hover
     still wins so the row reacts under the cursor. */
  .rowwrap.live,
  .rowwrap.connected {
    background: color-mix(in srgb, var(--accent) 6%, transparent);
  }

  .rowwrap.attn {
    background: color-mix(in srgb, var(--warn) 8%, transparent);
  }

  .rowwrap.live:hover,
  .rowwrap.connected:hover,
  .rowwrap.attn:hover {
    background: var(--row-hover);
  }

  .row {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    color: var(--fg);
    text-align: left;
    padding: 9px 10px;
    cursor: pointer;
    border-radius: 6px;
  }

  .row:disabled {
    cursor: progress;
  }

  .row.sub {
    padding: 6px 10px;
  }

  .row.browse .name {
    color: var(--muted);
  }

  .row.browse:hover .name {
    color: var(--fg);
  }

  .name {
    flex: none;
    max-width: 45%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-md);
  }

  .path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .phase {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg);
  }

  .phase.quiet {
    color: var(--muted);
    font-family: var(--mono);
    font-size: var(--text-xs);
  }

  .badge {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 999px;
    padding: 1px 8px 1px 6px;
  }

  .session-state.attention {
    color: var(--warn);
  }

  /* Session/host state dot. This is the home screen's at-a-glance liveness
     signal — a dormant workspace reads muted, live work glows in the accent,
     an agent that needs you glows amber, and a connecting host pulses. */
  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--muted);
    opacity: 0.4;
  }

  .dot.alive {
    background: var(--accent);
    opacity: 1;
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 16%, transparent);
  }

  .dot.attn {
    background: var(--warn);
    opacity: 1;
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--warn) 16%, transparent);
  }

  .dot.starting {
    background: var(--muted);
    opacity: 1;
    animation: dotpulse 1.1s ease-in-out infinite;
  }

  @keyframes dotpulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.3;
    }
  }

  /* Hidden document: pause the presence dots — nobody sees them breathe
     (the html.app-hidden contract; see app.css). */
  :global(html.app-hidden) .dot.starting {
    animation-play-state: paused;
  }


  .when {
    flex: none;
    margin-left: auto;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    opacity: 0.8;
  }

  .side {
    flex: none;
    visibility: hidden;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    padding: 0.2rem 8px;
    cursor: pointer;
  }

  .side:hover {
    color: var(--fg);
  }

  .side.x:hover {
    color: var(--err);
  }

  .side.stop:hover {
    color: var(--err);
  }

  .rowwrap:hover .side {
    visibility: visible;
  }

  /* The stop control stays visible for a running workspace (not hover-gated
     like the others) — ending live work should never be a hidden gesture. */
  .side.shown {
    visibility: visible;
    color: var(--warn);
  }

  .side.shown:hover {
    color: var(--err);
  }

  .remote-ws {
    margin: 0 0 6px 22px;
    padding-left: 8px;
    border-left: 1px solid var(--edge);
    display: flex;
    flex-direction: column;
  }

  /* A login_serve cluster's entry into its cluster page: quiet like the
     browse row, waking on hover. */
  .row.jobs svg {
    flex: none;
    color: var(--muted);
    opacity: 0.7;
  }

  .row.jobs .name {
    color: var(--muted);
  }

  .row.jobs:hover .name {
    color: var(--fg);
  }

  /* The scheduler tag on a cluster row — a fact, quietly stated. */
  .pill-sched {
    flex: none;
    font-size: var(--text-xs);
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 999px;
    padding: 1px 7px;
    white-space: nowrap;
  }

  .note-line.warn {
    color: var(--warn);
  }

  .row.confirm {
    cursor: default;
  }

  .confirm {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 10px;
    border-radius: 6px;
    background: var(--row-active);
  }

  /* The most final action (shut down a host) reads in the danger tone. */
  .confirm.strong {
    background: color-mix(in srgb, var(--err) 11%, var(--row-active));
    box-shadow: inset 2px 0 0 var(--err);
  }

  .confirm-label {
    flex: 1;
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .confirm-yes,
  .confirm-no {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
    padding: 2px 8px;
    border-radius: 4px;
  }

  .confirm-yes {
    color: var(--err);
  }

  .confirm-yes:hover {
    background: color-mix(in srgb, var(--err) 12%, transparent);
  }

  .confirm-no {
    color: var(--muted);
  }

  .confirm-no:hover {
    color: var(--fg);
  }

  .hint {
    margin: 0;
    padding: 4px 10px;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.55;
  }

  .hint code {
    font-family: var(--mono);
    font-size: var(--text-xs);
    border: 1px solid var(--edge);
    border-radius: 4px;
    padding: 0 4px;
  }

  .add {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 2px 10px 8px;
  }

  .add-input {
    flex: 1;
    min-width: 0;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--overlay-bg);
    color: var(--fg);
    font-family: var(--mono);
    font-size: var(--text-sm);
    padding: 6px 10px;
    outline: none;
  }

  .add-input:focus {
    border-color: var(--focus-ring);
  }

  .add-input::placeholder {
    color: var(--muted);
    opacity: 0.7;
  }

  /* Dev-build language: amber, the "this is special" register — a dev build
     talks only to isolated ~/.chimaera-dev daemons, and its host rows must
     never read like a release's. */
  .pill-dev {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--warn);
    border: 1px solid color-mix(in srgb, var(--warn) 40%, transparent);
    border-radius: 999px;
    padding: 1px 7px;
  }

  .err-line {
    padding: 2px 10px 6px;
    font-size: var(--text-sm);
    color: var(--err);
    white-space: pre-wrap;
  }

  .workspaces .rows { border: 1px solid var(--edge); border-radius: 10px; padding: 5px; }
  .workspace-row, .host-row { padding-right: 8px; }
  .workspace-row .row, .host-row .row { padding: 14px 12px; gap: 14px; }
  .workspace-label { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 5px; }
  .workspace-label .name { max-width: none; font-family: inherit; font-weight: 550; font-size: var(--text-md); }
  .workspace-label .path, .workspace-label .phase { flex: none; font-size: var(--text-xs); }
  .workspace-meta { display: flex; flex-direction: column; align-items: flex-end; gap: 5px; flex: none; }
  .session-state.stale { color: var(--muted); }
  .session-state { font-size: var(--text-xs); color: var(--accent); }
  .workspace-meta .when { font-family: inherit; font-size: var(--text-xs); }
  .host-card { border: 1px solid var(--edge); border-radius: 10px; padding: 5px; }
  .remotes .rows { gap: 10px; }
  .host-name { display: flex; align-items: center; gap: 9px; min-width: 0; }
  .host-name > .name { flex: 1 1 auto; min-width: 0; }
  .host-open { flex: none; font-size: var(--text-sm); color: var(--muted); }
  .host-open span { margin-left: 4px; }
  .host-row.connected, .workspace-row.live { background: transparent; }
  .host-row.connected:hover, .workspace-row.live:hover { background: var(--row-hover); }
  .remote-ws { margin: 0 9px 7px 26px; padding: 6px 0 0 12px; border-color: var(--edge); }
  .remote-ws .name { font-family: inherit; font-size: var(--text-sm); }
  .remote-ws .path, .remote-ws .when { font-size: var(--text-xs); }
  .remote-ws .row.sub { gap: 12px; }
  .remote-ws .row.browse { justify-content: space-between; }
  .blank h3 { margin: 0; color: var(--fg); font-size: var(--text-lg); font-weight: 500; letter-spacing: -.02em; }
  .blank p { line-height: 1.6; }
  .remotes > .hint { padding: 16px 18px; border: 1px solid var(--edge); border-radius: 10px; }
  .offline-note { margin: 0 0 8px; font-size: var(--text-sm); line-height: 1.5; color: var(--muted); }
  .confirm { flex-wrap: wrap; min-height: 58px; }
  .confirm-label { min-width: 120px; line-height: 1.5; }
  .add { padding: 10px 0; flex-wrap: wrap; }
  .add-input { min-width: 160px; min-height: 34px; }
  .side { min-height: 30px; }
  .side.x { font: inherit; font-size: var(--text-sm); }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  @media (max-width: 900px) {
    .inner { padding-left: 24px; padding-right: 24px; }
    .workspace-row .row { flex-wrap: wrap; gap: 10px; }
    .workspace-row .workspace-label { flex-basis: calc(100% - 24px); }
    .workspace-meta { flex-direction: row; margin-left: 17px; align-items: center; flex-wrap: wrap; }
    .workspace-meta .when { margin-left: 0; }
  }
  @media (max-width: 700px) {
    .home { flex-direction: column; }
    .inner { padding: 24px 20px 20px; gap: 30px; }
    .masthead { align-items: flex-start; gap: 20px; }
    .where .hostname { display: none; }
    .open-folder { padding: 9px 12px; }
    .open-folder kbd { display: none; }
    .host-row .row { padding: 12px 9px; gap: 10px; }
    .host-open { font-size: var(--text-xs); }
    .remote-ws { margin-left: 13px; padding-left: 9px; }
    .remote-ws .row.sub { flex-wrap: wrap; gap: 5px 10px; }
    .remote-ws .row.sub .name { max-width: 100%; }
    .remote-ws .path { flex-basis: 100%; order: 1; }
  }
  @media (prefers-reduced-motion: reduce) {
    .dot { animation: none; }
  }
</style>

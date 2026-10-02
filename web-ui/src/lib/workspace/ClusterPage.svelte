<script lang="ts">
  /**
   * A cluster's page, shown in place inside the local home. You start Slurm
   * **jobs** and open **workspaces** inside them (docs/design/hpc-portal-plan.md
   * §4.2): each running or waiting job is a card holding the workspaces open
   * in it; every other workspace is "not open"; an ended job stays as one
   * line until dismissed. Nothing here runs on the login node: every action
   * is one short command the app runs over ssh, or a request to the job's
   * own job-host, while it is open.
   *
   * Polling: the overview on mount, every 60 s while visible, right after an
   * action, and on the shell's `cluster-changed`. The shell asks the queue at
   * most once a minute whatever we call; a degraded round keeps the last read
   * on screen and says so.
   */
  import { onMount } from "svelte";
  import {
    clusterClose,
    clusterDismissJob,
    clusterFacts,
    clusterMove,
    clusterQueueOpen,
    notificationPermission,
    openNotificationSettings,
    requestNotificationPermission,
    type NativeNotificationPermission,
    clusterOpen,
    clusterOpenTerminal,
    clusterRemoveWorkspace,
    clusterSetLoginServe,
    setNotCluster,
    clusterStopJob,
    clusterStopLoginDaemon,
    onClusterChanged,
    type ClusterJob,
    type ClusterWorkspace,
    type ClusterWorkspaceView,
    type HostState,
    type LaunchSpec,
  } from "../net/native";
  import { asyncDisposer } from "../shared/asyncDisposer";
  import { pageVisible } from "../shared/visibility";
  import { contextMenu, type ContextMenuEntry } from "../shared/contextMenu.svelte";
  import ConfirmDialog from "../shared/ConfirmDialog.svelte";
  import { clusterNow, clusterOverviews } from "./clusterStore.svelte";
  import {
    agoWords,
    endedLine,
    jobStatusLine,
    liveJobs,
    openInHint,
    openPlan,
    otherJobsWords,
    shortHost,
    tildePath,
    parentPath,
    workspaceActivity,
  } from "./cluster";
  import ClusterStartSheet from "./ClusterStartSheet.svelte";
  import ClusterSettingDialog from "./ClusterSettingDialog.svelte";
  import ClusterFolderPicker from "./ClusterFolderPicker.svelte";

  interface Props {
    alias: string;
    /** The host's shell state (cluster info: login daemon, override). */
    host: HostState | null;
    /** Back to the host list (or, in a workspace's window, to the workspace). */
    onBack: () => void;
    /** What the back button says. */
    backLabel?: string;
    /** In a workspace's window: that workspace (its row says so, and Open
     *  goes back to it here instead of opening another window). */
    here?: string | null;
    onHere?: () => void;
    /** The shell returned a new state for this host (the override toggle). */
    onHostState: (state: HostState) => void;
    /** Something changed the host list (a login daemon was shut down). */
    onHostsChanged: () => void;
    /** The user said this host isn't a cluster (its new state); the ⋯ menu
     *  offers it only when given. */
    onNotCluster?: (state: HostState) => void;
  }

  let {
    alias,
    host,
    onBack,
    backLabel = "Home",
    here = null,
    onHere,
    onHostState,
    onHostsChanged,
    onNotCluster,
  }: Props = $props();

  const entry = $derived(clusterOverviews.entry(alias));
  const overview = $derived(entry?.overview ?? null);
  /** Re-read once a minute while visible, so countdowns move between polls. */
  let now = $state(Date.now());
  const clusterTime = $derived(entry === undefined ? now : clusterNow(entry, now));

  const loginServe = $derived(host?.cluster?.login_serve === true);
  const loginDaemon = $derived(loginServe ? null : (host?.cluster?.login_daemon ?? null));

  const jobs = $derived(overview === null ? [] : liveJobs(overview.jobs));
  const running = $derived(jobs.filter((j) => j.state === "running"));
  const ended = $derived(
    (overview?.jobs ?? [])
      .filter((j) => j.state === "ended")
      .sort((a, b) => (b.ended_at_ms ?? b.submitted_ms) - (a.ended_at_ms ?? a.submitted_ms)),
  );
  const closedWs = $derived((overview?.workspaces ?? []).filter((w) => w.state === "closed"));
  const firstVisit = $derived(
    overview !== null && overview.workspaces.length === 0 && overview.jobs.length === 0,
  );
  /** The folders that hold workspaces — the folder picker's quick links. */
  const places = $derived.by(() => {
    const out: string[] = [];
    for (const w of overview?.workspaces ?? []) {
      const p = parentPath(w.path);
      if (p !== null && !out.includes(p)) out.push(p);
    }
    return out;
  });

  function wsIn(job: ClusterJob): ClusterWorkspaceView[] {
    return (overview?.workspaces ?? []).filter((w) => w.job === job.id && w.state !== "closed");
  }

  function jobName(id: string | undefined): string {
    return overview?.jobs.find((j) => j.id === id)?.name ?? "another job";
  }

  /** A waiting job that continues `job` (its card says so). */
  function continuation(job: ClusterJob): ClusterJob | undefined {
    return jobs.find((j) => j.replaces === job.id && j.state !== "running");
  }

  // --- per-row action state ----------------------------------------------------
  type WsBusy = "opening" | "closing" | "moving" | "removing";
  type JobBusy = "stopping" | "cancelling" | "dismissing";
  let wsBusy = $state<Record<string, WsBusy>>({});
  let wsError = $state<Record<string, string>>({});
  let jobBusy = $state<Record<string, JobBusy>>({});
  let jobError = $state<Record<string, string>>({});

  type Sheet =
    | { kind: "start"; preselect: string[]; spec: LaunchSpec | null }
    | { kind: "continue"; job: ClusterJob };
  let sheet = $state<Sheet | null>(null);
  /** The folder picker is open; `job` = add-and-open opens it there. */
  let picker = $state<{ job: ClusterJob | null } | null>(null);
  let setting = $state<
    { mode: "startup"; workspace: { id: string; name: string } | null } | { mode: "rules" } | null
  >(null);
  let confirmStop = $state<ClusterJob | null>(null);
  let confirmStopError = $state<string | null>(null);
  let confirmClose = $state<ClusterWorkspaceView | null>(null);
  let confirmRemove = $state<ClusterWorkspaceView | null>(null);
  let confirmRemoveError = $state<string | null>(null);
  let confirmLoginServe = $state(false);
  let confirmNotCluster = $state(false);
  let notClusterError = $state<string | null>(null);
  let loginServeError = $state<string | null>(null);

  let mastError = $state<string | null>(null);
  let mastNote = $state<string | null>(null);
  let daemonBusy = $state(false);
  let daemonError = $state<string | null>(null);
  let moreBtn = $state<HTMLButtonElement | null>(null);

  function refreshExplicit(): void {
    void clusterOverviews.refresh(alias, 0, true);
  }

  function refresh(): void {
    void clusterOverviews.refresh(alias, 0);
  }

  onMount(() =>
    asyncDisposer(
      onClusterChanged((changed) => {
        if (changed === alias) refresh();
      }),
    ),
  );

  /** The page's first read is always fresh; later ones keep the floor. */
  let opened = false;
  // The 60 s poll and the minute tick, both only while the window is
  // visible. The effect re-runs on return: the floor keeps a quick
  // hide/show from turning into an extra ask.
  $effect(() => {
    if (!$pageVisible) return;
    now = Date.now();
    void clusterOverviews.refresh(alias, opened ? 55_000 : 0);
    opened = true;
    const t = setInterval(() => {
      now = Date.now();
      void clusterOverviews.refresh(alias, 55_000);
    }, 60_000);
    return () => clearInterval(t);
  });

  /** Notifications are how you hear a job started — say so when they're off. */
  let notify = $state<NativeNotificationPermission | null>(null);
  const jobsAlive = $derived(jobs.length > 0);
  $effect(() => {
    if (!jobsAlive || !$pageVisible) return;
    void notificationPermission().then((p) => (notify = p));
  });

  async function turnOnNotifications(): Promise<void> {
    if (notify === "not_determined") notify = await requestNotificationPermission();
    else await openNotificationSettings();
  }

  function errText(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  function setIn<T>(map: Record<string, T>, id: string, v: T | null): Record<string, T> {
    const next = { ...map };
    if (v === null) delete next[id];
    else next[id] = v;
    return next;
  }

  /** Run one workspace action with its busy word and error line. */
  async function wsAction(w: ClusterWorkspaceView, busy: WsBusy, run: () => Promise<void>): Promise<void> {
    if (wsBusy[w.id] !== undefined) return;
    wsError = setIn(wsError, w.id, null);
    wsBusy = setIn(wsBusy, w.id, busy);
    try {
      await run();
    } catch (e) {
      wsError = setIn(wsError, w.id, errText(e));
    } finally {
      wsBusy = setIn(wsBusy, w.id, null);
      refresh();
    }
  }

  async function jobAction(j: ClusterJob, busy: JobBusy, run: () => Promise<void>): Promise<void> {
    if (jobBusy[j.id] !== undefined) return;
    jobError = setIn(jobError, j.id, null);
    jobBusy = setIn(jobBusy, j.id, busy);
    try {
      await run();
    } catch (e) {
      jobError = setIn(jobError, j.id, errText(e));
    } finally {
      jobBusy = setIn(jobBusy, j.id, null);
      refresh();
    }
  }

  function openIn(w: ClusterWorkspaceView, jobId: string | null): void {
    void wsAction(w, "opening", () => clusterOpen(alias, w.id, jobId));
  }

  function startSheet(preselect: string[], spec: LaunchSpec | null = null): void {
    sheet = { kind: "start", preselect, spec };
  }

  /** Open: where it's open (this window's own workspace: back to it here).
   *  Anywhere else is your pick — a running job, when a waiting one starts,
   *  or a new job; with no job alive, the start sheet with it ticked. */
  function openClicked(e: MouseEvent, w: ClusterWorkspaceView): void {
    if (w.state === "open") {
      if (w.id === here && onHere !== undefined) onHere();
      else openIn(w, null);
      return;
    }
    const plan = openPlan(overview?.jobs ?? []);
    if (plan.kind === "sheet") {
      startSheet([w.id]);
      return;
    }
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items: ContextMenuEntry[] = [];
    for (const j of plan.running) {
      const detail = openInHint(j, wsIn(j).map((x) => x.name), clusterTime);
      items.push({
        label: `In ${j.name}`,
        detail: detail === "" ? undefined : detail,
        onSelect: () => openIn(w, j.id),
      });
    }
    for (const j of plan.pending) {
      items.push({
        label: `When ${j.name} starts`,
        detail: j.state === "starting" ? "starting now" : "waiting for a node",
        onSelect: () => void wsAction(w, "opening", () => clusterQueueOpen(alias, w.id, j.id)),
      });
    }
    items.push("separator", { label: "In a new job…", onSelect: () => startSheet([w.id]) });
    contextMenu.openAtPoint(r.right, r.bottom + 4, items, { alignRight: true });
  }

  function closeWs(w: ClusterWorkspaceView): void {
    if ((w.working ?? 0) > 0) confirmClose = w;
    else void wsAction(w, "closing", () => clusterClose(alias, w.id));
  }

  function wsMenu(e: MouseEvent, w: ClusterWorkspaceView): void {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items: ContextMenuEntry[] = [];
    if (w.state === "open" && !w.closing) {
      for (const j of running.filter((j) => j.id !== w.job)) {
        items.push({
          label: `Move to ${j.name}`,
          onSelect: () => void wsAction(w, "moving", () => clusterMove(alias, w.id, j.id)),
        });
      }
      items.push({ label: "Close", onSelect: () => closeWs(w) }, "separator");
    }
    items.push({
      label: "Startup commands…",
      onSelect: () => (setting = { mode: "startup", workspace: { id: w.id, name: w.name } }),
    });
    if (w.state === "closed") {
      items.push("separator", {
        label: "Remove from this list…",
        danger: true,
        onSelect: () => {
          confirmRemoveError = null;
          confirmRemove = w;
        },
      });
    }
    contextMenu.openAtPoint(r.right, r.bottom + 4, items, { alignRight: true });
  }

  /** "+ Open a workspace here": the closed workspaces, then adding one. */
  function openHereMenu(e: MouseEvent, job: ClusterJob): void {
    if (closedWs.length === 0) {
      picker = { job };
      return;
    }
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items: ContextMenuEntry[] = closedWs.map((w) => ({
      label: w.name,
      detail: tildePath(w.path, overview?.home),
      onSelect: () => openIn(w, job.id),
    }));
    items.push("separator", { label: "Add a workspace…", onSelect: () => (picker = { job }) });
    contextMenu.openAtPoint(r.left, r.bottom + 4, items);
  }

  async function confirmStopNow(): Promise<void> {
    const j = confirmStop;
    if (j === null) return;
    confirmStopError = null;
    jobBusy = setIn(jobBusy, j.id, "stopping");
    try {
      await clusterStopJob(alias, j.id);
      confirmStop = null;
    } catch (e) {
      confirmStopError = errText(e);
    } finally {
      jobBusy = setIn(jobBusy, j.id, null);
      refresh();
    }
  }

  async function removeNow(): Promise<void> {
    const w = confirmRemove;
    if (w === null) return;
    confirmRemoveError = null;
    wsBusy = setIn(wsBusy, w.id, "removing");
    try {
      await clusterRemoveWorkspace(alias, w.id);
      confirmRemove = null;
    } catch (e) {
      confirmRemoveError = errText(e);
    } finally {
      wsBusy = setIn(wsBusy, w.id, null);
      refresh();
    }
  }

  /** Added from a job's "Open a workspace here": open it there. Added from
   *  the list, it's just added — its Open asks where. */
  function added(ws: ClusterWorkspace, openAfter: boolean): void {
    const target = picker?.job ?? null;
    picker = null;
    refresh();
    if (!openAfter || target === null) return;
    const view: ClusterWorkspaceView = { ...ws, state: "closed" };
    openIn(view, target.id);
  }

  async function openTerminal(): Promise<void> {
    mastError = null;
    try {
      await clusterOpenTerminal(alias);
    } catch (e) {
      mastError = errText(e);
    }
  }

  async function refreshPartitions(): Promise<void> {
    mastError = null;
    mastNote = "Reading partitions…";
    try {
      const facts = await clusterFacts(alias, true);
      const n = facts.partitions.length;
      mastNote = `Partitions refreshed — ${n} listed.`;
    } catch (e) {
      mastNote = null;
      mastError = `Couldn't read partitions: ${errText(e)}`;
    }
  }

  async function setLoginServe(on: boolean): Promise<void> {
    loginServeError = null;
    try {
      onHostState(await clusterSetLoginServe(alias, on));
      confirmLoginServe = false;
    } catch (e) {
      if (on) loginServeError = errText(e);
      else mastError = errText(e);
    }
  }

  async function markNotCluster(): Promise<void> {
    notClusterError = null;
    try {
      const state = await setNotCluster(alias, true);
      confirmNotCluster = false;
      onNotCluster?.(state);
    } catch (e) {
      notClusterError = errText(e);
    }
  }

  async function shutDownLoginDaemon(): Promise<void> {
    daemonBusy = true;
    daemonError = null;
    try {
      await clusterStopLoginDaemon(alias);
      onHostsChanged();
    } catch (e) {
      daemonError = errText(e);
    } finally {
      daemonBusy = false;
    }
  }

  function openMore(): void {
    const el = moreBtn;
    if (el === null) return;
    const r = el.getBoundingClientRect();
    const waiting = overview === null;
    const items: ContextMenuEntry[] = [
      {
        label: "Startup commands…",
        disabled: waiting,
        hint: waiting ? "Waiting for the cluster" : undefined,
        onSelect: () => (setting = { mode: "startup", workspace: null }),
      },
      {
        label: "Rules for agents…",
        disabled: waiting,
        hint: waiting ? "Waiting for the cluster" : undefined,
        onSelect: () => (setting = { mode: "rules" }),
      },
      { label: "Refresh partitions", onSelect: () => void refreshPartitions() },
      "separator",
      {
        label: "Run Chimaera on the login node",
        checked: loginServe,
        onSelect: () => {
          if (loginServe) void setLoginServe(false);
          else {
            loginServeError = null;
            confirmLoginServe = true;
          }
        },
      },
    ];
    if (onNotCluster !== undefined) {
      items.push({
        label: "This isn't a cluster…",
        onSelect: () => {
          notClusterError = null;
          confirmNotCluster = true;
        },
      });
    }
    contextMenu.openAtPoint(r.right, r.bottom + 4, items, { alignRight: true });
  }

  function jobDot(j: ClusterJob): string {
    if (jobBusy[j.id] === "stopping" || jobBusy[j.id] === "cancelling") return "ending";
    return j.state === "running" ? "alive" : j.state === "starting" ? "booting" : "queued";
  }

  function stopBody(j: ClusterJob): string {
    const names = wsIn(j)
      .filter((w) => w.state === "open")
      .map((w) => w.name);
    if (names.length === 0) return "Nothing is open in it.";
    const list =
      names.length === 1
        ? names[0]
        : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
    return `${list} ${names.length === 1 ? "closes" : "close"}. ${names.length === 1 ? "Its" : "Their"} chats are saved.`;
  }
</script>

{#snippet dots()}
  <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
    <circle cx="3.5" cy="8" r="1.2" fill="currentColor" />
    <circle cx="8" cy="8" r="1.2" fill="currentColor" />
    <circle cx="12.5" cy="8" r="1.2" fill="currentColor" />
  </svg>
{/snippet}

{#snippet refreshButton()}
  <button
    class="ghost refresh"
    class:spinning={entry?.loading === true}
    title={overview !== null && overview.queue_at_ms > 0
      ? `Refresh — the queue was read ${agoWords(overview.queue_at_ms, clusterTime)}`
      : "Refresh"}
    aria-label="Refresh"
    disabled={entry?.loading === true}
    onclick={refreshExplicit}
  >
    <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
      <path
        d="M13.2 8a5.2 5.2 0 1 1-1.5-3.7M13.4 2.5v2.3h-2.3"
        fill="none"
        stroke="currentColor"
        stroke-width="1.4"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  </button>
{/snippet}

{#snippet wsRow(w: ClusterWorkspaceView)}
  {@const busy = wsBusy[w.id]}
  {@const activity = workspaceActivity(w, clusterTime)}
  <div class="ws" class:live={w.state === "open"}>
    <div class="ws-main">
      <span class="name" title={w.name}>{w.name}</span>
      <span class="path" title={w.path}>{tildePath(w.path, overview?.home)}</span>
      {#if w.id === here}<span class="here-tag">this window</span>{/if}
      <span class="acts">
        {#if busy !== undefined}
          <span class="busy-word"
            >{busy === "opening"
              ? "Opening…"
              : busy === "closing"
                ? "Closing…"
                : busy === "moving"
                  ? "Moving…"
                  : "Removing…"}</span
          >
        {:else}
          {#if w.state !== "queued" && !w.closing}
            <button
              class="act primary"
              aria-haspopup={w.state === "closed" && jobs.length > 0 ? "menu" : undefined}
              onclick={(e) => openClicked(e, w)}
            >
              Open{#if w.state === "closed" && jobs.length > 0}
                <svg class="chev-down" viewBox="0 0 16 16" width="9" height="9" aria-hidden="true">
                  <path d="M4 6l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
                </svg>
              {/if}
            </button>
          {/if}
          <button
            class="act icon"
            aria-label="More for {w.name}"
            aria-haspopup="menu"
            title="More"
            onclick={(e) => wsMenu(e, w)}>{@render dots()}</button
          >
        {/if}
      </span>
    </div>
    {#if activity !== ""}
      <div class="detail" class:warn={w.failed !== undefined} title={w.failed || undefined}>{activity}</div>
    {/if}
    {#if wsError[w.id] !== undefined}
      <div class="detail row-err">{wsError[w.id]}</div>
    {/if}
  </div>
{/snippet}

<div class="inner">
  <header class="masthead">
    <div class="topline">
      <button class="back-home" aria-label="Back to {backLabel}" title="Back to {backLabel}" onclick={onBack}>
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
        <span>{backLabel}</span>
      </button>
      <div class="mast-acts">
        <button class="mast-btn" title="A terminal on {alias}'s login node" onclick={() => void openTerminal()}>
          <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
            <rect x="1.8" y="2.8" width="12.4" height="10.4" rx="2" fill="none" stroke="currentColor" stroke-width="1.3" />
            <path d="M4.5 6.2 6.6 8l-2.1 1.8M8.2 10h3.3" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" />
          </svg>
          Terminal
        </button>
        <button
          class="mast-btn icon"
          aria-label="More for {alias}"
          aria-haspopup="menu"
          title="More"
          bind:this={moreBtn}
          onclick={openMore}>{@render dots()}</button
        >
        <button
          class="mast-btn primary"
          disabled={overview === null}
          onclick={() => startSheet([])}
        >
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <path d="M8 3v10M3 8h10" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
          </svg>
          Start a job
        </button>
      </div>
    </div>
    <h1>{alias}</h1>
    <div class="kind">
      <span class="sched">Slurm cluster</span>
      {#if overview !== null && overview.login_node !== ""}
        <span title="Connected through {overview.login_node}"
          >· via <span class="mono">{shortHost(overview.login_node)}</span></span
        >
      {/if}
      {#if loginServe}
        <span class="pill-warn" title="Allowed on the login node (from the … menu)">login node</span>
      {/if}
    </div>
    {#if mastError !== null}
      <div class="err-line">{mastError}</div>
    {:else if mastNote !== null}
      <div class="note-line">{mastNote}</div>
    {/if}
  </header>

  {#if jobsAlive && (notify === "denied" || notify === "not_determined")}
    <div class="notice" role="note">
      <p>Notifications are off, so you won't hear when a job starts or is about to end.</p>
      <button class="notice-act" onclick={() => void turnOnNotifications()}>
        {notify === "denied" ? "Open settings" : "Turn on"}
      </button>
    </div>
  {/if}

  {#if loginDaemon !== null}
    <div class="notice" role="note">
      <p>
        A Chimaera from before is still running on <span class="mono">{loginDaemon.node}</span>.
        Many clusters don't allow servers on login nodes.
      </p>
      <button class="notice-act" disabled={daemonBusy} onclick={() => void shutDownLoginDaemon()}>
        {daemonBusy ? "Shutting down…" : "Shut it down"}
      </button>
      {#if daemonError !== null}<div class="notice-err">{daemonError}</div>{/if}
    </div>
  {/if}

  {#if overview === null}
    {#if entry !== undefined && entry.error !== null}
      <div class="err-line">
        Couldn't read the cluster: {entry.error}
        <button class="inline-act" onclick={refreshExplicit}>Try again</button>
      </div>
    {:else}
      <div class="reading" role="status">Reading the cluster…</div>
    {/if}
  {:else if firstVisit}
    <div class="welcome">
      <h2>Work on {alias} in Slurm jobs</h2>
      <p>
        Chimaera runs inside jobs you start, never on the login node. Add a folder to work in,
        then start a job to open it.
      </p>
      <div class="welcome-acts">
        <button class="cta primary" onclick={() => (picker = { job: null })}>Add a workspace…</button>
        <button class="cta" onclick={() => startSheet([])}>Start a job</button>
      </div>
    </div>
  {:else}
    {#if overview.degraded}
      <div class="degraded" role="status">
        The queue didn't answer; showing the last read{overview.queue_at_ms > 0
          ? ` (${agoWords(overview.queue_at_ms, clusterTime)})`
          : ""}.
      </div>
    {/if}
    {#if entry !== undefined && entry.error !== null}
      <div class="err-line quiet">
        Couldn't refresh: {entry.error}
        <button class="inline-act" onclick={refreshExplicit}>Try again</button>
      </div>
    {/if}

    {#if jobs.length > 0}
      <section class="jobs" aria-label="Jobs">
        <div class="sec-head">
          <span class="sec-title">jobs</span>
          {@render refreshButton()}
        </div>
        {#each jobs as j (j.id)}
          {@const busy = jobBusy[j.id]}
          {@const inside = wsIn(j)}
          {@const next = continuation(j)}
          <div class="job" class:running={j.state === "running"}>
            <div class="job-head">
              <span class="dot {jobDot(j)}" title={j.state}></span>
              <span class="job-name" title={j.slurm_job_id ? `Slurm job ${j.slurm_job_id}` : j.name}>{j.name}</span>
              <span class="acts">
                {#if busy === "stopping"}
                  <span class="busy-word">Stopping…</span>
                {:else if busy === "cancelling"}
                  <span class="busy-word">Cancelling…</span>
                {:else if j.state === "waiting"}
                  <button
                    class="act"
                    onclick={() => void jobAction(j, "cancelling", () => clusterStopJob(alias, j.id))}
                    >Cancel</button
                  >
                {:else}
                  {#if j.state === "running" && !j.attached && next === undefined}
                    <button class="act" onclick={() => (sheet = { kind: "continue", job: j })}
                      >Continue in a new job…</button
                    >
                  {/if}
                  <button
                    class="act"
                    onclick={() => {
                      confirmStopError = null;
                      confirmStop = j;
                    }}>Stop</button
                  >
                {/if}
              </span>
            </div>
            <div class="job-line">{jobStatusLine(j, clusterTime)}</div>
            {#if j.replaces !== undefined}
              <div class="job-line note">
                Continues {jobName(j.replaces)} — its workspaces move here when this one starts.
              </div>
            {/if}
            {#if next !== undefined}
              <div class="job-line note">Continuing in a new job — waiting for a node.</div>
            {/if}
            {#if j.state === "running" && j.egress === false}
              <div class="job-line warn">Agents can't reach the internet from this node.</div>
            {/if}
            {#if jobError[j.id] !== undefined}
              <div class="job-line row-err">{jobError[j.id]}</div>
            {/if}
            {#if inside.length > 0 || j.state === "running"}
              <div class="job-ws">
                {#each inside as w (w.id)}
                  {@render wsRow(w)}
                {/each}
                {#if j.state === "running"}
                  <button class="open-here" onclick={(e) => openHereMenu(e, j)}>
                    <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
                      <path d="M8 3v10M3 8h10" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
                    </svg>
                    Open a workspace here
                  </button>
                {/if}
              </div>
            {/if}
          </div>
        {/each}
      </section>
    {/if}

    {#each ended as j (j.id)}
      <div class="ended">
        <span class="ended-text">{endedLine(j, clusterTime)}</span>
        <button
          class="act"
          onclick={() =>
            startSheet(
              j.open.filter((id) => closedWs.some((w) => w.id === id)),
              j.spec,
            )}>Start again</button
        >
        <button
          class="act icon"
          aria-label="Dismiss"
          title="Dismiss"
          disabled={jobBusy[j.id] !== undefined}
          onclick={() => void jobAction(j, "dismissing", () => clusterDismissJob(alias, j.id))}
        >
          <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
            <path d="M4 4l8 8M12 4l-8 8" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
          </svg>
        </button>
      </div>
    {/each}

    <section aria-label="Not open">
      {#if closedWs.length > 0}
        <div class="sec-head">
          <span class="sec-title">{closedWs.length < (overview.workspaces.length ?? 0)
              ? "other workspaces"
              : "workspaces"}</span>
          {#if jobs.length === 0}{@render refreshButton()}{/if}
        </div>
        <div class="rows">
          {#each closedWs as w (w.id)}
            {@render wsRow(w)}
          {/each}
        </div>
      {/if}
      <button class="add-ws" onclick={() => (picker = { job: null })}>
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
          <path d="M8 3v10M3 8h10" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
        </svg>
        Add a workspace…
      </button>
    </section>

    {#if otherJobsWords(overview.other_jobs) !== ""}
      <div class="other-jobs">{otherJobsWords(overview.other_jobs)}</div>
    {/if}
  {/if}
</div>

{#if sheet !== null && overview !== null}
  <ClusterStartSheet
    {alias}
    config={overview.config}
    clusterStartup={overview.startup.cluster}
    workspaces={overview.workspaces}
    home={overview.home}
    preselect={sheet.kind === "start" ? sheet.preselect : []}
    initialSpec={sheet.kind === "start" ? sheet.spec : null}
    continueJob={sheet.kind === "continue" ? sheet.job : null}
    onStarted={() => {
      sheet = null;
      refresh();
    }}
    onClose={() => (sheet = null)}
  />
{/if}

{#if picker !== null}
  <ClusterFolderPicker
    {alias}
    {places}
    canOpen={picker.job !== null}
    onAdded={added}
    onClose={() => (picker = null)}
  />
{/if}

{#if setting !== null && overview !== null}
  <ClusterSettingDialog
    {alias}
    mode={setting.mode}
    workspace={setting.mode === "startup" ? setting.workspace : null}
    startup={setting.mode === "startup"
      ? setting.workspace !== null
        ? (overview.startup.workspaces[setting.workspace.id] ?? "")
        : overview.startup.cluster
      : ""}
    rules={overview.config.agent_rules}
    onSaved={() => {
      setting = null;
      refresh();
    }}
    onClose={() => (setting = null)}
  />
{/if}

{#if confirmStop !== null}
  <ConfirmDialog
    title={`Stop ${confirmStop.name}?`}
    body={stopBody(confirmStop)}
    confirmLabel="Stop job"
    danger
    error={confirmStopError}
    onConfirm={() => void confirmStopNow()}
    onCancel={() => (confirmStop = null)}
  />
{/if}

{#if confirmClose !== null}
  {@const w = confirmClose}
  <ConfirmDialog
    title={`Close ${w.name}?`}
    body={`${w.working === 1 ? "1 chat is" : `${w.working} chats are`} working — ${w.working === 1 ? "it stops" : "they stop"} now. Chats are saved; the job keeps running.`}
    confirmLabel="Close"
    onConfirm={() => {
      confirmClose = null;
      void wsAction(w, "closing", () => clusterClose(alias, w.id));
    }}
    onCancel={() => (confirmClose = null)}
  />
{/if}

{#if confirmRemove !== null}
  <ConfirmDialog
    title={`Remove ${confirmRemove.name} from this list?`}
    body={`Your files and chats stay where they are. Add ${confirmRemove.path} again any time to bring it back.`}
    confirmLabel="Remove"
    error={confirmRemoveError}
    onConfirm={() => void removeNow()}
    onCancel={() => (confirmRemove = null)}
  />
{/if}

{#if confirmNotCluster}
  <ConfirmDialog
    title={`Treat ${alias} as a regular server?`}
    body={`Its login shell reaches Slurm, so it looked like a cluster. As a regular server, Chimaera runs on ${alias} itself, like on any remote, and this page goes away. If it is a cluster's login node, leave it as it is: many clusters don't allow that. You can switch back from its row on Home.`}
    confirmLabel="Treat as a server"
    error={notClusterError}
    onConfirm={() => void markNotCluster()}
    onCancel={() => (confirmNotCluster = false)}
  />
{/if}

{#if confirmLoginServe}
  <ConfirmDialog
    title="Run Chimaera on the login node?"
    body="Chimaera and its agents would keep running on a shared login node after you disconnect. Many clusters don't allow that. Turn this on only if your cluster's admins say it's fine."
    confirmLabel="Turn on anyway"
    danger
    error={loginServeError}
    onConfirm={() => void setLoginServe(true)}
    onCancel={() => (confirmLoginServe = false)}
  />
{/if}

<style>
  .inner {
    max-width: 640px;
    margin: 0 auto;
    /* The top stays clear of the native window's 32px drag strip. */
    padding: clamp(40px, 8vh, 72px) 24px 64px;
    display: flex;
    flex-direction: column;
    gap: 30px;
  }

  .masthead {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .topline {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 10px;
  }

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
    margin-left: -4px;
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

  .mast-acts {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .mast-btn {
    appearance: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    padding: 4px 10px;
    border-radius: 6px;
    cursor: pointer;
    transition: border-color 0.12s ease;
  }

  .mast-btn:hover {
    border-color: color-mix(in srgb, var(--accent) 60%, var(--edge));
  }

  .mast-btn.icon {
    padding: 4px 6px;
    color: var(--muted);
  }

  .mast-btn.icon:hover {
    color: var(--fg);
  }

  h1 {
    margin: 0;
    font-family: var(--mono);
    font-size: calc(var(--text-lg) + 4px);
    font-weight: 600;
    letter-spacing: 0.01em;
    overflow-wrap: anywhere;
  }

  .kind {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 5px;
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .sched {
    color: var(--accent);
  }

  .pill-warn {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--warn);
    border: 1px solid color-mix(in srgb, var(--warn) 40%, transparent);
    border-radius: 999px;
    padding: 0 7px;
    margin-left: 3px;
  }

  .mono {
    font-family: var(--mono);
  }

  .note-line {
    font-size: var(--text-xs);
    color: var(--muted);
  }

  /* The found-from-before login daemon: calm, one fact, one action. */
  .notice {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 14px;
    padding: 11px 14px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--warn) 10%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn) 28%, var(--edge));
  }

  .notice p {
    flex: 1;
    min-width: 240px;
    margin: 0;
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--fg);
  }

  .notice-act {
    appearance: none;
    flex: none;
    border: 1px solid color-mix(in srgb, var(--warn) 50%, var(--edge));
    background: var(--overlay-bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    padding: 4px 11px;
    border-radius: 6px;
    cursor: pointer;
  }

  .notice-act:hover:enabled {
    border-color: var(--warn);
  }

  .notice-act:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .notice-err {
    flex-basis: 100%;
    font-size: var(--text-xs);
    color: var(--err);
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
    padding: 0 8px 4px;
  }

  .sec-title {
    font-size: var(--text-xs);
    color: var(--muted);
    text-transform: lowercase;
    letter-spacing: 0.04em;
  }

  .sec-acts {
    display: flex;
    align-items: center;
    gap: 2px;
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

  .ghost:hover:enabled {
    color: var(--fg);
  }

  .ghost.refresh {
    display: flex;
    align-items: center;
  }

  .ghost.refresh.spinning svg {
    animation: spin 0.9s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  .add {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 2px 10px 10px;
  }

  .add-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .add-input {
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

  .add-input.path-field {
    flex: 2;
  }

  .add-input.name-field {
    flex: 1;
  }

  .add-input.bad {
    border-color: color-mix(in srgb, var(--err) 60%, var(--edge));
  }

  .add-input:focus {
    border-color: var(--focus-ring);
  }

  .add-input::placeholder {
    color: var(--muted);
    opacity: 0.7;
  }

  .field-err {
    font-size: var(--text-xs);
    color: var(--err);
    white-space: pre-wrap;
  }

  .field-hint {
    font-size: var(--text-xs);
    color: var(--muted);
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

  .cta:hover:enabled {
    border-color: var(--accent);
  }

  .cta:disabled {
    opacity: 0.55;
    cursor: default;
  }

  .cta.small {
    flex: none;
    padding: 5px 12px;
    font-size: var(--text-sm);
  }

  .degraded {
    padding: 0 10px 2px;
    font-size: var(--text-xs);
    color: var(--warn);
  }

  .reading {
    padding: 4px 10px;
    font-size: var(--text-sm);
    color: var(--muted);
    animation: breathe 1.4s ease-in-out infinite;
  }

  @keyframes breathe {
    50% {
      opacity: 0.45;
    }
  }


  .rows {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .ws {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 8px 10px 9px;
    border-radius: 6px;
    transition: background-color 0.12s ease;
  }

  .ws:hover {
    background: var(--row-hover);
  }

  /* A running workspace carries the home's faint accent wash. */
  .ws.live {
    background: color-mix(in srgb, var(--accent) 6%, transparent);
  }

  .ws.live:hover {
    background: var(--row-hover);
  }

  .ws-main {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }

  .name {
    flex: none;
    max-width: 40%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-md);
  }

  .ws.live .name {
    color: var(--accent);
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

  .here-tag {
    flex: none;
    font-size: var(--text-xs);
    color: var(--accent);
    border: 1px solid color-mix(in srgb, var(--accent) 35%, transparent);
    border-radius: 999px;
    padding: 0 7px;
    white-space: nowrap;
  }

  .acts {
    flex: none;
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .act {
    appearance: none;
    display: inline-flex;
    align-items: center;
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    padding: 2px 9px;
    border-radius: 5px;
    cursor: pointer;
    transition:
      color 0.12s ease,
      border-color 0.12s ease;
  }

  .act:hover:enabled {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--fg) 25%, var(--edge));
  }

  .act.primary {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }

  .act.primary:hover:enabled {
    border-color: var(--accent);
  }

  .act.icon {
    padding: 2px 5px;
  }

  .act:disabled {
    opacity: 0.55;
    cursor: default;
  }

  .busy-word {
    font-size: var(--text-xs);
    color: var(--warn);
  }

  .detail {
    padding-left: 17px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .detail.note {
    opacity: 0.9;
  }

  .detail.warn {
    color: var(--warn);
    opacity: 1;
  }

  .detail.row-err {
    color: var(--err);
    white-space: pre-wrap;
  }

  .other-jobs {
    padding: 8px 10px 0;
    font-size: var(--text-xs);
    color: var(--muted);
    opacity: 0.85;
  }

  /* State dots — the home screen's language: dormant muted, running in the
     accent, a booting job pulses, a queued job is a hollow pulse, a stop in
     flight is amber. */
  .dot {
    flex: none;
    box-sizing: border-box;
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

  .dot.booting {
    background: var(--accent);
    opacity: 1;
    animation: dotpulse 1.1s ease-in-out infinite;
  }

  .dot.queued {
    background: transparent;
    border: 1.5px solid var(--muted);
    opacity: 1;
    animation: dotpulse 1.6s ease-in-out infinite;
  }

  .dot.ending {
    background: var(--warn);
    opacity: 0.9;
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

  :global(html.app-hidden) .dot.booting,
  :global(html.app-hidden) .dot.queued,
  :global(html.app-hidden) .reading,
  :global(html.app-hidden) .ghost.refresh.spinning svg {
    animation-play-state: paused;
  }

  .err-line {
    padding: 2px 10px 6px;
    font-size: var(--text-sm);
    color: var(--err);
    white-space: pre-wrap;
  }

  .err-line.quiet {
    font-size: var(--text-xs);
    padding-top: 0;
  }

  .inline-act {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    color: var(--fg);
    text-decoration: underline;
    cursor: pointer;
    padding: 0 4px;
  }
  .mast-btn.primary {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 12%, var(--overlay-bg));
  }

  .mast-btn.primary:hover:enabled {
    background: color-mix(in srgb, var(--accent) 20%, var(--overlay-bg));
  }

  .mast-btn:disabled {
    opacity: 0.55;
    cursor: default;
  }

  /* First visit: one invitation, two ways in. */
  .welcome {
    border: 1px dashed var(--edge);
    border-radius: 10px;
    padding: 28px 24px;
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    gap: 10px;
  }

  .welcome h2 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
    color: var(--fg);
  }

  .welcome p {
    margin: 0;
    max-width: 46ch;
    font-size: var(--text-md);
    line-height: 1.55;
    color: var(--muted);
  }

  .welcome-acts {
    display: flex;
    gap: 8px;
    margin-top: 6px;
  }

  .cta.primary {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 12%, var(--overlay-bg));
  }

  /* A job: a card holding the workspaces open in it. */
  .jobs {
    gap: 10px;
  }

  .job {
    border: 1px solid var(--edge);
    border-radius: 10px;
    padding: 10px 12px 8px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    background: var(--overlay-bg);
  }

  .job.running {
    border-color: color-mix(in srgb, var(--accent) 30%, var(--edge));
  }

  .job-head {
    display: flex;
    align-items: center;
    gap: 9px;
    min-width: 0;
  }

  .job-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
  }

  .job-line {
    padding-left: 16px;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .job-line.note {
    font-size: var(--text-xs);
  }

  .job-line.warn {
    font-size: var(--text-xs);
    color: var(--warn);
  }

  .job-line.row-err {
    font-size: var(--text-xs);
    color: var(--err);
    white-space: pre-wrap;
  }

  .job-ws {
    margin-top: 6px;
    padding-top: 4px;
    border-top: 1px solid color-mix(in srgb, var(--edge) 70%, transparent);
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .open-here,
  .add-ws {
    appearance: none;
    align-self: flex-start;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: none;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    padding: 5px 10px;
    border-radius: 5px;
    cursor: pointer;
  }

  .open-here:hover,
  .add-ws:hover {
    color: var(--fg);
    background: var(--row-hover);
  }

  .add-ws {
    margin-top: 2px;
  }

  .chev-down {
    margin-left: 4px;
  }

  /* An ended job: one line, until dismissed. */
  .ended {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border-radius: 6px;
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }

  .ended-text {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }
</style>

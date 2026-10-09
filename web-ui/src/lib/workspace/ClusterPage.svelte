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
  import { onMount, untrack } from "svelte";
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
    isJobStopping,
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
    /** Something changed the host list (a login daemon was shut down). */
    onHostsChanged: () => void;
    /** Connection placement and recovery, available from the local Home hub. */
    onConnectionSettings?: () => void;
    /** A job window's Start again: open the start sheet for this workspace,
     *  set up like the job (by Slurm id) that ended, once the page has read
     *  the cluster. */
    startAgain?: { workspace: string; slurmJobId: string } | null;
  }

  let {
    alias,
    host,
    onBack,
    backLabel = "Home",
    here = null,
    onHere,
    onHostsChanged,
    onConnectionSettings,
    startAgain = null,
  }: Props = $props();

  const entry = $derived(clusterOverviews.entry(alias));
  const overview = $derived(entry?.overview ?? null);
  /** Re-read once a minute while visible, so countdowns move between polls. */
  let now = $state(Date.now());
  const clusterTime = $derived(entry === undefined ? now : clusterNow(entry, now));

  const loginServe = $derived(host?.cluster?.login_serve === true);
  const loginDaemon = $derived(loginServe ? null : (host?.cluster?.login_daemon ?? null));

  const jobs = $derived(overview === null ? [] : liveJobs(overview.jobs).map((j) =>
    jobBusy[j.id] === "stopping" || jobBusy[j.id] === "cancelling" ? { ...j, stopping: true } : j,
  ));
  const running = $derived(jobs.filter((j) => j.state === "running" && !isJobStopping(j)));
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

  // scancel returning means cancellation was accepted, not that Slurm has
  // finished. Preserve the action through stale overview reads and SSH errors.
  $effect(() => {
    const current = overview;
    if (current === null) return;
    untrack(() => {
      const next = Object.fromEntries(Object.entries(jobBusy).filter(([id]) =>
        current.jobs.some((job) => job.id === id && job.state !== "ended"),
      ));
      if (Object.keys(next).length !== Object.keys(jobBusy).length) jobBusy = next;
    });
  });

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
  let confirmClose = $state<ClusterWorkspaceView | null>(null);
  let confirmRemove = $state<ClusterWorkspaceView | null>(null);
  let confirmRemoveError = $state<string | null>(null);

  let mastError = $state<string | null>(null);
  let mastNote = $state<string | null>(null);
  let daemonBusy = $state(false);
  let daemonError = $state<string | null>(null);
  let moreBtn = $state<HTMLButtonElement | null>(null);

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
      jobBusy = setIn(jobBusy, j.id, null);
    } finally {
      if (busy === "dismissing") jobBusy = setIn(jobBusy, j.id, null);
      refresh();
    }
  }

  /** "Clear all": dismiss the ended jobs one at a time (each is its own
   *  short command on the cluster — never a burst), then read once. */
  let clearing = $state(false);
  async function clearEnded(): Promise<void> {
    if (clearing) return;
    clearing = true;
    try {
      for (const j of ended) {
        if (jobBusy[j.id] !== undefined) continue;
        jobError = setIn(jobError, j.id, null);
        jobBusy = setIn(jobBusy, j.id, "dismissing");
        try {
          await clusterDismissJob(alias, j.id);
        } catch (e) {
          jobError = setIn(jobError, j.id, errText(e));
        } finally {
          jobBusy = setIn(jobBusy, j.id, null);
        }
      }
    } finally {
      clearing = false;
      refresh();
    }
  }

  function openIn(w: ClusterWorkspaceView, jobId: string | null): void {
    const target = jobs.find((j) => j.id === (jobId ?? w.job));
    if (target !== undefined && isJobStopping(target)) return;
    void wsAction(w, "opening", () => clusterOpen(alias, w.id, jobId));
  }

  function startSheet(preselect: string[], spec: LaunchSpec | null = null): void {
    sheet = { kind: "start", preselect, spec };
  }

  // Start again from a job window: once, when the overview first lands.
  let startedAgain = false;
  $effect(() => {
    if (startAgain === null || overview === null || startedAgain) return;
    startedAgain = true;
    const from = overview.jobs.find((j) => j.slurm_job_id === startAgain.slurmJobId);
    untrack(() => startSheet([startAgain.workspace], from?.spec ?? null));
  });

  /** Open: where it's open (this window's own workspace: back to it here).
   *  Anywhere else is your pick — a running job, when a waiting one starts,
   *  or a new job; with no job alive, the start sheet with it ticked. */
  function openClicked(e: MouseEvent, w: ClusterWorkspaceView): void {
    if (w.state === "open") {
      if (w.id === here && onHere !== undefined) onHere();
      else openIn(w, null);
      return;
    }
    const plan = openPlan(jobs);
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

  function confirmStopNow(): void {
    const j = confirmStop;
    if (j === null) return;
    confirmStop = null;
    void jobAction(j, "stopping", () => clusterStopJob(alias, j.id));
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
    ];
    if (onConnectionSettings !== undefined) {
      items.push("separator", { label: "Connection settings…", onSelect: onConnectionSettings });
    }
    contextMenu.openAtPoint(r.right, r.bottom + 4, items, { alignRight: true });
  }

  function jobDot(j: ClusterJob): string {
    if (isJobStopping(j) || jobBusy[j.id] === "stopping" || jobBusy[j.id] === "cancelling") return "ending";
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
    onclick={refresh}
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

{#snippet wsRow(w: ClusterWorkspaceView, stopping = false)}
  {@const busy = wsBusy[w.id]}
  {@const activity = workspaceActivity(w, clusterTime, undefined, stopping)}
  <div class="ws" class:live={w.state === "open" && !stopping}>
    <div class="ws-main">
      <span class="ws-label">
        <span class="name" title={w.name}>{w.name}</span>
        <span class="path" title={w.path}>{tildePath(w.path, overview?.home)}</span>
      </span>
      {#if w.id === here}<span class="here-tag">this window</span>{/if}
      {#if activity !== ""}
        <span
          class="meta"
          class:warn={!stopping && w.failed !== undefined}
          class:working={!stopping && w.state === "open" && (w.working ?? 0) > 0}
          title={!stopping ? w.failed || undefined : undefined}>{activity}</span
        >
      {/if}
      {#if !stopping}
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
      {/if}
    </div>
    {#if !stopping && wsError[w.id] !== undefined}
      <div class="row-err">{wsError[w.id]}</div>
    {/if}
  </div>
{/snippet}

<div class="inner">
  <header class="masthead">
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
    <div class="title-row">
      <div class="title">
        <h1>{alias}</h1>
        <div class="kind">
          <span>Slurm cluster</span>
          {#if overview !== null && overview.login_node !== ""}
            <span title="Connected through {overview.login_node}"
              >· via <span class="mono">{shortHost(overview.login_node)}</span></span
            >
          {/if}
          {#if loginServe}
            <span class="pill-warn" title="Allowed on the login node (from the … menu)">login node</span>
          {/if}
        </div>
      </div>
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
    {#if mastError !== null}
      <div class="err-line">{mastError}</div>
    {:else if mastNote !== null}
      <div class="note-line">{mastNote}</div>
    {/if}
    {#if jobsAlive && (notify === "denied" || notify === "not_determined")}
      <!-- A fact with a way to change it — a caption, not an alarm. -->
      <p class="note-line" role="note">
        Notifications are off, so you won't hear when a job starts or is about to end.
        <button class="inline-act" onclick={() => void turnOnNotifications()}>
          {notify === "denied" ? "Open settings" : "Turn on"}
        </button>
      </p>
    {/if}
  </header>

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
        <button class="inline-act" onclick={refresh}>Try again</button>
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
    {#if overview.degraded || (entry !== undefined && entry.error !== null)}
      <div class="status-lines">
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
            <button class="inline-act" onclick={refresh}>Try again</button>
          </div>
        {/if}
      </div>
    {/if}

    {#if jobs.length > 0}
      <section aria-label="Jobs">
        <div class="sec-head">
          <h2 class="sec-title">Jobs</h2>
          {@render refreshButton()}
        </div>
        <div class="surface">
          {#each jobs as j (j.id)}
            {@const busy = jobBusy[j.id] ?? (isJobStopping(j) ? "stopping" : undefined)}
            {@const stopping = isJobStopping(j)}
            {@const inside = wsIn(j)}
            {@const next = continuation(j)}
            <div class="job">
              <div class="job-head">
                <span class="dot {jobDot(j)}" title={stopping ? "stopping" : j.state}></span>
                <span class="job-label">
                  <span class="job-name" title={j.slurm_job_id ? `Slurm job ${j.slurm_job_id}` : j.name}>{j.name}</span>
                  <span class="job-line">{busy === "stopping" || busy === "cancelling" ? "Waiting for Slurm to finish…" : jobStatusLine(j, clusterTime)}</span>
                </span>
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
                      onclick={() => (confirmStop = j)}>Stop</button
                    >
                  {/if}
                </span>
              </div>
              {#if !stopping && j.replaces !== undefined}
                <div class="job-note">
                  Continues {jobName(j.replaces)} — its workspaces move here when this one starts.
                </div>
              {/if}
              {#if !stopping && next !== undefined}
                <div class="job-note">Continuing in a new job — waiting for a node.</div>
              {/if}
              {#if j.state === "running" && j.egress === false}
                <div class="job-note warn">Agents can't reach the internet from this node.</div>
              {/if}
              {#if jobError[j.id] !== undefined}
                <div class="job-note row-err">{jobError[j.id]}</div>
              {/if}
              {#if inside.length > 0 || j.state === "running"}
                <div class="job-ws">
                  {#each inside as w (w.id)}
                    {@render wsRow(w, stopping)}
                  {/each}
                  {#if j.state === "running" && busy === undefined}
                    <button class="add-row" onclick={(e) => openHereMenu(e, j)}>
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
        </div>
      </section>
    {/if}

    {#if ended.length > 0}
      <section aria-label="Ended jobs">
        <div class="sec-head">
          <h2 class="sec-title">Ended</h2>
          {#if ended.length > 1}
            <button class="ghost" disabled={clearing} onclick={() => void clearEnded()}
              >{clearing ? "Clearing…" : "Clear all"}</button
            >
          {/if}
        </div>
        <div class="surface quiet">
          {#each ended as j (j.id)}
            <div class="ended">
              <div class="ended-main">
                <span class="ended-text">{endedLine(j, clusterTime)}</span>
                <span class="acts">
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
                    aria-label="Dismiss {j.name}"
                    title="Dismiss"
                    disabled={jobBusy[j.id] !== undefined}
                    onclick={() => void jobAction(j, "dismissing", () => clusterDismissJob(alias, j.id))}
                  >
                    <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
                      <path d="M4 4l8 8M12 4l-8 8" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
                    </svg>
                  </button>
                </span>
              </div>
              {#if jobError[j.id] !== undefined}
                <div class="row-err">{jobError[j.id]}</div>
              {/if}
            </div>
          {/each}
        </div>
      </section>
    {/if}

    <section aria-label="Not open">
      <div class="sec-head">
        <h2 class="sec-title">{closedWs.length > 0 && closedWs.length < (overview.workspaces.length ?? 0)
            ? "Other workspaces"
            : "Workspaces"}</h2>
        {#if jobs.length === 0}{@render refreshButton()}{/if}
      </div>
      <div class="surface">
        {#each closedWs as w (w.id)}
          {@render wsRow(w)}
        {/each}
        <button class="add-row" onclick={() => (picker = { job: null })}>
          <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
            <path d="M8 3v10M3 8h10" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
          </svg>
          Add a workspace…
        </button>
      </div>
      {#if otherJobsWords(overview.other_jobs) !== ""}
        <p class="caption">{otherJobsWords(overview.other_jobs)}</p>
      {/if}
    </section>
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
    onConfirm={confirmStopNow}
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

<style>
  /* The Home screen's layout language (HomeScreen.svelte): the same column,
     section titles, one bordered surface per section with compact rows (name,
     a muted status, actions at the right end) and the same type scale. The
     values mirror HomeScreen's scoped rules; keep the two in step. */
  .inner {
    container: cluster-page / inline-size;
    box-sizing: border-box;
    width: 100%;
    max-width: calc(860px + 2 * clamp(24px, 4vw, 64px));
    margin: 0 auto;
    /* The top stays clear of the native window's 32px drag strip. */
    padding: 56px clamp(24px, 4vw, 64px) 40px;
    display: flex;
    flex-direction: column;
    --block-gap: 32px;
  }
  /* The status lines belong to the block after them, so they sit 16px
     closer to it than blocks sit to each other. */
  .inner > * + * {
    margin-top: var(--block-gap);
  }
  .inner > .status-lines + * {
    margin-top: calc(var(--block-gap) - 16px);
  }

  .masthead {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 14px;
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

  .title-row {
    align-self: stretch;
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 12px 20px;
  }

  .title {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  h1 {
    margin: 0;
    font-size: calc(var(--text-lg) + 4px);
    font-weight: 550;
    letter-spacing: -0.02em;
    color: var(--fg);
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
    padding: 5px 11px;
    border-radius: 6px;
    cursor: pointer;
    white-space: nowrap;
    transition: border-color 0.12s ease;
  }

  .mast-btn:hover:enabled {
    border-color: color-mix(in srgb, var(--accent) 60%, var(--edge));
  }

  .mast-btn.icon {
    padding: 5px 7px;
    color: var(--muted);
  }

  .mast-btn.icon:hover {
    color: var(--fg);
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

  /* Masthead captions: the partitions note, and notifications being off. */
  .note-line {
    margin: 0;
    font-size: var(--text-xs);
    line-height: 1.5;
    color: var(--muted);
  }

  .inline-act {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    color: var(--fg);
    text-decoration: underline;
    text-decoration-color: color-mix(in srgb, var(--fg) 35%, transparent);
    text-underline-offset: 3px;
    cursor: pointer;
    padding: 0 2px;
  }

  .inline-act:hover {
    color: var(--accent);
    text-decoration-color: currentColor;
  }

  /* The found-from-before login daemon: calm, one fact, one action. */
  .notice {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 14px;
    padding: 11px 14px;
    border-radius: 10px;
    background: color-mix(in srgb, var(--warn) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn) 28%, var(--edge));
  }

  .notice p {
    flex: 1;
    min-width: 220px;
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
    gap: 16px;
    padding: 0 0 4px;
  }

  .sec-title {
    margin: 0;
    font-size: var(--text-md);
    font-weight: 550;
    color: var(--fg);
    letter-spacing: -0.01em;
  }

  /* One surface per section, as Home's workspace list. */
  .surface {
    display: flex;
    flex-direction: column;
    gap: 2px;
    border: 1px solid var(--edge);
    border-radius: 10px;
    padding: 5px;
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

  .ghost:disabled {
    cursor: default;
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

  .status-lines {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .degraded {
    font-size: var(--text-xs);
    color: var(--warn);
  }

  .reading {
    font-size: var(--text-sm);
    color: var(--muted);
    animation: breathe 1.4s ease-in-out infinite;
  }

  @keyframes breathe {
    50% {
      opacity: 0.45;
    }
  }

  .err-line {
    font-size: var(--text-sm);
    color: var(--err);
    white-space: pre-wrap;
  }

  .err-line.quiet {
    font-size: var(--text-xs);
  }

  /* First visit: one invitation, two ways in (Home's empty state). */
  .welcome {
    border: 1px solid var(--edge);
    border-radius: 10px;
    padding: 40px 24px;
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    gap: 12px;
  }

  .welcome h2 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 500;
    letter-spacing: -0.02em;
    color: var(--fg);
  }

  .welcome p {
    margin: 0;
    max-width: 46ch;
    font-size: var(--text-md);
    line-height: 1.6;
    color: var(--muted);
  }

  .welcome-acts {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 8px;
    margin-top: 6px;
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

  .cta.primary {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 12%, var(--overlay-bg));
  }

  /* A job: its row, then the workspaces open in it hanging under it the way
     Home hangs a host's workspaces under the host. */
  .job {
    display: flex;
    flex-direction: column;
    padding: 4px 0 2px;
  }

  .job + .job {
    border-top: 1px solid color-mix(in srgb, var(--edge) 70%, transparent);
    margin-top: 3px;
    padding-top: 7px;
  }

  .job-head {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
    padding: 5px 8px 5px 12px;
  }

  /* The dot sits beside the job's name, not the middle of a wrapped line. */
  .job-head > .dot {
    align-self: flex-start;
    margin-top: 7px;
  }

  .job-label {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .job-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-md);
    font-weight: 550;
    color: var(--fg);
  }

  .job-line {
    font-size: var(--text-xs);
    color: var(--muted);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .job-note {
    padding: 0 12px 2px 31px;
    font-size: var(--text-xs);
    color: var(--muted);
    line-height: 1.5;
  }

  .job-note.warn {
    color: var(--warn);
  }

  .job-note.row-err {
    color: var(--err);
    white-space: pre-wrap;
  }

  .job-ws {
    margin: 4px 8px 4px 26px;
    padding-left: 8px;
    border-left: 1px solid var(--edge);
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  /* A workspace row: Home's one-line row — name and path, a muted status,
     actions at the right end. */
  .ws {
    display: flex;
    flex-direction: column;
    padding: 6px 8px 6px 12px;
    border-radius: 6px;
    transition: background-color 0.12s ease;
  }

  .ws:hover {
    background: color-mix(in srgb, var(--row-hover) 45%, transparent);
  }

  .ws-main {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
    min-height: 26px;
  }

  .ws-label {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 12px;
  }

  .name {
    flex: none;
    max-width: 60%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-md);
    font-weight: 550;
    color: var(--fg);
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
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .meta {
    flex: none;
    max-width: 40%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .meta.working {
    color: var(--accent);
  }

  .meta.warn {
    color: var(--warn);
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
    white-space: nowrap;
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

  .chev-down {
    margin-left: 4px;
  }

  .busy-word {
    font-size: var(--text-xs);
    color: var(--warn);
  }

  .row-err {
    padding-top: 2px;
    font-size: var(--text-xs);
    color: var(--err);
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  /* "+ Open a workspace here" / "+ Add a workspace…": Home's quiet browse
     row, waking on hover. */
  .add-row {
    appearance: none;
    align-self: stretch;
    display: flex;
    align-items: center;
    gap: 7px;
    border: none;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    padding: 6px 12px;
    border-radius: 6px;
    cursor: pointer;
  }

  .add-row:hover {
    color: var(--fg);
    background: color-mix(in srgb, var(--row-hover) 45%, transparent);
  }

  /* Ended jobs: one quiet list, one row each, until dismissed. */
  .ended {
    display: flex;
    flex-direction: column;
    padding: 5px 8px 5px 12px;
    border-radius: 6px;
  }

  .ended:hover {
    background: color-mix(in srgb, var(--row-hover) 45%, transparent);
  }

  .ended-main {
    display: flex;
    align-items: center;
    gap: 12px;
    min-width: 0;
  }

  .ended-text {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--muted);
  }

  /* The quiet surface's row actions stay muted until the row is hovered. */
  .surface.quiet .act {
    border-color: transparent;
    background: none;
  }

  .surface.quiet .ended:hover .act,
  .surface.quiet .act:focus-visible {
    border-color: var(--edge);
    background: var(--overlay-bg);
  }

  .caption {
    margin: 4px 0 0;
    padding: 0 2px;
    font-size: var(--text-xs);
    color: var(--muted);
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

  button:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 2px;
  }

  /* A narrow page (560 px or less, e.g. in a split pane): rows wrap their
     status and actions under the name instead of squeezing it. */
  @container cluster-page (max-width: 560px) {
    .ws-main,
    .ended-main {
      flex-wrap: wrap;
      row-gap: 4px;
    }

    .ws-label {
      flex-basis: 100%;
    }

    .meta {
      flex: 1;
      max-width: none;
    }

    .ended-text {
      flex: 1 1 240px;
    }

    .ended-main .acts {
      margin-left: auto;
    }

    .job-head {
      flex-wrap: wrap;
      row-gap: 6px;
    }

    .job-head .acts {
      margin-left: 19px;
    }

    .job-ws {
      margin-left: 14px;
    }
  }

  /* The page's own padding follows the window, as Home's does: `.inner` is
     the size container, and a container cannot query itself. */
  @media (max-width: 700px) {
    .inner {
      padding: 24px 20px 20px;
      --block-gap: 28px;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .dot {
      animation: none;
    }
  }
</style>

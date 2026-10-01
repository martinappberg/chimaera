<script lang="ts">
  /**
   * A cluster host's page, shown in place inside the local home. Workspace-
   * first: each workspace is running / starting / waiting / stopped, and its
   * job is a detail of it ("running on n042 · 4 CPU · 16 GB · 5d 22h left").
   * Nothing here runs on the login node: every action is one short command
   * the app runs over ssh while it is open (docs/hpc-portal-plan.md §2).
   *
   * Polling: the overview on mount, every 60 s while visible, right after an
   * action, and on the shell's `cluster-changed`. The shell asks the queue at
   * most once a minute whatever we call; a degraded round keeps the last read
   * on screen and says so.
   */
  import { onMount } from "svelte";
  import {
    clusterAddWorkspace,
    clusterFacts,
    clusterOpen,
    clusterOpenTerminal,
    clusterRemoveWorkspace,
    clusterSetLoginServe,
    clusterStop,
    clusterStopLoginDaemon,
    onClusterChanged,
    type ClusterWorkspaceView,
    type HostState,
    type StartResult,
  } from "../net/native";
  import { asyncDisposer } from "../shared/asyncDisposer";
  import { pageVisible } from "../shared/visibility";
  import { contextMenu, type ContextMenuEntry } from "../shared/contextMenu.svelte";
  import ConfirmDialog from "../shared/ConfirmDialog.svelte";
  import { clusterNow, clusterOverviews } from "./clusterStore.svelte";
  import { agoWords, otherJobsWords, stateDot, workspaceDetail, workspaceNotes } from "./cluster";
  import ClusterStartSheet from "./ClusterStartSheet.svelte";
  import ClusterSettingDialog from "./ClusterSettingDialog.svelte";
  import ClusterFiles from "./ClusterFiles.svelte";

  interface Props {
    alias: string;
    /** The host's shell state (cluster info: login daemon, override). */
    host: HostState | null;
    /** Back to the host list. */
    onBack: () => void;
    /** The shell returned a new state for this host (the override toggle). */
    onHostState: (state: HostState) => void;
    /** Something changed the host list (a login daemon was shut down). */
    onHostsChanged: () => void;
  }

  let { alias, host, onBack, onHostState, onHostsChanged }: Props = $props();

  const entry = $derived(clusterOverviews.entry(alias));
  const overview = $derived(entry?.overview ?? null);
  /** Re-read once a minute while visible, so countdowns move between polls. */
  let now = $state(Date.now());
  const clusterTime = $derived(entry === undefined ? now : clusterNow(entry, now));

  const loginServe = $derived(host?.cluster?.login_serve === true);
  const loginDaemon = $derived(loginServe ? null : (host?.cluster?.login_daemon ?? null));

  // --- per-row action state ----------------------------------------------------
  type RowBusy = "opening" | "stopping" | "cancelling" | "removing";
  let rowBusy = $state<Record<string, RowBusy>>({});
  let rowError = $state<Record<string, string>>({});
  /** A stop just sent: the row says "stopping…" while it still shows the
   *  state it was stopped from (the queue answers within a minute). */
  let pendingStop = $state<Record<string, { from: string; at: number }>>({});
  /** A start just submitted (`at` on the cluster's clock): said in place of
   *  the stopped line until the row moves, a later end shows, or ten
   *  minutes pass — never left covering a job that failed at once. */
  let justStarted = $state<Record<string, { text: string; at: number }>>({});

  let startFor = $state<ClusterWorkspaceView | null>(null);
  let confirmStop = $state<ClusterWorkspaceView | null>(null);
  let confirmStopError = $state<string | null>(null);
  let confirmRemove = $state<ClusterWorkspaceView | null>(null);
  let confirmRemoveError = $state<string | null>(null);
  let confirmLoginServe = $state(false);
  let loginServeError = $state<string | null>(null);
  let setting = $state<"startup" | "rules" | null>(null);

  let adding = $state(false);
  let addPath = $state("");
  let addName = $state("");
  let addPathError = $state<string | null>(null);
  let addBusy = $state(false);
  let addPathEl = $state<HTMLInputElement | null>(null);

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

  function errText(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  function setBusy(id: string, b: RowBusy | null): void {
    const next = { ...rowBusy };
    if (b === null) delete next[id];
    else next[id] = b;
    rowBusy = next;
  }

  function setRowError(id: string, msg: string | null): void {
    const next = { ...rowError };
    if (msg === null) delete next[id];
    else next[id] = msg;
    rowError = next;
  }

  async function open(w: ClusterWorkspaceView): Promise<void> {
    if (rowBusy[w.id] !== undefined) return;
    setRowError(w.id, null);
    setBusy(w.id, "opening");
    try {
      await clusterOpen(alias, w.id);
    } catch (e) {
      setRowError(w.id, errText(e));
    } finally {
      setBusy(w.id, null);
    }
  }

  /** Stop (or cancel) a workspace's job; throws for the caller to show. */
  async function stop(w: ClusterWorkspaceView, kind: "stopping" | "cancelling"): Promise<void> {
    setRowError(w.id, null);
    setBusy(w.id, kind);
    try {
      await clusterStop(alias, w.id);
      pendingStop = { ...pendingStop, [w.id]: { from: w.state, at: Date.now() } };
    } finally {
      setBusy(w.id, null);
      refresh();
    }
  }

  async function confirmStopNow(): Promise<void> {
    const w = confirmStop;
    if (w === null) return;
    confirmStopError = null;
    try {
      await stop(w, "stopping");
      confirmStop = null;
    } catch (e) {
      confirmStopError = errText(e);
    }
  }

  async function cancelWaiting(w: ClusterWorkspaceView): Promise<void> {
    try {
      await stop(w, "cancelling");
    } catch (e) {
      setRowError(w.id, errText(e));
    }
  }

  async function removeNow(): Promise<void> {
    const w = confirmRemove;
    if (w === null) return;
    confirmRemoveError = null;
    setBusy(w.id, "removing");
    try {
      await clusterRemoveWorkspace(alias, w.id);
      confirmRemove = null;
      refresh();
    } catch (e) {
      confirmRemoveError = errText(e);
    } finally {
      setBusy(w.id, null);
    }
  }

  function started(w: ClusterWorkspaceView, result: Exclude<StartResult, { kind: "refused" }>): void {
    startFor = null;
    const next = { ...pendingStop };
    delete next[w.id];
    pendingStop = next;
    justStarted = {
      ...justStarted,
      [w.id]: {
        text:
          result.kind === "attached"
            ? "started attached — stops when you disconnect"
            : "submitted · waiting for a node",
        at: clusterTime,
      },
    };
    refresh();
  }

  function rowMenu(e: MouseEvent, w: ClusterWorkspaceView): void {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    contextMenu.openAtPoint(
      r.right,
      r.bottom + 4,
      [
        {
          label: "Remove…",
          danger: true,
          onSelect: () => {
            confirmRemoveError = null;
            confirmRemove = w;
          },
        },
      ],
      { alignRight: true },
    );
  }

  async function addWorkspace(): Promise<void> {
    if (addBusy) return;
    const path = addPath.trim();
    if (path === "") {
      addPathError = "Enter a folder on the cluster.";
      addPathEl?.focus();
      return;
    }
    addPathError = null;
    addBusy = true;
    try {
      await clusterAddWorkspace(alias, path, addName.trim());
      addPath = "";
      addName = "";
      adding = false;
      refresh();
    } catch (e) {
      addPathError = errText(e);
      addPathEl?.focus();
    } finally {
      addBusy = false;
    }
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
        onSelect: () => (setting = "startup"),
      },
      {
        label: "Rules for agents…",
        disabled: waiting,
        hint: waiting ? "Waiting for the cluster" : undefined,
        onSelect: () => (setting = "rules"),
      },
      { label: "Refresh partitions", onSelect: () => void refreshPartitions() },
      "separator",
      {
        label: "Run chimaera on the login node",
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
    contextMenu.openAtPoint(r.right, r.bottom + 4, items, { alignRight: true });
  }

  function startedNote(w: ClusterWorkspaceView): string | undefined {
    const j = justStarted[w.id];
    if (j === undefined || w.state !== "stopped") return undefined;
    if (w.ended_at_ms !== undefined && w.ended_at_ms >= j.at) return undefined;
    return clusterTime - j.at < 600_000 ? j.text : undefined;
  }

  /** "stopping…" while a sent stop hasn't shown in the queue yet (≤ 3 min). */
  function isStopping(w: ClusterWorkspaceView): boolean {
    const p = pendingStop[w.id];
    return p !== undefined && p.from === w.state && now - p.at < 180_000;
  }

  const places = $derived((overview?.workspaces ?? []).map((w) => ({ name: w.name, path: w.path })));
</script>

<div class="inner">
  <header class="masthead">
    <div class="topline">
      <button class="back-home" aria-label="Back to Home" title="Back to Home" onclick={onBack}>
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
      <div class="mast-acts">
        <button class="mast-btn" title="An interactive shell on {alias}'s login node" onclick={() => void openTerminal()}>
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
          onclick={openMore}
        >
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
            <circle cx="3.5" cy="8" r="1.2" fill="currentColor" />
            <circle cx="8" cy="8" r="1.2" fill="currentColor" />
            <circle cx="12.5" cy="8" r="1.2" fill="currentColor" />
          </svg>
        </button>
      </div>
    </div>
    <h1>{alias}</h1>
    <div class="kind">
      <span class="sched">Slurm cluster</span>
      <span>· chimaera runs on compute nodes only</span>
      {#if loginServe}
        <span class="pill-warn" title="Allowed on the login node (from the … menu)">login node</span>
      {/if}
    </div>
    {#if overview !== null}
      <div class="login-line">connected through <span class="mono">{overview.login_node}</span></div>
    {/if}
    {#if mastError !== null}
      <div class="err-line">{mastError}</div>
    {:else if mastNote !== null}
      <div class="note-line">{mastNote}</div>
    {/if}
  </header>

  {#if loginDaemon !== null}
    <div class="notice" role="note">
      <p>
        A chimaera server from before is still running on <span class="mono">{loginDaemon.node}</span>.
        Clusters don't allow servers on login nodes.
      </p>
      <button class="notice-act" disabled={daemonBusy} onclick={() => void shutDownLoginDaemon()}>
        {daemonBusy ? "Shutting down…" : "Shut it down"}
      </button>
      {#if daemonError !== null}<div class="notice-err">{daemonError}</div>{/if}
    </div>
  {/if}

  <section>
    <div class="sec-head">
      <span class="sec-title">workspaces</span>
      <span class="sec-acts">
        <button
          class="ghost"
          onclick={() => {
            adding = !adding;
            addPathError = null;
          }}>add a workspace…</button
        >
        <button
          class="ghost refresh"
          class:spinning={entry?.loading === true}
          title="Refresh"
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
      </span>
    </div>

    {#if adding}
      <form
        class="add"
        onsubmit={(e) => {
          e.preventDefault();
          void addWorkspace();
        }}
      >
        <div class="add-row">
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="add-input path-field"
            class:bad={addPathError !== null}
            bind:value={addPath}
            bind:this={addPathEl}
            placeholder="$SCRATCH/project or ~/project"
            aria-label="Folder on the cluster"
            spellcheck="false"
            autocomplete="off"
            autofocus
            oninput={() => (addPathError = null)}
            onkeydown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                adding = false;
              }
            }}
          />
          <input
            class="add-input name-field"
            bind:value={addName}
            placeholder="name (optional)"
            aria-label="Name"
            spellcheck="false"
            autocomplete="off"
          />
          <button class="cta small" type="submit" disabled={addBusy}>{addBusy ? "Adding…" : "Add"}</button>
        </div>
        {#if addPathError !== null}
          <div class="field-err">{addPathError}</div>
        {:else}
          <div class="field-hint">A folder on {alias}'s shared filesystem. Nothing in it is changed.</div>
        {/if}
      </form>
    {/if}

    {#if overview?.degraded}
      <div class="degraded" role="status">
        The queue didn't answer; showing the last read{overview.queue_at_ms > 0
          ? ` (${agoWords(overview.queue_at_ms, clusterTime)})`
          : ""}.
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
    {:else}
      {#if entry !== undefined && entry.error !== null}
        <div class="err-line quiet">Couldn't refresh: {entry.error}</div>
      {/if}
      {#if overview.workspaces.length === 0 && !adding}
        <div class="blank">
          <p>No workspaces yet. Add a folder on the cluster; each workspace runs as its own Slurm job.</p>
          <button class="cta" onclick={() => (adding = true)}>Add a workspace</button>
        </div>
      {:else}
        <div class="rows">
          {#each overview.workspaces as w, i (`${i}:${w.id}`)}
            {@const busy = rowBusy[w.id]}
            {@const stopping = isStopping(w)}
            {@const notes = workspaceNotes(w)}
            {@const startNote = startedNote(w)}
            <div class="ws" class:live={w.state === "running"}>
              <div class="ws-main">
                <span class="dot {stopping ? 'ending' : stateDot(w.state)}" title={w.state}></span>
                <span class="name" title={w.name}>{w.name}</span>
                <span class="path" title={w.path}>{w.path}</span>
                <span class="acts">
                  {#if stopping}
                    <span class="busy-word">stopping…</span>
                  {:else if w.state === "running"}
                    <button class="act primary" disabled={busy !== undefined} onclick={() => void open(w)}
                      >{busy === "opening" ? "Opening…" : "Open"}</button
                    >
                    <button
                      class="act"
                      disabled={busy !== undefined}
                      onclick={() => {
                        confirmStopError = null;
                        confirmStop = w;
                      }}>{busy === "stopping" ? "Stopping…" : "Stop"}</button
                    >
                  {:else if w.state === "starting"}
                    <button
                      class="act"
                      disabled={busy !== undefined}
                      onclick={() => {
                        confirmStopError = null;
                        confirmStop = w;
                      }}>{busy === "stopping" ? "Stopping…" : "Stop"}</button
                    >
                  {:else if w.state === "waiting"}
                    <button class="act" disabled={busy !== undefined} onclick={() => void cancelWaiting(w)}
                      >{busy === "cancelling" ? "Cancelling…" : "Cancel"}</button
                    >
                  {:else}
                    <button
                      class="act primary"
                      disabled={busy !== undefined || startNote !== undefined}
                      onclick={() => (startFor = w)}>{w.fresh ? "Start" : "Start again"}</button
                    >
                    <button
                      class="act icon"
                      aria-label="More for {w.name}"
                      aria-haspopup="menu"
                      title="More"
                      disabled={busy !== undefined}
                      onclick={(e) => rowMenu(e, w)}
                    >
                      <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
                        <circle cx="3.5" cy="8" r="1.2" fill="currentColor" />
                        <circle cx="8" cy="8" r="1.2" fill="currentColor" />
                        <circle cx="12.5" cy="8" r="1.2" fill="currentColor" />
                      </svg>
                    </button>
                  {/if}
                </span>
              </div>
              <div class="detail">
                {startNote ?? workspaceDetail(w, clusterTime)}
              </div>
              {#each notes as n, j (j)}
                <div class="detail note" class:warn={n.warn}>{n.text}</div>
              {/each}
              {#if rowError[w.id] !== undefined}
                <div class="detail row-err">{rowError[w.id]}</div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
      <div class="other-jobs">{otherJobsWords(overview.other_jobs)}</div>
    {/if}
  </section>

  <ClusterFiles {alias} {places} />
</div>

{#if startFor !== null && overview !== null}
  {@const w = startFor}
  <ClusterStartSheet
    {alias}
    workspace={w}
    config={overview.config}
    onStarted={(r) => started(w, r)}
    onClose={() => (startFor = null)}
  />
{/if}

{#if setting !== null && overview !== null}
  <ClusterSettingDialog
    {alias}
    mode={setting}
    startup={overview.config.startup}
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
    body="Its job ends; chats are saved."
    confirmLabel="Stop"
    danger
    error={confirmStopError}
    onConfirm={() => void confirmStopNow()}
    onCancel={() => (confirmStop = null)}
  />
{/if}

{#if confirmRemove !== null}
  <ConfirmDialog
    title={`Remove ${confirmRemove.name}?`}
    body={`This removes chimaera's folder for it on ${alias} — its saved chats and setup. The project folder ${confirmRemove.path} is untouched.`}
    confirmLabel="Remove"
    danger
    error={confirmRemoveError}
    onConfirm={() => void removeNow()}
    onCancel={() => (confirmRemove = null)}
  />
{/if}

{#if confirmLoginServe}
  <ConfirmDialog
    title="Run chimaera on the login node?"
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

  .login-line {
    font-size: var(--text-xs);
    color: var(--muted);
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

  .blank {
    border: 1px dashed var(--edge);
    border-radius: 8px;
    padding: 24px;
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
    max-width: 44ch;
    line-height: 1.5;
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
</style>

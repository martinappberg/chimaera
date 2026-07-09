<script lang="ts">
  import { onMount } from "svelte";
  import BrandMark from "./BrandMark.svelte";
  import { keyHint } from "./keybindings";
  import { needsAttention, type Session, type Workspace } from "./sessions";
  import {
    addHost,
    beginUpdate,
    checkAppUpdate,
    connectHost,
    disconnectHost,
    endHostSessions,
    isNativeShell,
    listHosts,
    localDaemonState,
    onConnectProgress,
    onHostStatus,
    openWindow,
    remoteWorkspaces,
    removeHost,
    shutdownHost,
    updateLocalDaemon,
    type ConnectProgress,
    type HostState,
    type LocalDaemonState,
  } from "./native";
  import type { Health } from "./api";

  interface Props {
    workspaces: Workspace[];
    sessions: Session[];
    hostLabel: string;
    health: Health | null;
    /** The authenticated events socket is up (daemon reachable). */
    connected: boolean;
    /** Open `w` in THIS window. */
    onOpen: (w: Workspace) => void;
    /** Remove `w` from the daemon's registry (files untouched). */
    onRemove: (w: Workspace) => void;
    /** End every live session in `w` (the registration itself is untouched). */
    onStop: (w: Workspace) => void;
    /** Open the folder picker (browse/register a new folder). */
    onOpenFolder: () => void;
  }

  let {
    workspaces,
    sessions,
    hostLabel,
    health,
    connected,
    onOpen,
    onRemove,
    onStop,
    onOpenFolder,
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

  const sorted = $derived(
    [...workspaces].sort((a, b) => (b.last_opened_at ?? 0) - (a.last_opened_at ?? 0)),
  );

  /** Live rollup per workspace: total live sessions + how many need you. */
  const liveByWs = $derived.by(() => {
    const map = new Map<string, { live: number; attn: number }>();
    for (const s of sessions) {
      const entry = map.get(s.workspace_id) ?? { live: 0, attn: 0 };
      if (s.alive) entry.live += 1;
      if (needsAttention(s)) entry.attn += 1;
      map.set(s.workspace_id, entry);
    }
    return map;
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
  let addOpen = $state(false);
  let addAlias = $state("");
  let addError = $state<string | null>(null);
  let confirmForget = $state<string | null>(null);
  /** Host pending a "end all sessions" / "shut down" confirm (alias). */
  let confirmEnd = $state<string | null>(null);
  let confirmShutdown = $state<string | null>(null);

  // --- local daemon build parity (native shell, local window only) ------------

  let localState = $state<LocalDaemonState | null>(null);
  let localUpdating = $state(false);
  let localError = $state<string | null>(null);

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

  /** Current live sessions on the daemon serving this window. */
  const liveNow = $derived(sessions.filter((s) => s.alive).length);

  const PHASE_LABEL: Record<ConnectProgress["phase"], string> = {
    probing: "probing for a running daemon…",
    updating: "updating the daemon…",
    downloading: "downloading chimaera…",
    installing: "installing chimaera…",
    starting: "starting the daemon…",
    tunneling: "bringing the tunnel up…",
  };

  onMount(() => {
    if (!native) return;
    void refreshHosts();
    // Only the local window asks about the local daemon's build parity.
    if (hostLabel === "local") {
      void localDaemonState().then((s) => (localState = s));
      // Quietly ask GitHub whether a newer signed app build exists.
      void checkAppUpdate().then((v) => (appUpdate = v));
    }
    const unlisteners: Array<() => void> = [];
    void onConnectProgress((p) => {
      phases = new Map(phases).set(p.alias, PHASE_LABEL[p.phase] ?? p.phase);
    }).then((u) => unlisteners.push(u));
    // Keep host rows live: the shell's health monitor reports a dropped or
    // recovered tunnel, and connect flights report their outcome — including
    // ones this window didn't start (startup restore, another window's
    // reconnect), which otherwise leave the row "connecting" forever.
    void onHostStatus((e) => {
      hosts = hosts.map((h) =>
        h.alias === e.alias
          ? {
              ...h,
              status: e.status === "connected" ? "connected" : "disconnected",
              local_port: e.status === "connected" ? (e.local_port ?? h.local_port) : null,
            }
          : h,
      );
      // Any terminal transition ends the phase line, whoever ran the connect.
      phases = mapWithout(phases, e.alias);
      if (e.status === "down") remoteWs = mapWithout(remoteWs, e.alias);
      if (e.status === "error" && e.error !== undefined) {
        hostErrors = new Map(hostErrors).set(e.alias, e.error);
      } else if (e.status === "connected") {
        hostErrors = mapWithout(hostErrors, e.alias);
        // A connect this window didn't run (startup restore, another
        // window) still gets its workspace list, so the row is browsable.
        if (!remoteWs.has(e.alias)) {
          void remoteWorkspaces(e.alias)
            .then((list) => {
              remoteWs = new Map(remoteWs).set(
                e.alias,
                [...list].sort((a, b) => (b.last_opened_at ?? 0) - (a.last_opened_at ?? 0)),
              );
            })
            .catch(() => {
              // dropped again in between; the next transition retries
            });
        }
      }
    }).then((u) => unlisteners.push(u));
    return () => unlisteners.forEach((u) => u());
  });

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
      const list = await remoteWorkspaces(alias);
      remoteWs = new Map(remoteWs).set(
        alias,
        [...list].sort((a, b) => (b.last_opened_at ?? 0) - (a.last_opened_at ?? 0)),
      );
    } catch (e) {
      hostErrors = new Map(hostErrors).set(alias, e instanceof Error ? e.message : String(e));
      void refreshHosts();
    } finally {
      phases = mapWithout(phases, alias);
    }
  }

  async function disconnect(alias: string): Promise<void> {
    await disconnectHost(alias);
    remoteWs = mapWithout(remoteWs, alias);
    void refreshHosts();
  }

  async function forget(alias: string): Promise<void> {
    confirmForget = null;
    await removeHost(alias);
    remoteWs = mapWithout(remoteWs, alias);
    void refreshHosts();
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

  /** Shorten an absolute path with ~ for scanability. */
  function tildify(path: string): string {
    const m = path.match(/^\/(?:home|Users)\/[^/]+(\/.*)?$/);
    return m ? `~${m[1] ?? ""}` : path;
  }

  function openRow(e: MouseEvent, w: Workspace): void {
    if (e.metaKey || e.ctrlKey) {
      // Cmd/Ctrl-click is the explicit "give me another window" gesture — on
      // THIS screen's own daemon (see ownAlias).
      void openWindow(ownAlias, w.id, true);
    } else {
      onOpen(w);
    }
  }
</script>

<div class="home">
  {#if health !== null}
    <!-- The mark identifies the DAEMON serving this window (the daemon
         outlives app reinstalls by design, so this is the version that
         actually matters — and a dev daemon must say so instead of posing
         as an ordinary "v0.0.1"). -->
    {#if health.version === "0.0.1"}
      <span
        class="version-mark"
        title="this window is served by a development daemon (build {health.build ?? 'unknown'})"
        >daemon dev·{(health.build ?? "unknown").split(".")[0]}</span
      >
    {:else}
      <span class="version-mark" title="daemon version">v{health.version}</span>
    {/if}
  {/if}
  <div class="inner">
    <header class="masthead">
      <div class="brand">
        <BrandMark size={24} draw title="chimaera" />
        <h1>chimaera</h1>
      </div>
      <div class="where" title={health?.hostname}>
        <span class="daemon-dot" class:ok={connected} aria-hidden="true"></span>
        <span class="host-label">{hostLabel}</span>
        {#if health !== null && health.hostname !== hostLabel}
          <span class="hostname">{health.hostname}</span>
        {/if}
      </div>
      {#if native && hostLabel === "local" && localState?.outdated}
        <div class="update-line" title={buildNote(localState.build)}>
          <span>daemon is an older build —</span>
          <button class="update-act" disabled={localUpdating} onclick={() => void updateLocal()}>
            {localUpdating ? "updating…" : `update${endsLabel(liveNow)}`}
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

    <section>
      <div class="sec-head">
        <span class="sec-title">workspaces</span>
        <button class="ghost" onclick={onOpenFolder}
          >open a folder… <kbd>{keyHint("picker")}</kbd></button
        >
      </div>
      {#if sorted.length === 0}
        <div class="blank">
          <p>Nothing here yet — open a folder to start terminals and agents in it.</p>
          <button class="cta" onclick={onOpenFolder}>Open a folder</button>
        </div>
      {:else}
        <div class="rows">
          {#each sorted as w (w.id)}
            {@const live = liveByWs.get(w.id)}
            {@const wsState = live && live.attn > 0 ? "attn" : live && live.live > 0 ? "alive" : ""}
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
                  }}>end sessions</button
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
              <div class="rowwrap" role="presentation" class:live={wsState === "alive"} class:attn={wsState === "attn"}>
                <button class="row" title={w.root} onclick={(e) => openRow(e, w)}>
                  <span
                    class="dot {wsState}"
                    title={wsState === "attn"
                      ? `${live?.attn} need${live?.attn === 1 ? "s" : ""} you`
                      : wsState === "alive"
                        ? `${live?.live} live session${live?.live === 1 ? "" : "s"}`
                        : "no live sessions"}
                  ></span>
                  <span class="name">{w.name}</span>
                  <span class="path">{tildify(w.root)}</span>
                  {#if live !== undefined && live.attn > 0}
                    <span class="badge attn" title="{live.attn} need{live.attn === 1 ? 's' : ''} you">
                      <span class="dot attn"></span>{live.attn}
                    </span>
                  {/if}
                  {#if live !== undefined && live.live > 0}
                    <span
                      class="badge"
                      title="{live.live} live session{live.live === 1 ? '' : 's'}"
                    >
                      <span class="dot alive"></span>{live.live}
                    </span>
                  {/if}
                  <span class="when">{ago(w.last_opened_at)}</span>
                </button>
                {#if live !== undefined && live.live > 0}
                  <button
                    class="side stop shown"
                    title="end this workspace's {live.live} running session{live.live === 1
                      ? ''
                      : 's'}"
                    onclick={() => (confirmStopId = w.id)}>stop</button
                  >
                {/if}
                <button
                  class="side"
                  title="open in a new window"
                  onclick={() => void openWindow(ownAlias, w.id, true)}>new window</button
                >
                <button
                  class="side x"
                  title="remove from this list (folder untouched)"
                  onclick={() => (confirmRemoveId = w.id)}>&times;</button
                >
              </div>
            {/if}
          {/each}
        </div>
      {/if}
    </section>

    <section>
      <div class="sec-head">
        <span class="sec-title">remote hosts</span>
        {#if native}
          <button
            class="ghost"
            onclick={() => {
              addOpen = !addOpen;
              addError = null;
            }}>add a host…</button
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
              placeholder="ssh alias or user@host (from your ~/.ssh/config)"
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
              >connect</button
            >
          </form>
          {#if addError !== null}
            <div class="err-line">{addError}</div>
          {/if}
        {/if}

        {#if hosts.length === 0 && !addOpen}
          <p class="hint">
            No remotes yet. Add your cluster's ssh alias — chimaera installs its own daemon in
            <code>~/.chimaera</code> over ssh, no root needed.
          </p>
        {:else}
          <div class="rows">
            {#each hosts as h (h.alias)}
              {@const phase = phases.get(h.alias)}
              {@const err = hostErrors.get(h.alias)}
              {@const ws = remoteWs.get(h.alias)}
              {#if confirmShutdown === h.alias}
                <div class="row confirm strong" role="alertdialog" aria-label="shut down host?">
                  <span class="name">{h.alias}</span>
                  <span class="confirm-label"
                    >shut down — end {h.live_sessions ?? 0} session{h.live_sessions === 1
                      ? ""
                      : "s"} and stop the daemon?</span
                  >
                  <button class="confirm-yes" onclick={() => void shutdown(h.alias)}>shut down</button
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
                    >end sessions</button
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
              {:else}
                <div class="rowwrap" role="presentation" class:connected={h.status === "connected"}>
                  <button
                    class="row"
                    title={h.status === "connected"
                      ? `browse ${h.alias}`
                      : `connect to ${h.alias}`}
                    disabled={h.status === "connecting"}
                    onclick={() =>
                      h.status === "connected"
                        ? void openWindow(h.alias, null)
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
                    <span class="name">{h.alias}</span>
                    {#if phase !== undefined}
                      <span class="phase">{phase}</span>
                    {:else if h.status === "connected"}
                      <span class="phase quiet">connected · 127.0.0.1:{h.local_port}</span>
                    {:else}
                      <span class="when">{ago(h.last_connected_at)}</span>
                    {/if}
                    {#if h.status === "connected" && (h.live_sessions ?? 0) > 0}
                      <span
                        class="badge"
                        title="{h.live_sessions} live session{h.live_sessions === 1
                          ? ''
                          : 's'} on {h.alias}"
                      >
                        <span class="dot alive"></span>{h.live_sessions}
                      </span>
                    {/if}
                  </button>
                  {#if h.status === "connected"}
                    {#if (h.live_sessions ?? 0) > 0}
                      <button
                        class="side"
                        title="end all sessions on {h.alias} — the daemon keeps running"
                        onclick={() => (confirmEnd = h.alias)}>end sessions</button
                      >
                    {/if}
                    <button
                      class="side"
                      title="close the tunnel — sessions keep running on {h.alias}"
                      onclick={() => void disconnect(h.alias)}>disconnect</button
                    >
                    <button
                      class="side stop"
                      title="shut down {h.alias} — end all sessions and stop the daemon"
                      onclick={() => (confirmShutdown = h.alias)}>shut down</button
                    >
                  {/if}
                  <button class="side x" title="forget host" onclick={() => (confirmForget = h.alias)}
                    >&times;</button
                  >
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
                          onclick={() => void openWindow(h.alias, rw.id)}
                        >
                          <span class="name">{rw.name}</span>
                          <span class="path">{tildify(rw.root)}</span>
                          <span class="when">{ago(rw.last_opened_at)}</span>
                        </button>
                      </div>
                    {/each}
                    <div class="rowwrap" role="presentation">
                      <button class="row sub browse" onclick={() => void openWindow(h.alias, null)}>
                        <span class="name">browse {h.alias}…</span>
                      </button>
                    </div>
                  </div>
                {/if}
              {/if}
            {/each}
          </div>
        {/if}
      {/if}
    </section>
  </div>
</div>

<style>
  .home {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    background: var(--bg);
  }

  .inner {
    max-width: 640px;
    margin: 0 auto;
    padding: clamp(24px, 10vh, 96px) 24px 64px;
    display: flex;
    flex-direction: column;
    gap: 36px;
  }

  .masthead {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
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

  .brand {
    display: flex;
    align-items: center;
    gap: 9px;
  }

  /* Quiet running-version stamp, pinned to the home screen's corner. */
  .version-mark {
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

  h1 {
    margin: 0;
    font-size: 20px;
    font-weight: 600;
    letter-spacing: 0.01em;
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

  .host-label {
    color: var(--fg);
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
    padding: 0 8px 4px;
  }

  .sec-title {
    font-size: var(--text-xs);
    color: var(--muted);
    text-transform: lowercase;
    letter-spacing: 0.04em;
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
    font-size: 10px;
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 3px;
    padding: 0 3px;
    margin-left: 2px;
  }

  .blank {
    border: 1px dashed var(--edge);
    border-radius: 8px;
    padding: 28px 24px;
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

  /* A connected host wears its alias in the accent — the same "this is live"
     language the rail uses for a remote window's host label. */
  .rowwrap.connected > .row > .name {
    color: var(--accent);
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

  .badge.attn {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 40%, transparent);
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

  /* Inside a count pill the halo would clip against the border — the pill's
     own tint already carries the state, so the inner dot stays flat. */
  .badge .dot {
    width: 6px;
    height: 6px;
    box-shadow: none;
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

  .err-line {
    padding: 2px 10px 6px;
    font-size: var(--text-sm);
    color: var(--err);
    white-space: pre-wrap;
  }
</style>

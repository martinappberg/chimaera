<script lang="ts">
  import { keepsPaneViewAlive, tabKey, MAX_PANES, type PaneNode, type Tab } from "./layout";
  import { untrack, type Component } from "svelte";
  import { get } from "svelte/store";
  import { dirtyFiles } from "../shared/editing";
  import { sessionLabel, type Session } from "../workspace/sessions";
  import type { DropSpot, LayoutCtrl } from "./dnd";
  import { registerPane, unregisterPane, zoneWord } from "./dnd";
  import { dirLabel } from "../previews/files";
  import { pauseLabel, placementLabel, sessionPause, shellPauseLabel } from "../net/placement";
  import { accountSignedOut, proPossible } from "../net/plan";
  import { terminalKept, terminalStatus } from "../terminal/refusals.svelte";
  import { pausedConnect } from "../pro/providers";
  import { canOpenOnboarding, cloudOnboarding } from "../pro/onboarding.svelte";
  import { agentHue, type LinkCtrl } from "../workspace/agentLinks";
  import { activeModLabel, keyHint, keyHintSuffix, paneFocusHint, paneMoveHint } from "../shared/keybindings";
  import PaneTabs from "./PaneTabs.svelte";
  import { hintsActive, paneHintsActive } from "../shared/chordHints.svelte";
  import { loadPaneView, retryPaneView, type PaneViewKind } from "./lazyViews";
  import {
    clearChunkFailure,
    noteChunkFailure,
    requestAssetReload,
  } from "./assetTransition";
  import Spinner from "../previews/Spinner.svelte";
  import { modalFocus } from "../shared/modalFocus";
  import { browserOpener } from "../browser/agentOpen";
  import { findTargetsChanged, targetIn } from "../shared/find";
  import type { DashCtx } from "../dashboard/dash";

  interface Props {
    node: PaneNode;
    focusedPaneId: string;
    /** True when this pane is rendered zoomed (fullscreen in the window). */
    zoomed?: boolean;
    dropSpot: DropSpot | null;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    fileNames: Map<string, string>;
    /** terminal session id -> agent session id (linked-terminal edges). */
    links: Map<string, string>;
    linkCtrl: LinkCtrl;
    /** Active workspace root (touched-files paths relativize against it). */
    wsRoot: string | null;
    /** Active workspace id (the git surfaces query the daemon with it). */
    wsId: string | null;
    /** Panes whose bottom band is armed for the current drag. */
    bandPanes: ReadonlySet<string>;
    /** App-level context for the dashboard surface. */
    dash: DashCtx;
    ctrl: LayoutCtrl;
  }

  let {
    node,
    focusedPaneId,
    zoomed = false,
    dropSpot,
    sessions,
    names,
    fileNames,
    links,
    linkCtrl,
    wsRoot,
    wsId,
    bandPanes,
    dash,
    ctrl,
  }: Props = $props();

  const focused = $derived(node.id === focusedPaneId);
  const activeTab = $derived(node.tabs[node.active] ?? null);
  const fileTrail = $derived(activeTab?.surface === "file" ? activeTab.fileTrail : undefined);
  const historyVisible = $derived(fileTrail !== undefined && (
    fileTrail.paths[fileTrail.index - 1] !== undefined || fileTrail.paths[fileTrail.index + 1] !== undefined
  ));
  const paneNumber = $derived(ctrl.paneTargets().find((p) => p.id === node.id)?.number ?? 1);
  let canFind = $state(false);
  $effect(() => {
    void $findTargetsChanged;
    void activeTab;
    canFind = targetIn(contentEl?.querySelector(".layer.active") ?? null) !== null;
  });
  /** Edge/center drop-zone preview for THIS pane, if a drag hovers it. */
  const zone = $derived(
    dropSpot?.kind === "zone" && dropSpot.paneId === node.id ? dropSpot.zone : null,
  );
  /** Context bridge: the "@ reference" band hovers over this pane's bottom. */
  const refBand = $derived(dropSpot?.kind === "ref" && dropSpot.paneId === node.id);
  /** The "link to agent" band preview (dragging a terminal over this agent). */
  const linkBand = $derived(dropSpot?.kind === "link" && dropSpot.paneId === node.id);
  /** A link-intent drag (from the link icon) hovering anywhere in this agent
   *  pane: the whole view lights up as one target. */
  const linkPane = $derived(dropSpot?.kind === "linkpane" && dropSpot.paneId === node.id);
  /** An OS-desktop file drag hovering this live-session pane: the whole pane
   *  is the upload-and-reference target (HTML5 dnd — no tile gesture to
   *  partition against). */
  const uploadPane = $derived(dropSpot?.kind === "upload" && dropSpot.paneId === node.id);
  /** An upload or in-app transfer hovering a Finder pane: land INTO the folder
   *  under the pointer. The pane only frames itself — the Finder lights the
   *  exact column or dir row `dir` names (see FinderView's dropDir). */
  const folderDrop = $derived(
    (dropSpot?.kind === "uploadDir" || (dropSpot?.kind === "fileOp" && dropSpot.blocked === null)) &&
    dropSpot.paneId === node.id ? dropSpot : null,
  );
  const uploadDir = $derived(folderDrop?.dir ?? null);
  const uploadRow = $derived(
    folderDrop?.row ?? false,
  );
  /** The live session this pane shows, named as its tab is — so a drop band
   *  says WHICH session it targets ("@ reference in claude-1"), not just
   *  "this session"; two session panes side by side must read differently. */
  const activeSessionName = $derived(
    activeTab !== null && activeTab.surface === "terminal"
      ? sessionLabel(names, sessions, activeTab.sessionId)
      : null,
  );
  /** This pane's bottom band is reserved for the current drag: the center
   *  (adopt) preview stops above it instead of flashing the full pane. */
  const bandArmed = $derived(bandPanes.has(node.id));
  /** When this pane IS an agent session: its own hue (link band tint). */
  const ownAgentHue = $derived.by(() => {
    if (activeTab === null || activeTab.surface !== "terminal") return null;
    const s = sessions.get(activeTab.sessionId);
    return s !== undefined && s.kind === "agent" ? agentHue(activeTab.sessionId) : null;
  });

  /** When this pane shows a linked terminal: the leash-holder's hue, and
   *  whether that agent is executing here right now (border pulse). */
  const linkedAgentId = $derived(
    activeTab !== null && activeTab.surface === "terminal"
      ? (links.get(activeTab.sessionId) ?? null)
      : null,
  );
  const linkHue = $derived(linkedAgentId !== null ? agentHue(linkedAgentId) : null);
  const agentExec = $derived(
    linkedAgentId !== null &&
      activeTab !== null &&
      activeTab.surface === "terminal" &&
      sessions.get(activeTab.sessionId)?.exec_stage === "executing",
  );

  // --- view keep-alive (the "keep tabs in RAM" model) -----------------------
  //
  // A pane retains recently-used view trees whose DOM is itself valuable, so
  // switching files/Finders/diffs and long chats preserves scroll, decode,
  // editor state, and rendered transcript DOM. PTYs deliberately remount:
  // termPool can re-parent the xterm element into its hidden stash, avoiding
  // duplicate invisible WebGL renderers while preserving scrollback/socket
  // state. The live set is a per-pane MRU capped at LIVE_CAP; views enter only
  // after being active, so nothing is first measured at a degenerate size.
  const LIVE_CAP = 8;
  let liveKeys = $state<string[]>([]);

  /** Trim the MRU to LIVE_CAP from its cold end, never evicting a file with
   *  unsaved edits: its buffer would survive in the store anyway, but its
   *  view (scroll, search panel, selection chip) is what the user left. */
  function trimLive(keys: string[], dirty: ReadonlySet<string>): string[] {
    if (keys.length <= LIVE_CAP) return keys;
    const out = [...keys];
    for (let i = out.length - 1; i > 0 && out.length > LIVE_CAP; i--) {
      const k = out[i];
      if (!(k.startsWith("f:") && dirty.has(k.slice(2)))) out.splice(i, 1);
    }
    return out;
  }

  function retainView(tab: Tab): boolean {
    return keepsPaneViewAlive(
      tab,
      tab.surface === "terminal" ? sessions.get(tab.sessionId)?.ui : undefined,
    );
  }

  $effect(() => {
    const active = activeTab;
    if (active === null || !retainView(active)) return;
    const key = tabKey(active);
    untrack(() => {
      const i = liveKeys.indexOf(key);
      if (i !== 0) {
        liveKeys =
          i < 0 ? [key, ...liveKeys] : [key, ...liveKeys.slice(0, i), ...liveKeys.slice(i + 1)];
      }
      if (liveKeys.length > LIVE_CAP) liveKeys = trimLive(liveKeys, get(dirtyFiles));
    });
  });

  /** A 1×1 transparent GIF: the selection stops (see `.sel-stop`). */
  const STOP_SRC = "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

  // Parked long enough, a layer goes dormant: visibility:hidden, which drops
  // the backing stores WebKit keeps for a merely transparent scroller. It is
  // an inherited toggle (a restyle + relayout of that whole subtree), so it
  // runs off the switch path, from idle time, and only after a pause — a
  // quick switch-back stays a two-element restyle.
  const DORMANT_AFTER_MS = 30_000;
  let dormant = $state<Set<string>>(new Set());
  const dormantTimers = new Map<string, number>();
  function scheduleDormant(key: string): void {
    if (dormantTimers.has(key)) return;
    dormantTimers.set(
      key,
      window.setTimeout(() => {
        dormantTimers.delete(key);
        if (parkedKey === key || !node.tabs.some((t) => tabKey(t) === key)) return;
        dormant = new Set(dormant).add(key);
      }, DORMANT_AFTER_MS),
    );
  }
  function wake(key: string): void {
    const timer = dormantTimers.get(key);
    if (timer !== undefined) {
      window.clearTimeout(timer);
      dormantTimers.delete(key);
    }
    if (dormant.has(key)) {
      const next = new Set(dormant);
      next.delete(key);
      dormant = next;
    }
  }
  $effect(() => () => {
    for (const timer of dormantTimers.values()) window.clearTimeout(timer);
  });

  // A parked layer must not keep the DOM selection or focus: a caret left in
  // an inert (unselectable) subtree makes WebKit re-canonicalize it through
  // the whole parked document on every rendering commit. Runs before the DOM
  // flips, so `.layer.active` is still the departing layer; the incoming
  // layer wakes in the same batch as its `active` flip.
  let parkedKey: string | null = null;
  $effect.pre(() => {
    const key = activeTab === null ? null : tabKey(activeTab);
    const previous = parkedKey;
    parkedKey = key;
    untrack(() => {
      if (key !== null) wake(key);
      if (previous === null || previous === key) return;
      scheduleDormant(previous);
      const departing = contentEl?.querySelector(".layer.active");
      if (!departing) return;
      const focused = document.activeElement;
      if (focused instanceof HTMLElement && departing.contains(focused)) focused.blur();
      const sel = document.getSelection();
      if (sel !== null && sel.anchorNode !== null && departing.contains(sel.anchorNode)) {
        sel.removeAllRanges();
      }
    });
  });

  // Drop live keys whose tab has closed (or moved to another pane), so the cap
  // counts only retained views this pane still holds.
  $effect(() => {
    const present = new Set(node.tabs.filter(retainView).map(tabKey));
    untrack(() => {
      if (liveKeys.some((k) => !present.has(k))) {
        liveKeys = liveKeys.filter((k) => present.has(k));
      }
    });
  });

  // The tabs whose views are mounted right now: the active tab (always) plus
  // retained DOM-backed views, in tab-bar order.
  const mountedTabs = $derived(
    node.tabs.filter((t) => t === activeTab || (retainView(t) && liveKeys.includes(tabKey(t)))),
  );

  // Every workbench surface is a feature boundary. Loading only the kinds in
  // this pane's bounded live set keeps home/workspace startup lean; once a
  // view arrives the existing keep-alive model preserves it across tab swaps.
  let views = $state<Partial<Record<PaneViewKind, Component<any>>>>({});
  let viewErrors = $state<Partial<Record<PaneViewKind, unknown>>>({});

  function requestView(kind: PaneViewKind, retryError?: unknown) {
    const isRetry = retryError !== undefined;
    const request = retryError === undefined ? loadPaneView(kind) : retryPaneView(kind, retryError);
    void request.then(
      (view) => {
        views = { ...views, [kind]: view };
        // The browser may re-emit the original preload failure while the
        // recovery path fresh-loads its asset. Settle the shared notice only
        // after the affected pane proves it rendered successfully.
        if (isRetry) clearChunkFailure();
      },
      (error: unknown) => {
        // Lazy chunks are immutable per daemon build. A reconnect/update can
        // atomically replace that set underneath an already-open window; the
        // old import URL can never succeed on Retry. Keep the useful detail in
        // the console. The shared transition notice covers nested chunks too;
        // this pane still keeps an in-place retry for ordinary network loss.
        console.error(`could not load ${kind} view`, error);
        noteChunkFailure();
        viewErrors = { ...viewErrors, [kind]: error ?? new Error(`could not load ${kind} view`) };
      },
    );
  }

  function retryView(kind: PaneViewKind) {
    const error = viewErrors[kind];
    if (error === undefined) return;
    const next = { ...viewErrors };
    delete next[kind];
    viewErrors = next;
    clearChunkFailure();
    requestView(kind, error);
  }

  function reloadWindow() {
    requestAssetReload();
  }

  /** Never throws: it renders inside the error card itself. */
  function errorText(error: unknown): string {
    let text: string;
    try {
      const message =
        typeof error === "object" && error !== null && "message" in error
          ? (error as { message: unknown }).message
          : error;
      text = typeof message === "string" ? message : (JSON.stringify(message) ?? String(message));
    } catch {
      text = "unknown error";
    }
    return text.length > 300 ? `${text.slice(0, 300)}…` : text;
  }

  function viewKind(tab: Tab): PaneViewKind | null {
    // A subagent's conversation renders in the chat view.
    if (tab.surface === "subagent") return "chat";
    if (tab.surface !== "terminal") return tab.surface;
    const session = sessions.get(tab.sessionId);
    if (session === undefined) return null;
    return session.ui === "chat" ? "chat" : "terminal";
  }

  $effect(() => {
    const needed = new Set(mountedTabs.map(viewKind).filter((kind) => kind !== null));
    for (const kind of needed) {
      if (views[kind] !== undefined || viewErrors[kind] !== undefined) continue;
      requestView(kind);
    }
  });

  let rootEl = $state<HTMLElement | null>(null);
  let contentEl = $state<HTMLDivElement | null>(null);
  let tabbarEl = $state<HTMLElement | null>(null);

  // Register this pane's geometry with the dnd hit-tester.
  $effect(() => {
    const root = rootEl;
    const content = contentEl;
    if (root === null || content === null) return;
    registerPane(node.id, { root, content, tabbar: tabbarEl });
    return () => unregisterPane(node.id, root);
  });
</script>

{#snippet loadFailure(kind: PaneViewKind, label: string)}
  <div class="hint load-failure">
    <span>could not load {label}</span>
    <button type="button" onclick={() => retryView(kind)}>retry</button>
    <button type="button" onclick={reloadWindow}>reload window</button>
  </div>
{/snippet}

<!-- A view that throws while rendering (a bug, or data it didn't expect —
     plugin output is untrusted) fails alone: without a boundary the error
     escapes Svelte's update and every view in the window stops updating. -->
{#snippet viewCrash(error: unknown, reset: () => void)}
  <div class="hint load-failure crash">
    <span>this view hit an error</span>
    <span class="crash-msg" title={errorText(error)}>{errorText(error)}</span>
    <button type="button" onclick={reset}>try again</button>
    <button type="button" onclick={reloadWindow}>reload window</button>
  </div>
{/snippet}

{#snippet surface(tab: Tab, active: boolean)}
  {#if tab.surface === "terminal"}
    <!-- The surface follows server truth: which process runs behind the session
         id (a chat driver or a PTY). Same tab, same identity — the view toggle
         just flips this field on the bus. -->
    {@const s = sessions.get(tab.sessionId)}
    {#if s === undefined}
      <!-- The session is gone (mid-teardown, before pruneSessions drops the
           tab): render nothing, never a fresh TerminalView against a dead id. -->
      <div class="hint"><span>closing…</span></div>
    {:else if s.suspended && s.ui !== "chat"}
      <!-- A paused terminal: its project runs elsewhere, or it waits for an
           update, a sign-in on the cloud machine or its transfer (the row's
           additive `pause` says which). A paused chat stays mounted below:
           its transcript and scroll survive and it follows the conversation. -->
      {@const pause = sessionPause(s)}
      {@const paused = s.kind === "shell"
        ? shellPauseLabel(pause, { places: proPossible() })
        : pauseLabel(pause, { signedOut: $accountSignedOut })}
      <!-- Waiting for an agent sign-in on the cloud (the row's additive
           `blocked_provider`): offer the one thing that unblocks it. -->
      {@const connect = canOpenOnboarding() ? pausedConnect(s) : null}
      <div class="hint paused"><span role="status">{paused.status}</span>{#if connect !== null}<button type="button" onclick={() => cloudOnboarding.request({ providerIds: [connect.providerId], workspaceId: connect.workspaceId })}>Connect {connect.label} to continue</button>{:else if paused.detail !== null}<small>{paused.detail}</small>{/if}</div>
    {:else if s.ui === "chat"}
      {@const ChatView = views.chat}
      {#if ChatView !== undefined}
        <ChatView
          session={s}
          focused={focused && active}
          visible={active}
          terminals={[...sessions.values()]
            .filter((t) => t.kind === "shell" && t.alive && t.workspace_id === s.workspace_id)
            .map((t) => ({ id: t.id, name: names.get(t.id) ?? t.name }))}
          onOpenFile={(p: string) => ctrl.openFileFrom(node.id, p, false)}
          onOpenPath={(p: string, k: "file" | "dir") => ctrl.openPathFrom(node.id, p, k, false)}
          onSwitchToTerminal={s.view_switchable === false ? undefined : () => ctrl.switchView(s.id, "term")}
          onForked={(forked: Session) =>
            ctrl.revealWorktreeSession(forked.id, forked.workspace_id)}
          onOpenSubagent={(agentId: string, title: string, newSplit: boolean) =>
            ctrl.openSubagentFrom(node.id, s.id, agentId, title, newSplit)}
        />
      {:else if viewErrors.chat}
        {@render loadFailure("chat", "chat view")}
      {:else}
        <Spinner />
      {/if}
    {:else}
      {@const TerminalView = views.terminal}
      {#if TerminalView !== undefined}
        <TerminalView
          sessionId={tab.sessionId}
          focused={focused && active}
          fontSize={node.fontSize}
          placement={placementLabel(s.placement, s.placement_available, { owner: terminalStatus(s.id), reachable: terminalKept(s.id) })}
          reach={`${typeof s.placement === "object" ? s.placement.remote : "here"}|${s.placement_available !== false}`}
        />
      {:else if viewErrors.terminal}
        {@render loadFailure("terminal", "terminal view")}
      {:else}
        <Spinner />
      {/if}
    {/if}
  {:else if tab.surface === "file"}
    {@const FileView = views.file}
    {#if FileView !== undefined}
      <FileView path={tab.path} {wsRoot} fontSize={node.fontSize} />
    {:else if viewErrors.file}
      {@render loadFailure("file", "file view")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "finder"}
    {@const FinderView = views.finder}
    {#if FinderView !== undefined}
      <FinderView
        path={tab.path}
        {wsRoot}
        dropDir={active ? uploadDir : null}
        dropOnRow={active && uploadRow}
        dropAction={folderDrop?.kind === "fileOp" ? folderDrop.operation : "upload"}
        onDragStart={ctrl.dragFileEntry}
        onOpenFile={(p: string, split: boolean) => ctrl.openFileFrom(node.id, p, split)}
        onNavigate={(p: string) => ctrl.navigateFinder(tab.id, p)}
      />
    {:else if viewErrors.finder}
      {@render loadFailure("finder", "Finder")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "diff"}
    {@const DiffView = views.diff}
    {#if DiffView !== undefined}
      <DiffView path={tab.path} mode={tab.mode} rev={tab.rev} repo={tab.repo} orig={tab.orig} {wsId} />
    {:else if viewErrors.diff}
      {@render loadFailure("diff", "diff view")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "gitx"}
    {@const GitDetailView = views.gitx}
    {#if GitDetailView !== undefined}
      <GitDetailView {wsId} paneId={node.id} {ctrl} {tab} />
    {:else if viewErrors.gitx}
      {@render loadFailure("gitx", "history view")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "git"}
    {@const GitView = views.git}
    {#if GitView !== undefined}
      <GitView {wsId} paneId={node.id} {ctrl} {sessions} {names} onOpenSession={ctrl.revealWorktreeSession} />
    {:else if viewErrors.git}
      {@render loadFailure("git", "git view")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "subagent"}
    <!-- One of a chat's subagents, as a read-only chat of its own. `session`
         stays the PARENT: the view reads the subagent through it. -->
    {@const parent = sessions.get(tab.sessionId)}
    {#if parent === undefined}
      <div class="hint"><span>session closed</span></div>
    {:else}
      {@const ChatView = views.chat}
      {#if ChatView !== undefined}
        <ChatView
          session={parent}
          subagent={{
            agentId: tab.agentId,
            title: tab.title,
            parentName: names.get(parent.id) ?? parent.name,
            onOpenParent: () => ctrl.revealWorktreeSession(parent.id, parent.workspace_id),
          }}
          focused={focused && active}
          visible={active}
          onOpenFile={(p: string) => ctrl.openFileFrom(node.id, p, false)}
          onOpenPath={(p: string, k: "file" | "dir") => ctrl.openPathFrom(node.id, p, k, false)}
        />
      {:else if viewErrors.chat}
        {@render loadFailure("chat", "chat view")}
      {:else}
        <Spinner />
      {/if}
    {/if}
  {:else if tab.surface === "changes"}
    {@const cs = sessions.get(tab.sessionId)}
    {#if cs !== undefined}
      {@const SessionChangesView = views.changes}
      {#if SessionChangesView !== undefined}
        <SessionChangesView session={cs} {wsRoot} paneId={node.id} {ctrl} />
      {:else if viewErrors.changes}
        {@render loadFailure("changes", "changes view")}
      {:else}
        <Spinner />
      {/if}
    {:else}
      <div class="hint"><span>session closed</span></div>
    {/if}
  {:else if tab.surface === "dashboard"}
    {@const DashboardView = views.dashboard}
    {#if DashboardView !== undefined}
      <DashboardView
        {dash}
        {sessions}
        {names}
        {wsId}
        {wsRoot}
        paneId={node.id}
        {ctrl}
        visible={active}
      />
    {:else if viewErrors.dashboard}
      {@render loadFailure("dashboard", "dashboard")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "timeline"}
    {@const TimelineView = views.timeline}
    {#if TimelineView !== undefined}
      <TimelineView {dash} {sessions} {names} {wsId} {wsRoot} paneId={node.id} {ctrl} visible={active} />
    {:else if viewErrors.timeline}
      {@render loadFailure("timeline", "timeline")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "knowledge"}
    {@const KnowledgeView = views.knowledge}
    {#if KnowledgeView !== undefined}
      <KnowledgeView {wsId} {wsRoot} paneId={node.id} {ctrl} visible={active} />
    {:else if viewErrors.knowledge}
      {@render loadFailure("knowledge", "knowledge")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "plugins"}
    {@const PluginsView = views.plugins}
    {#if PluginsView !== undefined}
      <PluginsView {dash} {wsId} {wsRoot} paneId={node.id} {ctrl} visible={active} />
    {:else if viewErrors.plugins}
      {@render loadFailure("plugins", "extensions")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "plugin"}
    {@const PluginTab = views.plugin}
    {#if PluginTab !== undefined}
      <PluginTab {wsId} {wsRoot} plugin={tab.plugin} view={tab.view} />
    {:else if viewErrors.plugin}
      {@render loadFailure("plugin", "plugin view")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "sessions"}
    {@const SessionsView = views.sessions}
    {#if SessionsView !== undefined}
      <SessionsView {dash} {sessions} {names} {wsId} {wsRoot} paneId={node.id} {ctrl} visible={active} />
    {:else if viewErrors.sessions}
      {@render loadFailure("sessions", "all sessions")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "kept"}
    {@const KeptApplicationView = views.kept}
    {#if KeptApplicationView !== undefined}
      <KeptApplicationView {wsId} {wsRoot} visible={active} {tab} paneId={node.id}
        currentTab={(original: object, pane: string) => node.id === pane && node.tabs.some((candidate) => candidate === original)}
        callbacks={{
          openFile: (pane: string, path: string) => ctrl.openFileFrom(pane, path, false),
          openFolder: (pane: string, path: string) => ctrl.openPathFrom(pane, path, "dir", false),
          close: (original: object, pane: string) => {
            if (node.id !== pane) return;
            const index = node.tabs.findIndex((candidate) => candidate === original);
            if (index >= 0) ctrl.closeTab(pane, index);
          },
          modal: (element: HTMLElement) => {
            const handle = modalFocus(element);
            return { destroy: () => handle?.destroy?.() };
          },
        }} />
    {:else if viewErrors.kept}
      {@render loadFailure("kept", "the review of both versions")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "browser"}
    {@const BrowserView = views.browser}
    {#if BrowserView !== undefined}
      <BrowserView
        tabId={tab.id}
        host={tab.host}
        port={tab.port}
        path={tab.path}
        visible={active}
        onNavigate={(p: string) => ctrl.navigateBrowser(tab.id, p)}
        onRetarget={(h: string, p: number, pth: string, found: boolean) =>
          ctrl.retargetBrowser(tab.id, h, p, pth, found)}
        onFocusRequest={() => ctrl.focusPane(node.id)}
        opener={tab.openedBy !== undefined ? browserOpener(tab.openedBy, sessions, names) : null}
        onRevealOpener={() => {
          if (tab.openedBy !== undefined) linkCtrl.reveal(tab.openedBy, node.id);
        }}
      />
    {:else if viewErrors.browser}
      {@render loadFailure("browser", "browser pane")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "pro"}
    {@const ProView = views.pro}
    {#if ProView !== undefined}
      <ProView visible={active} workspaceId={wsId} incarnation={tab}
        current={() => node.tabs.some(candidate => candidate === tab)}
        onClose={() => { const index = node.tabs.findIndex(candidate => candidate === tab); if (index >= 0) ctrl.closeTab(node.id, index); }} />
    {:else if viewErrors.pro}
      {@render loadFailure("pro", "Chimaera Pro")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "settings"}
    {@const SettingsView = views.settings}
    {#if SettingsView !== undefined}
      <SettingsView visible={active} />
    {:else if viewErrors.settings}
      {@render loadFailure("settings", "settings")}
    {:else}
      <Spinner />
    {/if}
  {/if}
{/snippet}

<!-- The shell exists solely so the exec ring below can live OUTSIDE .pane:
     the pane clips its descendants (overflow: hidden), so a ring inside it
     would have to overlap the outermost content pixels. Layout-transparent —
     it takes over the pane's flex-child contract and the pane fills it. -->
<div class="pane-shell">
<section
  class="pane"
  data-pane-id={node.id}
  class:focused
  class:linked={linkHue !== null}
  class:agent-exec={agentExec}
  aria-label={`Pane ${paneNumber}`}
  style:--hue={linkHue}
  tabindex="-1"
  bind:this={rootEl}
  onpointerdowncapture={() => ctrl.focusPane(node.id)}
>
  <div class="pane-guide" class:shown={hintsActive() || paneHintsActive()} aria-hidden="true" inert>
    <div class="pane-guide-center">
      <span class="pane-guide-label">{focused ? "Current pane" : "Pane"}</span>
      <span class="pane-guide-number">{paneNumber}</span>
      {#if paneMoveHint(paneNumber)}
        <span class="pane-guide-move">{paneHintsActive() ? "Move tab here" : "Focus pane"} <kbd>{paneHintsActive() ? paneMoveHint(paneNumber) : paneFocusHint(paneNumber)}</kbd></span>
      {/if}
    </div>
    {#if focused}
      <div class="pane-guide-actions">
        {#if keyHint("newPane") && ctrl.paneTargets().length < MAX_PANES}
          <span>New empty pane <kbd>{keyHint("newPane")}</kbd></span>
        {/if}
        {#if node.tabs.length > 1 && keyHint("cycleNext")}
          <span>Next tab <kbd>{keyHint("cycleNext")}</kbd></span>
        {/if}
      </div>
    {/if}
  </div>
  <!-- Every pane always has its top bar — orientation, drag handle, and the
       mouse home for zoom/split/close, even single-pane single-tab. -->
  <PaneTabs
    {node}
    {zoomed}
        {sessions}
    {names}
    {fileNames}
    {links}
    {linkCtrl}
    {dropSpot}
    {ctrl}
    bind:el={tabbarEl}
    onFind={canFind ? () => {
      ctrl.focusPane(node.id);
      targetIn(contentEl?.querySelector(".layer.active") ?? null)?.("open");
    } : undefined}
  />
  <div class="content" bind:this={contentEl}>
    <!-- Retained file/workbench/chat views stay mounted with the active one
         visible. PTY components remount against termPool, so inactive xterm
         elements park without making long chat transcripts reconstruct. -->
    <!-- One layer per TAB, not per mounted view: a switch must never insert
         a sibling here (WebKit invalidates the following siblings' whole
         subtrees for positional selectors). The stops fence the caret walk
         (see .sel-stop). -->
    {#each node.tabs as tab (tabKey(tab))}
      {@const active = tab === activeTab}
      <div class="layer" class:active class:dormant={dormant.has(tabKey(tab))} inert={!active}>
        <img class="sel-stop" src={STOP_SRC} alt="" aria-hidden="true" />
        {#if mountedTabs.includes(tab)}
          <svelte:boundary onerror={(e) => console.error("pane view failed", e)}>
            {@render surface(tab, active)}
            {#snippet failed(error, reset)}
              {@render viewCrash(error, reset)}
            {/snippet}
          </svelte:boundary>
        {/if}
        <img class="sel-stop" src={STOP_SRC} alt="" aria-hidden="true" />
      </div>
    {/each}
    {#if activeTab === null}
      <div class="empty-pane">
        <span class="empty-title">Space for your next view</span>
        <span class="empty-detail">Drag any tab here, or hold <kbd>{activeModLabel()}</kbd> for shortcuts.</span>
        <button class="empty-open" onclick={() => ctrl.quickOpen(node.id)}>Open a file or session {#if keyHint("quickOpen")}<kbd>{keyHint("quickOpen")}</kbd>{/if}</button>
        <div class="empty-foot">
          <span>{#if keyHint("newAgent")}<kbd>{keyHint("newAgent")}</kbd>{/if} new agent</span>
          <span aria-hidden="true">·</span>
          <span>{#if keyHint("newTerminal")}<kbd>{keyHint("newTerminal")}</kbd>{/if} terminal</span>
        </div>
      </div>
    {/if}
  </div>

  <div class="document-nav" class:shown={historyVisible} role="group" aria-label="Document history" aria-hidden={!historyVisible} inert={!historyVisible}>
    <button aria-label="Previous document" title={`Previous document${keyHintSuffix("fileBack")}`} disabled={fileTrail === undefined || fileTrail.index === 0} onclick={() => ctrl.navigateFileHistory(node.id, -1)}>
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="m9.5 4-4 4 4 4" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" /></svg>
    </button>
    <span class="document-nav-divider" aria-hidden="true"></span>
    <button aria-label="Next document" title={`Next document${keyHintSuffix("fileForward")}`} disabled={fileTrail === undefined || fileTrail.index === fileTrail.paths.length - 1} onclick={() => ctrl.navigateFileHistory(node.id, 1)}>
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true"><path d="m6.5 4 4 4-4 4" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" /></svg>
    </button>
  </div>

  {#if zone !== null}
    <!-- A zone preview always says what it is: the rectangle alone can't tell
         "split right" from "add as tab" at a glance. -->
    <div class="drop drop-{zone}" class:banded={bandArmed}>
      <span class="drop-chip">{zoneWord(zone)}</span>
    </div>
  {:else if linkBand}
    <!-- Distinct from the split/adopt zones: a labeled, dashed band over the
         agent's input area. Dropping links the terminal and types its
         @term: reference into the composer (never submits). -->
    <div class="drop-link" class:hued={ownAgentHue !== null} style:--band-hue={ownAgentHue}>
      <span class="drop-chip">link to {activeSessionName ?? "this agent"}</span>
    </div>
  {:else if linkPane}
    <!-- Link-intent drag: the whole agent view is one target (no aiming for a
         band). Full-pane wash in the agent's hue, centered label. -->
    <div class="drop-linkpane" class:hued={ownAgentHue !== null} style:--band-hue={ownAgentHue}>
      <span class="drop-chip">link to {activeSessionName ?? "this agent"}</span>
    </div>
  {/if}

  {#if refBand}
    <!-- Drag-to-reference: types the path into this session's input, never
         opens a tab, never submits. Visibly distinct from the adopt zone. -->
    <div class="drop-ref">
      <span class="drop-chip"
        ><span class="drop-ref-at">@</span> reference in {activeSessionName ?? "this session"}</span
      >
    </div>
  {/if}

  {#if uploadPane}
    <!-- OS-desktop drop: uploads to the session's host, then types the
         path — same "@ reference" grammar, whole pane as the target. -->
    <div class="drop-upload">
      <span class="drop-chip"
        ><span class="drop-ref-at">@</span> upload &amp; reference in {activeSessionName ??
          "this session"}</span
      >
    </div>
  {:else if uploadDir !== null}
    <!-- OS-desktop drop onto a Finder pane: a quiet frame says this pane is
         receiving; the Finder's own column/row highlight says WHERE (a
         whole-pane wash hid exactly that). -->
    <div class="drop-frame">
      <!-- The destination, named once at the pane's foot (never inside the
           column: an in-flow chip would grow the column mid-drag). -->
      <span class="drop-chip folder">upload into <b>{dirLabel(uploadDir)}</b></span>
    </div>
  {/if}
</section>
{#if agentExec}
  <!-- The agent is executing here: the border breathes in the agent's hue —
       peripheral-vision signal that the leash is being pulled. A STATIC ring
       breathed via opacity (composited) instead of box-shadow keyframes (a
       repaint per frame for the whole turn); centered on the pane's border
       line so it occludes no content or scrollbars. -->
  <div class="exec-ring" style:--hue={linkHue} aria-hidden="true"></div>
{/if}
</div>

<style>
  /* Keep the floating control inside the document area, clear of editor
     status and sheet bars. The same inset works across preview types. */
  .document-nav { position: absolute; bottom: calc(var(--pane-toolbar-height) + 12px); left: 12px; z-index: 3; display: flex; align-items: center; padding: 2px; border: 1px solid var(--edge); border-radius: 7px; background: color-mix(in srgb, var(--bg) 94%, transparent); box-shadow: 0 2px 6px color-mix(in srgb, var(--fg) 4%, transparent); opacity: 0; transition: opacity 140ms ease; }
  .content:hover ~ .document-nav.shown { opacity: 0.7; }
  .document-nav.shown:hover, .document-nav.shown:focus-within { opacity: 1; }
  .document-nav button { width: 22px; height: 22px; display: grid; place-items: center; padding: 0; border: 0; border-radius: 4px; background: transparent; color: var(--muted); cursor: pointer; }
  .document-nav button:hover:not(:disabled) { background: var(--row-hover); color: var(--fg); }
  .document-nav button:focus-visible { outline: 1px solid var(--accent); outline-offset: -1px; }
  .document-nav button:disabled { opacity: 0.3; cursor: default; }
  .document-nav-divider { width: 1px; height: 12px; margin: 0 2px; background: var(--edge); }
  .pane-guide {
    position: absolute;
    inset: 0;
    z-index: 8;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 24px;
    border-radius: inherit;
    color: var(--muted);
    background: color-mix(in srgb, var(--accent) 3%, transparent);
    font-size: 11px;
    pointer-events: none;
    opacity: 0;
    transition: opacity 160ms ease;
  }
  .pane-guide.shown { opacity: 1; }
  .pane-guide-center { display: flex; flex-direction: column; align-items: center; gap: 18px; text-align: center; }
  .pane-guide-label { font-size: 10px; letter-spacing: 0.14em; text-transform: uppercase; color: color-mix(in srgb, var(--muted) 75%, transparent); }
  .pane-guide-number { font-size: clamp(80px, 12vw, 160px); font-weight: 200; line-height: 1; letter-spacing: -0.06em; color: color-mix(in srgb, var(--fg) 12%, transparent); }
  .pane-guide-move { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; justify-content: center; }
  .pane-guide kbd { font: 11px var(--mono); color: var(--text); border: 1px solid var(--edge); border-radius: 5px; padding: 3px 6px; white-space: nowrap; }
  .pane-guide-actions { position: absolute; bottom: 28px; left: 24px; right: 24px; display: flex; flex-wrap: wrap; align-items: center; justify-content: center; gap: 16px 24px; }
  .pane-guide-actions span { display: flex; align-items: center; gap: 10px; }
  @media (prefers-reduced-motion: reduce) { .pane-guide, .document-nav { transition: none; } }
  .empty-pane {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 24px;
    text-align: center;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .empty-title { color: var(--text); font-weight: 500; }
  .empty-detail { font-size: var(--text-xs); line-height: 1.7; max-width: 270px; }
  .empty-pane kbd {
    font: 10px var(--mono);
    white-space: nowrap;
    border: 1px solid var(--edge);
    border-radius: 4px;
    padding: 2px 4px;
  }
  .empty-open {
    margin: 3px 0;
    padding: 7px 10px;
    display: flex;
    max-width: 100%;
    flex-wrap: wrap;
    justify-content: center;
    gap: 12px;
    align-items: center;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .empty-open:hover { background: color-mix(in srgb, var(--accent) 7%, transparent); border-color: var(--accent); }
  .empty-foot { display: flex; gap: 9px; flex-wrap: wrap; justify-content: center; font-size: 10px; line-height: 1.9; margin-top: 8px; }
  .pane-shell {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    position: relative;
  }

  .pane {
    flex: 1;
    min-width: 0;
    min-height: 0;
    position: relative;
    display: flex;
    flex-direction: column;
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
    overflow: hidden;
    transition: border-color 0.12s ease;
    outline: none;
  }

  /* The focused pane is unmistakable: hairline accent instead of the edge. */
  .pane.focused {
    border-color: color-mix(in srgb, var(--accent) 62%, var(--edge));
  }

  /* A linked terminal carries its agent's hue as a quiet border tint
     (focus still wins — the accent hairline stays unambiguous). */
  .pane.linked:not(.focused) {
    border-color: color-mix(in srgb, hsl(var(--hue) 50% 55%) 38%, var(--edge));
  }

  /* The exec ring (see the template comment): 2px wide, centered on the
     pane's 1px border — [-1px, +1px] around the border box — so the inner
     half paints over chrome (the border line), never content, and the outer
     half breathes into the surrounding gap where the old box-shadow lived
     (an ancestor that clips at the pane's bounds — a split .cell — clipped
     the old shadow's outer ring identically). */
  .exec-ring {
    position: absolute;
    inset: -1px;
    pointer-events: none;
    border: 2px solid hsl(var(--hue) 60% 55% / 0.35);
    border-radius: 11px; /* the pane's 10px, 1px further out */
    opacity: 0;
    animation: agent-exec-pulse 1.4s ease-in-out infinite;
  }

  @keyframes agent-exec-pulse {
    50% {
      opacity: 1;
    }
  }

  /* Hidden document: nobody sees the breath — stop burning frames (the
     html.app-hidden contract; see app.css). */
  :global(html.app-hidden) .exec-ring {
    animation-play-state: paused;
  }

  @media (prefers-reduced-motion: reduce) {
    .exec-ring {
      animation: none;
      opacity: 1;
      border-color: hsl(var(--hue) 60% 55% / 0.3);
    }
  }

  .content {
    flex: 1;
    position: relative;
    min-height: 0;
    min-width: 0;
  }

  /* Keep-alive layers: one per tab, only the active one visible. Parked
     layers hide with opacity — never display:none (unmeasures
     xterm/CodeMirror) and, on the switch path, never visibility:hidden: it
     inherits, so WebKit re-resolves and re-shapes the whole parked subtree.
     inert (also inherited — its toggle restyles the two switched layers, and
     only those) keeps parked layers out of focus, hit-testing, and the AX
     tree. Nothing else toggles here: pointer-events and user-select inherit
     too, and a z-index would trap the views' position:fixed overlays inside
     the layer. Layout, scroll, and decode stay intact. */
  .layer {
    position: absolute;
    inset: 0;
  }

  .layer:not(.active) {
    opacity: 0;
  }

  /* Dormant (parked for a while, applied from idle time): the inherited
     toggle is paid off the switch path, and the reveal pays it once. */
  .layer.dormant {
    visibility: hidden;
  }

  /* Selection stops. macOS WebKit re-derives its editor state on every
     rendering commit while a caret sits in an editable, walking the DOM from
     the caret to the nearest visible selectable position; terminals are
     user-select:none and parked layers inert, so that walk crossed every
     parked node. A selectable 1px image at each end of a layer is the nearest
     such position (an empty block is not one; text would be copied). Its
     empty alt keeps it out of copied text. */
  .sel-stop {
    position: absolute;
    top: 0;
    left: 0;
    width: 1px;
    height: 1px;
    pointer-events: none;
    user-select: text;
    -webkit-user-select: text;
  }

  .hint {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.45rem;
    color: var(--muted);
    font-size: var(--text-sm);
    user-select: none;
  }

  .hint.paused {
    flex-direction: column;
    padding: 0 16px;
    text-align: center;
  }
  .hint.paused small {
    font-size: var(--text-xs);
  }
  .hint.paused button {
    margin-top: 0.35rem;
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 0.35rem 0.75rem;
    color: var(--fg);
    background: var(--bg);
    font: inherit;
    cursor: pointer;
  }
  .hint.paused button:hover {
    background: var(--row-hover);
  }
  .hint.paused button:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 2px;
  }
  @media (pointer: coarse) {
    .hint.paused button {
      min-height: 40px;
    }
  }



  .hint-sep {
    opacity: 0.5;
  }

  .load-failure button {
    border: 1px solid var(--edge);
    border-radius: 5px;
    padding: 0.2rem 0.55rem;
    color: var(--text);
    background: var(--bg);
    font: inherit;
    cursor: pointer;
  }

  .load-failure button:hover {
    border-color: color-mix(in srgb, var(--accent) 60%, var(--edge));
  }

  .crash {
    flex-wrap: wrap;
    align-content: center;
    row-gap: 0.6rem;
    padding: 0 24px;
  }

  .crash-msg {
    flex-basis: 100%;
    order: 1;
    text-align: center;
    font-family: var(--mono);
    font-size: var(--text-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Translucent drop-zone preview showing exactly where the drop lands, with
     its action named in the middle (the same band-label chip the ref/link
     bands use — one grammar for every preview). */
  .drop {
    position: absolute;
    z-index: 6;
    margin: 3px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 42%, transparent);
    border-radius: 7px;
    pointer-events: none;
  }

  .drop-center {
    inset: 0;
  }

  /* Drag-to-reference band over the input area (~22%, matching the dnd
     hit-test): dashed + labeled so it can't be mistaken for adopt-as-tab. */
  .drop-ref {
    position: absolute;
    z-index: 7;
    inset: 78% 0 0 0;
    margin: 3px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    border: 1px dashed color-mix(in srgb, var(--accent) 60%, transparent);
    border-radius: 7px;
    pointer-events: none;
  }

  .drop-ref-at {
    color: var(--accent);
    font-weight: 600;
  }
  .drop-left {
    inset: 0 50% 0 0;
  }
  .drop-right {
    inset: 0 0 0 50%;
  }
  .drop-top {
    inset: 0 0 50% 0;
  }
  .drop-bottom {
    inset: 50% 0 0 0;
  }

  /* While a bottom band is armed for this drag, the adopt-as-tab preview
     stops above it — the band region is reserved, never flashed over. */
  .drop-center.banded {
    inset: 0 0 22% 0;
  }

  /* The link band: same quiet recipe as the "@ reference" band (one band
     grammar), tinted in the receiving agent's hue when it has one. */
  .drop-link {
    position: absolute;
    z-index: 7;
    inset: 78% 0 0 0;
    margin: 3px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    border: 1px dashed color-mix(in srgb, var(--accent) 55%, transparent);
    border-radius: 7px;
    pointer-events: none;
  }

  .drop-link.hued {
    background: hsl(var(--band-hue) 55% 55% / 0.1);
    border-color: hsl(var(--band-hue) 55% 55% / 0.55);
  }

  /* Link-intent whole-pane target: the same grammar as the band, but the wash
     covers the entire view — the agent is one big drop zone. */
  .drop-linkpane {
    position: absolute;
    z-index: 7;
    inset: 0;
    margin: 3px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border: 1.5px dashed color-mix(in srgb, var(--accent) 60%, transparent);
    border-radius: 8px;
    pointer-events: none;
  }

  .drop-linkpane.hued {
    background: hsl(var(--band-hue) 55% 55% / 0.14);
    border-color: hsl(var(--band-hue) 55% 55% / 0.6);
  }

  /* OS-desktop file drop onto a Finder pane: only a dashed frame at the pane
     edge — no wash, no label — because the Finder column/row under the
     pointer is what says where the file lands, and a wash would cover it. */
  .drop-frame {
    position: absolute;
    z-index: 7;
    inset: 0;
    margin: 3px;
    display: flex;
    align-items: flex-end;
    justify-content: center;
    padding-bottom: 12px;
    border: 1.5px dashed color-mix(in srgb, var(--accent) 45%, transparent);
    border-radius: 8px;
    pointer-events: none;
  }

  /* OS-desktop file drop: the whole pane is the upload-and-reference target
     (HTML5 dnd has no competing tile gesture to partition against), so the
     "@ reference" band recipe washes the entire view instead of a bottom band. */
  .drop-upload {
    position: absolute;
    z-index: 7;
    inset: 0;
    margin: 3px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border: 1.5px dashed color-mix(in srgb, var(--accent) 60%, transparent);
    border-radius: 8px;
    pointer-events: none;
  }

</style>

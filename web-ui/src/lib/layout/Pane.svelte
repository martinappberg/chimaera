<script lang="ts">
  import { keepsPaneViewAlive, tabKey, type PaneNode, type Tab } from "./layout";
  import { untrack, type Component } from "svelte";
  import type { Session } from "../workspace/sessions";
  import type { DropSpot, LayoutCtrl } from "./dnd";
  import { registerPane, unregisterPane } from "./dnd";
  import { agentHue, type LinkCtrl } from "../workspace/agentLinks";
  import { activeModLabel, keyHint } from "../shared/keybindings";
  import PaneTabs from "./PaneTabs.svelte";
  import { loadPaneView, type PaneViewKind } from "./lazyViews";
  import Spinner from "../previews/Spinner.svelte";
  import type { DashCtx } from "../dashboard/dash";

  interface Props {
    node: PaneNode;
    focusedPaneId: string;
    /** True when this pane is rendered zoomed (fullscreen in the window). */
    zoomed?: boolean;
    /** True when this is the only pane (hides the move-pane grip). */
    soloPane?: boolean;
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
    soloPane = false,
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
  /** An OS-desktop file drag hovering a Finder pane: upload INTO the folder
   *  under the pointer (the whole Finder pane washes; `dir` names the target). */
  const uploadDir = $derived(dropSpot?.kind === "uploadDir" && dropSpot.paneId === node.id ? dropSpot.dir : null);
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
      if (liveKeys.length > LIVE_CAP) liveKeys = liveKeys.slice(0, LIVE_CAP);
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
  let viewErrors = $state<Partial<Record<PaneViewKind, true>>>({});

  function requestView(kind: PaneViewKind) {
    void loadPaneView(kind).then(
      (view) => (views = { ...views, [kind]: view }),
      (error: unknown) => {
        // Lazy chunks are immutable per daemon build. A reconnect/update can
        // atomically replace that set underneath an already-open window; the
        // old import URL can never succeed on Retry. Keep the useful detail in
        // the console, and offer the one recovery that obtains the new entry.
        console.error(`could not load ${kind} view`, error);
        viewErrors = { ...viewErrors, [kind]: true };
      },
    );
  }

  function reloadWindow() {
    location.reload();
  }

  function viewKind(tab: Tab): PaneViewKind | null {
    if (tab.surface !== "terminal") return tab.surface;
    const session = sessions.get(tab.sessionId);
    if (session === undefined) return null;
    return session.ui === "chat" ? "chat" : "terminal";
  }

  $effect(() => {
    const needed = new Set(mountedTabs.map(viewKind).filter((kind) => kind !== null));
    for (const kind of needed) {
      if (views[kind] !== undefined || viewErrors[kind]) continue;
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

{#snippet loadFailure(label: string)}
  <div class="hint load-failure">
    <span>could not load {label}</span>
    <span class="load-detail">reload this window to restore the view</span>
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
          onSwitchToTerminal={() => ctrl.switchView(s.id, "term")}
          onForked={(forked: Session) =>
            ctrl.revealWorktreeSession(forked.id, forked.workspace_id)}
        />
      {:else if viewErrors.chat}
        {@render loadFailure("chat view")}
      {:else}
        <Spinner />
      {/if}
    {:else}
      {@const TerminalView = views.terminal}
      {#if TerminalView !== undefined}
        <TerminalView sessionId={tab.sessionId} focused={focused && active} fontSize={node.fontSize} />
      {:else if viewErrors.terminal}
        {@render loadFailure("terminal view")}
      {:else}
        <Spinner />
      {/if}
    {/if}
  {:else if tab.surface === "file"}
    {@const FileView = views.file}
    {#if FileView !== undefined}
      <FileView path={tab.path} {wsRoot} fontSize={node.fontSize} />
    {:else if viewErrors.file}
      {@render loadFailure("file view")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "finder"}
    {@const FinderView = views.finder}
    {#if FinderView !== undefined}
      <FinderView
        path={tab.path}
        {wsRoot}
        onOpenFile={(p: string, split: boolean) => ctrl.openFileFrom(node.id, p, split)}
        onNavigate={(p: string) => ctrl.navigateFinder(tab.id, p)}
      />
    {:else if viewErrors.finder}
      {@render loadFailure("Finder")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "diff"}
    {@const DiffView = views.diff}
    {#if DiffView !== undefined}
      <DiffView path={tab.path} mode={tab.mode} {wsId} />
    {:else if viewErrors.diff}
      {@render loadFailure("diff view")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "git"}
    {@const GitView = views.git}
    {#if GitView !== undefined}
      <GitView {wsId} paneId={node.id} {ctrl} {sessions} {names} onOpenSession={ctrl.revealWorktreeSession} />
    {:else if viewErrors.git}
      {@render loadFailure("git view")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "changes"}
    {@const cs = sessions.get(tab.sessionId)}
    {#if cs !== undefined}
      {@const SessionChangesView = views.changes}
      {#if SessionChangesView !== undefined}
        <SessionChangesView session={cs} {wsRoot} paneId={node.id} {ctrl} />
      {:else if viewErrors.changes}
        {@render loadFailure("changes view")}
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
      {@render loadFailure("dashboard")}
    {:else}
      <Spinner />
    {/if}
  {:else if tab.surface === "settings"}
    {@const SettingsView = views.settings}
    {#if SettingsView !== undefined}
      <SettingsView />
    {:else if viewErrors.settings}
      {@render loadFailure("settings")}
    {:else}
      <Spinner />
    {/if}
  {/if}
{/snippet}

<section
  class="pane"
  class:focused
  class:linked={linkHue !== null}
  class:agent-exec={agentExec}
  style:--hue={linkHue}
  tabindex="-1"
  bind:this={rootEl}
  onpointerdowncapture={() => ctrl.focusPane(node.id)}
>
  <!-- Every pane always has its top bar — orientation, drag handle, and the
       mouse home for zoom/split/close, even single-pane single-tab. -->
  <PaneTabs
    {node}
    {zoomed}
    {soloPane}
    {sessions}
    {names}
    {fileNames}
    {links}
    {linkCtrl}
    {dropSpot}
    {ctrl}
    bind:el={tabbarEl}
  />
  <div class="content" bind:this={contentEl}>
    <!-- Retained file/workbench/chat views stay mounted with the active one
         visible. PTY components remount against termPool, so inactive xterm
         elements park without making long chat transcripts reconstruct. -->
    {#each mountedTabs as tab (tabKey(tab))}
      {@const active = tab === activeTab}
      <div class="layer" class:active inert={!active}>
        {@render surface(tab, active)}
      </div>
    {/each}
    {#if activeTab === null}
      {#if names.size === 0}
        <!-- No sessions to open or drag yet: point at creating one. -->
        <div class="hint">
          <span><kbd>{keyHint("newAgent")}</kbd> new agent</span>
          <span class="hint-sep">·</span>
          <span><kbd>{keyHint("newTerminal")}</kbd> new terminal</span>
        </div>
      {:else}
        <div class="hint">
          <span><kbd>{activeModLabel()}1–9</kbd> opens a session</span>
          <span class="hint-sep">·</span>
          <span>drag one here</span>
        </div>
      {/if}
    {/if}
  </div>

  {#if zone !== null}
    <div class="drop drop-{zone}" class:banded={bandArmed}></div>
  {:else if linkBand}
    <!-- Distinct from the split/adopt zones: a labeled, dashed band over the
         agent's input area. Dropping links the terminal and types its
         @term: reference into the composer (never submits). -->
    <div class="drop-link" class:hued={ownAgentHue !== null} style:--band-hue={ownAgentHue}>
      <span class="band-label">link to this agent</span>
    </div>
  {:else if linkPane}
    <!-- Link-intent drag: the whole agent view is one target (no aiming for a
         band). Full-pane wash in the agent's hue, centered label. -->
    <div class="drop-linkpane" class:hued={ownAgentHue !== null} style:--band-hue={ownAgentHue}>
      <span class="band-label">link to this agent</span>
    </div>
  {/if}

  {#if refBand}
    <!-- Drag-to-reference: types the path into this session's input, never
         opens a tab, never submits. Visibly distinct from the adopt zone. -->
    <div class="drop-ref">
      <span class="drop-ref-label"><span class="drop-ref-at">@</span> reference</span>
    </div>
  {/if}

  {#if uploadPane}
    <!-- OS-desktop drop: uploads to the session's host, then types the
         path — same "@ reference" grammar, whole pane as the target. -->
    <div class="drop-upload">
      <span class="drop-ref-label"
        ><span class="drop-ref-at">@</span> drop to upload &amp; reference</span
      >
    </div>
  {:else if uploadDir !== null}
    <!-- OS-desktop drop onto a Finder pane: upload INTO the folder under the
         pointer (no @-reference — this is a file-manager drop). -->
    <div class="drop-upload">
      <span class="drop-ref-label">drop to upload here</span>
    </div>
  {/if}
</section>

<style>
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

  /* The agent is executing here: the border breathes in the agent's hue —
     peripheral-vision signal that the leash is being pulled. */
  .pane.agent-exec {
    animation: agent-exec-pulse 1.4s ease-in-out infinite;
  }

  @keyframes agent-exec-pulse {
    0%,
    100% {
      box-shadow: 0 0 0 0 hsl(var(--hue) 60% 55% / 0);
    }
    50% {
      box-shadow: 0 0 0 2px hsl(var(--hue) 60% 55% / 0.35);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .pane.agent-exec {
      animation: none;
      box-shadow: 0 0 0 2px hsl(var(--hue) 60% 55% / 0.3);
    }
  }

  .content {
    flex: 1;
    position: relative;
    min-height: 0;
    min-width: 0;
  }

  /* Keep-alive layers: every mounted tab fills the content box; only the active
     one is visible. visibility:hidden (not display:none) leaves inactive views
     laid out at full size, so xterm/CodeMirror stay correctly measured and a
     switch-back needs no reflow — the DOM, scroll, and image decode are intact.
     inert keeps hidden inputs/editors out of the focus + tab order. */
  .layer {
    position: absolute;
    inset: 0;
  }

  .layer:not(.active) {
    visibility: hidden;
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

  .hint kbd {
    font-family: var(--mono);
    font-size: var(--text-xs);
    padding: 0 0.25rem;
    border: 1px solid var(--edge);
    border-radius: 4px;
    background: none;
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

  /* Translucent drop-zone preview showing exactly where the drop lands. */
  .drop {
    position: absolute;
    z-index: 6;
    margin: 3px;
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

  .drop-ref-label {
    font-family: var(--mono);
    font-size: var(--text-xs);
    letter-spacing: 0.06em;
    color: var(--fg);
    background: color-mix(in srgb, var(--term-bg) 82%, transparent);
    border-radius: 4px;
    padding: 2px 8px;
    user-select: none;
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

  .band-label {
    font-family: var(--mono);
    font-size: var(--text-xs);
    letter-spacing: 0.06em;
    color: var(--fg);
    background: color-mix(in srgb, var(--term-bg) 82%, transparent);
    border-radius: 4px;
    padding: 2px 8px;
    user-select: none;
  }
</style>

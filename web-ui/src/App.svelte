<script lang="ts">
  import { onMount, tick } from "svelte";
  import {
    ApiError,
    getActiveWorkspaceId,
    getHostLabel,
    getToken,
    health as fetchHealth,
    notifyUnauthorized,
    pollHealth,
    refreshTokenFromHash,
    setActiveWorkspaceId,
    unauthorized,
    type Health,
  } from "./lib/api";
  import {
    createSession,
    deleteSession,
    deleteWorkspace,
    displayName,
    dotState,
    dotTitle,
    listSessions,
    listWorkspaces,
    renameSession,
    needsAttention,
    pollSessions,
    touchWorkspace,
    type AgentSpawn,
    type Session,
    type Workspace,
  } from "./lib/sessions";
  import {
    getAgentDefault,
    installAgent,
    listAgents,
    listRecents,
    relativeAge,
    setAgentDefault,
    type AgentInfo,
    type LaunchPick,
    type RecentConvo,
  } from "./lib/launcher";
  import { EventsSocket } from "./lib/events";
  import {
    agentHue,
    deleteLink,
    listLinks,
    putLink,
    termReference,
    type Link,
    type LinkCtrl,
  } from "./lib/agentLinks";
  import { reconnectingSockets, typeIntoDetachedSession } from "./lib/ws";
  import { get } from "svelte/store";
  import {
    activeSelection,
    clearSelection,
    composeAgentPathReference,
    composeFileReference,
    composeShellPathReference,
    composeTerminalReference,
    referenceTarget,
    composeProvenanceSuffix,
    setReferenceHandler,
    setSelection,
    workspaceRelative,
  } from "./lib/reference";
  import { provenanceFor, rememberCopy } from "./lib/provenance";
  import {
    activateTab,
    adjacentPane,
    allFilePaths,
    closePane,
    cycleTab,
    defaultLayout,
    deserializeLayout,
    detachTab,
    dropTab,
    dropTabAtRootEdge,
    findPane,
    focusPane,
    focusedFile as focusedFileOf,
    focusedSession as focusedSessionOf,
    moveFocus,
    moveTabToIndex,
    openDiff,
    openFile,
    openFinder,
    findFinder,
    setFinderPath,
    openGit,
    openSession,
    openSettings,
    paneForTab,
    panes as panesOf,
    pruneFiles,
    pruneSessions,
    serializeLayout,
    sessionPaneId,
    setPaneFont,
    setRatio,
    splitPane,
    tabCount,
    toggleZoom,
    type FocusDir,
    type Layout,
    type SplitDir,
    type Tab,
  } from "./lib/layout";
  import type { PathKind } from "./lib/links";
  import { basename, fileTabTitles, fsProbe, viewKindFor } from "./lib/files";
  import { dirtyFiles } from "./lib/editing";
  import { activateGitWorkspace, gitStatus, onGitNudge, type DiffMode } from "./lib/git";
  import {
    paneContentEl,
    paneRootEl,
    registerLinkRow,
    registerStage,
    startDrag,
    unregisterLinkRow,
    unregisterStage,
    type DropSpot,
    type LayoutCtrl,
  } from "./lib/dnd";
  import { chordDigit, fontChord, isAppChord, isLayer2, isMac, KEYS } from "./lib/keys";
  import {
    closeThisWindow,
    connectHost,
    focusWindow,
    isNativeShell,
    onHostStatus,
    onLocalDaemonUpdated,
    onMenu,
    reportWindowScope,
  } from "./lib/native";
  import * as pool from "./lib/termPool";
  import {
    applyRemoteSettings,
    flushSettings,
    loadSettings,
  } from "./lib/settings/store.svelte";
  import { flushViewState, loadViewState, saveViewState, windowKey } from "./lib/viewState";
  import {
    FILES_FRAC_MAX,
    FILES_FRAC_MIN,
    RAIL_DEFAULT,
    RAIL_MAX,
    RAIL_MIN,
    loadRailChrome,
    saveRailChrome,
  } from "./lib/railState";
  import { hintsActive, initChordHints } from "./lib/chordHints.svelte";
  import FolderPicker from "./lib/FolderPicker.svelte";
  import HomeScreen from "./lib/HomeScreen.svelte";
  import AskpassModal from "./lib/AskpassModal.svelte";
  import Launcher from "./lib/Launcher.svelte";
  import SessionGlyph from "./lib/SessionGlyph.svelte";
  import QuickOpen from "./lib/QuickOpen.svelte";
  import FileTree from "./lib/FileTree.svelte";
  import SplitTree from "./lib/SplitNode.svelte";
  import Pane from "./lib/Pane.svelte";

  let health = $state<Health | null>(null);
  let workspaces = $state<Workspace[]>([]);
  let sessions = $state<Session[]>([]);
  /** Linked-terminal edges, mirrored from /ws/events snapshots. */
  let links = $state<Link[]>([]);
  let activeWsId = $state<string | null>(getActiveWorkspaceId());
  let pickerOpen = $state(false);
  let quickOpenOpen = $state(false);
  /** Element focused when the quick-open palette opened; restored on close. */
  let quickOpenRestoreEl: HTMLElement | null = null;
  let eventsUp = $state(false);
  let createError = $state<string | null>(null);

  // --- remote window: auto-reconnect a dropped tunnel ------------------------
  /** This window's host alias ("local" for the local daemon). */
  const hostAlias = getHostLabel();
  const isRemoteWindow = hostAlias !== "local";
  /** Only the native shell can re-run ssh; a browser tunnel is the CLI's job. */
  const canReconnect = isRemoteWindow && isNativeShell();
  /** This window's scope key for the shell registry (null alias = local). */
  const scopeAlias = isRemoteWindow ? hostAlias : null;
  /** The reconnect overlay is showing (tunnel dropped, healing). */
  let showReconnect = $state(false);
  /** A connectHost call is in flight. */
  let reconnecting = $state(false);
  /** Last reconnect failure, surfaced with a Retry. */
  let reconnectError = $state<string | null>(null);
  /** The user dismissed the overlay; don't auto-reshow until state changes. */
  let reconnectDismissed = $state(false);
  let reconnectGrace: ReturnType<typeof setTimeout> | null = null;

  /** Re-establish this remote window's ssh tunnel. Idempotent: the shell
   *  reuses a live tunnel, and reuses the old loopback port so a heal needs no
   *  navigation. Surfaces the SSH auth modal (mounted app-wide) only if ssh
   *  actually re-prompts. */
  async function attemptReconnect(): Promise<void> {
    if (!canReconnect || reconnecting) return;
    reconnecting = true;
    reconnectError = null;
    try {
      await connectHost(hostAlias);
      // Same-port heal → our WebSocket reconnects and eventsUp clears the
      // overlay; a moved port/token re-homes this window via onHostStatus.
    } catch (e) {
      reconnectError = e instanceof Error ? e.message : String(e);
    } finally {
      reconnecting = false;
    }
  }

  function beginReconnect(): void {
    if (!canReconnect || eventsUp) return;
    reconnectDismissed = false;
    showReconnect = true;
    void attemptReconnect();
  }

  function dismissReconnect(): void {
    showReconnect = false;
    reconnectDismissed = true;
    if (reconnectGrace !== null) {
      clearTimeout(reconnectGrace);
      reconnectGrace = null;
    }
  }

  // A remote window whose events socket stays down past a short grace has lost
  // its tunnel (not just a daemon blip): show the overlay and reconnect. When
  // it recovers, clear everything. host-status "down" skips the grace.
  $effect(() => {
    if (!canReconnect) return;
    if (eventsUp) {
      if (reconnectGrace !== null) {
        clearTimeout(reconnectGrace);
        reconnectGrace = null;
      }
      showReconnect = false;
      reconnectError = null;
      reconnectDismissed = false;
      return;
    }
    if (reconnectGrace === null && !showReconnect && !reconnectDismissed) {
      reconnectGrace = setTimeout(() => {
        reconnectGrace = null;
        beginReconnect();
      }, 1500);
    }
  });

  // Keep the shell's window registry current so "open this workspace" raises
  // this window instead of duplicating it (the SPA swaps `ws` client-side, so
  // the shell can't see the change otherwise).
  $effect(() => {
    if (isNativeShell()) void reportWindowScope(scopeAlias, activeWsId);
  });
  // --- the agent launcher (split button + popover) ---
  /** The persisted default agent the split button's main surface spawns. */
  let agentDefault = $state(getAgentDefault());
  /** The host's agent catalog (install status per CLI), so the split button
   *  can reflect whether its default is installed. Null until first fetched;
   *  the launcher popover reports its fresher probe back via onAgents. */
  let agents = $state<AgentInfo[] | null>(null);
  /** Install sessions we spawned; when one exits the catalog is re-probed so
   *  the button flips from "install" to spawn without a manual refresh. */
  const pendingInstalls = new Set<string>();
  /** The catalog row for the default agent, once the catalog is loaded. */
  const defaultAgentInfo = $derived(
    agents?.find((a) => a.id === agentDefault.agent) ?? null,
  );
  /** Known-missing: the catalog is loaded and says the default isn't here.
   *  (Null catalog / unknown agent falls back to spawn — the common path.) */
  const defaultMissing = $derived(defaultAgentInfo !== null && !defaultAgentInfo.installed);
  /** Missing but chimaera can install it in place (managed runtime). */
  const defaultInstallable = $derived(defaultMissing && (defaultAgentInfo?.managedInstall ?? false));
  let launcherOpen = $state(false);
  /** The split button's rect at open time; the popover hangs below it. */
  let launcherAnchor = $state<DOMRect | null>(null);
  let newSplitEl = $state<HTMLElement | null>(null);
  /** Hover-intent timer (~150ms, chevron only) that opens the launcher. */
  let launcherHoverTimer: ReturnType<typeof setTimeout> | null = null;
  // --- the rail's Recents section (ended agent conversations) ---
  let recents = $state<RecentConvo[]>([]);
  let recentsExpanded = $state(false);
  /** Follow-up fetch timer: retirement lands on the daemon's ~2s watch tick. */
  let recentsTimer: ReturnType<typeof setTimeout> | null = null;
  /** Rail row currently showing the inline kill confirmation. */
  let confirmKillId = $state<string | null>(null);
  /** Rail row currently in inline rename (double-click or F2). */
  let renamingId = $state<string | null>(null);
  let renameDraft = $state("");
  /** Element that held focus when the picker opened; restored on close. */
  let pickerRestoreEl: HTMLElement | null = null;
  /** Feedback under the retry button on the re-auth overlay. */
  let authRetryMsg = $state<string | null>(null);
  let authRetrying = $state(false);

  // In-window layout: the tree is daemon-owned per-window view state; until
  // the GET resolves the stage stays blank (fast, local) so a restored tree
  // never flashes through the single-pane default.
  let layout = $state<Layout>(defaultLayout());
  let layoutReady = $state(false);
  let gotSessions = $state(false);
  let autoOpened = false;
  let dropSpot = $state<DropSpot | null>(null);
  /** Panes whose bottom band is armed for the CURRENT drag (reference or
   *  link targets) — zone previews stop above the band on these. */
  let bandPanes = $state<ReadonlySet<string>>(new Set());

  /** File-tree reveal request (terminal dir links); nonce distinguishes repeats. */
  let treeReveal = $state<{ path: string; nonce: number } | null>(null);

  // Rail chrome (width + FILES section) is a window preference, persisted
  // locally so it survives reload and holds across workspace switches. Collapse
  // is not stored here — it maps onto the layout's focus mode (which persists
  // and carries the strip). Loaded once, clamped, before the first paint.
  const railChrome = loadRailChrome();
  /** Draggable sidebar width; the inline width wins unless focus mode collapses it. */
  let railWidth = $state(railChrome.width);
  let railDividerActive = $state(false);
  // Rail FILES section: collapsible, resizable share of the rail height.
  let filesOpen = $state(railChrome.filesOpen);
  let filesFrac = $state(railChrome.filesFrac);
  let filesDividerActive = $state(false);
  let railEl = $state<HTMLElement | null>(null);
  let daemonEl = $state<HTMLElement | null>(null);
  /** The stage element; its edges are the root-split drop targets. */
  let stageEl = $state<HTMLElement | null>(null);

  const winKey = windowKey();

  /**
   * View-state key: (window id, workspace id) composed client-side, so
   * switching workspaces away and back restores that workspace's layout.
   * Server key charset is [A-Za-z0-9_-]{1,64}; uuid + "_" + "w-xxxxxxxx" fits.
   */
  function stateKey(wsId: string | null): string {
    return wsId === null ? winKey : `${winKey}_${wsId}`;
  }

  const workspace = $derived(workspaces.find((w) => w.id === activeWsId) ?? null);
  const wsSessions = $derived(sessions.filter((s) => s.workspace_id === activeWsId));

  // Keep the git store pointed at the active workspace. It fetches status on
  // change; the events-socket epoch nudge refetches on subsequent changes.
  $effect(() => {
    void activateGitWorkspace(activeWsId);
  });
  // The rail groups terminals (few) above agents (many); this order is also
  // the mod+1–9 chord order and the focus-mode strip order, so what you see
  // is what the numbers mean.
  const shellSessions = $derived(wsSessions.filter((s) => s.kind !== "agent"));
  const agentSessions = $derived(wsSessions.filter((s) => s.kind === "agent"));
  const railSessions = $derived([...shellSessions, ...agentSessions]);
  // The first nine rail rows carry the ⌘1–9 chord; this map is what the
  // which-key hints (rail badges + strip chips) read to label them.
  const chordDigits = $derived(
    new Map(railSessions.slice(0, 9).map((s, i) => [s.id, i + 1] as const)),
  );
  const sessionsById = $derived(new Map(sessions.map((s) => [s.id, s])));
  /** terminal session id -> agent session id (one agent per terminal). */
  const linksByTerminal = $derived(new Map(links.map((l) => [l.terminal_id, l.agent_id])));
  const focusedSessionId = $derived(focusedSessionOf(layout));
  const focusedFilePath = $derived(focusedFileOf(layout));
  /** Open file tabs' display titles (basename, disambiguated by parent dir). */
  const fileTitles = $derived(fileTabTitles(allFilePaths(layout)));
  const zoomedPane = $derived(
    layout.zoomedPaneId !== null ? findPane(layout.root, layout.zoomedPaneId) : null,
  );

  /** Sessions in the active workspace waiting on the user. */
  const needsYou = $derived(wsSessions.filter(needsAttention).length);

  // --- context bridge: reference target resolution ---------------------------

  /** Agent sessions this window focused, most recent first. */
  let agentMru = $state<string[]>([]);
  $effect(() => {
    const sid = focusedSessionId;
    if (sid === null || sessionsById.get(sid)?.kind !== "agent") return;
    if (agentMru[0] === sid) return;
    agentMru = [sid, ...agentMru.filter((x) => x !== sid)].slice(0, 16);
  });

  /**
   * Where a reference lands, most-explicit first: the agent LINKED to the
   * selection's source terminal (the leash is the bridge the user built),
   * else the focused agent session, else the workspace's most recently
   * active agent, else its newest live agent. Null (no agent session at
   * all) renders every reference affordance disabled.
   */
  const refTargetSession = $derived.by(() => {
    const sel = $activeSelection;
    if (sel !== null && sel.kind === "terminal") {
      const leash = linksByTerminal.get(sel.sessionId);
      const linked = leash !== undefined ? sessionsById.get(leash) : undefined;
      if (
        linked !== undefined &&
        linked.kind === "agent" &&
        linked.alive &&
        linked.workspace_id === activeWsId
      ) {
        return linked;
      }
    }
    const focused = focusedSessionId !== null ? sessionsById.get(focusedSessionId) : undefined;
    if (
      focused !== undefined &&
      focused.kind === "agent" &&
      focused.alive &&
      focused.workspace_id === activeWsId
    ) {
      return focused;
    }
    for (const id of agentMru) {
      const s = sessionsById.get(id);
      if (s !== undefined && s.kind === "agent" && s.alive && s.workspace_id === activeWsId) {
        return s;
      }
    }
    let latest: Session | null = null;
    for (const s of wsSessions) {
      if (s.kind === "agent" && s.alive && (latest === null || s.created_at >= latest.created_at)) {
        latest = s;
      }
    }
    return latest;
  });

  // Publish the target for the affordances (chips + pane-bar button).
  $effect(() => {
    const t = refTargetSession;
    referenceTarget.set(
      t === null ? null : { id: t.id, name: displayNames.get(t.id) ?? displayName(t) },
    );
  });

  // Row name is the server-resolved display name (naming rule zero), with
  // the " · n" suffix only as a duplicate tiebreaker within the workspace.
  const displayNames = $derived.by(() => {
    const counts = new Map<string, number>();
    const names = new Map<string, string>();
    for (const s of wsSessions) {
      const base = displayName(s);
      const n = (counts.get(base) ?? 0) + 1;
      counts.set(base, n);
      names.set(s.id, n === 1 ? base : `${base} · ${n}`);
    }
    return names;
  });

  // Health polling keeps the hostname fresh and trips the 401 overlay; the
  // daemon dot itself tracks the authenticated events socket.
  $effect(() =>
    pollHealth(
      (h) => {
        health = h;
      },
      () => {
        // unreachable daemon; the events socket state already reflects this
      },
    ),
  );

  // /ws/events pushes full session snapshots; the 5s poll only runs as a
  // fallback while the socket is down (including before the first frame).
  // Links normally ride the socket frames; the fallback refreshes them too.
  $effect(() => {
    if (eventsUp) return;
    return pollSessions(
      (list) => {
        applySessions(list);
        listLinks().then(
          (l) => (links = l),
          () => {
            // stale links until the daemon is reachable again
          },
        );
      },
      () => {
        // transient poll failure; the daemon dot already reflects reachability
      },
    );
  });

  // Persist the layout (debounced in viewState) whenever it changes, keyed
  // by (window, workspace) so each workspace keeps its own tree.
  $effect(() => {
    const blob = { v: 1, ws: activeWsId, layout: serializeLayout(layout) };
    if (!layoutReady) return;
    saveViewState(stateKey(activeWsId), blob);
  });

  // Persist rail chrome (width + FILES section) locally on any change. Drags
  // are rAF-throttled upstream, so the localStorage write rate stays bounded.
  $effect(() => {
    saveRailChrome({ width: railWidth, filesOpen, filesFrac });
  });

  // The dnd hit-tester needs the stage rect for window-edge drop targets.
  $effect(() => {
    const el = stageEl;
    if (el === null) return;
    registerStage(el);
    return () => unregisterStage(el);
  });

  // Dispose pooled terminals for sessions that no longer exist.
  $effect(() => {
    const ids = sessions.map((s) => s.id);
    if (!gotSessions) return;
    pool.syncSessions(ids);
  });

  onMount(() => {
    pool.initPool({
      onTitle,
      onExited,
      onSocketError,
      onSelection: onTermSelection,
      onPaste: onTermPaste,
      linkContext,
      onOpenPath,
    });
    setReferenceHandler(referenceSelection);
    const events = new EventsSocket({
      onSessions: (list, linkList) => {
        applySessions(list);
        if (linkList !== undefined) links = linkList;
      },
      onSettings: applyRemoteSettings,
      onGit: onGitNudge,
      onStatus: (up) => (eventsUp = up),
      onFatal: (message) => {
        if (message === "unauthorized") notifyUnauthorized();
      },
    });
    refreshWorkspaces();
    void bootViewState();
    void loadSettings();
    void refreshAgents();

    // Native menu items the shell forwards to the focused window. Cmd+W
    // closes the focused VIEW (a home window just closes), reclaiming the
    // chords a browser reserves for tabs.
    let unlistenMenu: (() => void) | null = null;
    let unlistenDaemonMoved: (() => void) | null = null;
    let unlistenHostStatus: (() => void) | null = null;
    if (isNativeShell()) {
      void onMenu((action) => {
        switch (action) {
          case "close-view":
            if (activeWsId === null) closeThisWindow();
            else if (layoutReady) closeView(layout.focusedPaneId);
            break;
          case "new-terminal":
            newShell();
            break;
          case "new-agent":
            newAgentPrimary();
            break;
        }
      }).then((u) => (unlistenMenu = u));
      // The local daemon was replaced (self-update): its old origin is
      // gone. Windows on the local daemon re-home themselves to the new
      // port + token, keeping their workspace; tunnel windows are unmoved.
      void onLocalDaemonUpdated(({ port, token }) => {
        if (getHostLabel() !== "local") return;
        const params = new URLSearchParams();
        params.set("token", token);
        if (activeWsId !== null) params.set("ws", activeWsId);
        location.replace(`http://127.0.0.1:${port}/#${params.toString()}`);
      }).then((u) => (unlistenDaemonMoved = u));
      // This remote window's tunnel dropped or came back. "down" → reconnect
      // now (the shell confirmed the forward is dead); "connected" → re-home
      // only if the origin actually moved (daemon restart / update), else the
      // heal is in place and the WebSocket just reconnects.
      void onHostStatus((e) => {
        if (!canReconnect || e.alias !== hostAlias) return;
        if (e.status === "down") {
          beginReconnect();
          return;
        }
        const port = e.local_port;
        const token = e.token ?? null;
        const portMoved = port !== null && String(port) !== location.port;
        const tokenMoved = token !== null && token !== getToken();
        if (portMoved || tokenMoved) {
          const params = new URLSearchParams();
          if (token !== null) params.set("token", token);
          if (activeWsId !== null) params.set("ws", activeWsId);
          params.set("host", hostAlias);
          location.replace(`http://127.0.0.1:${port ?? location.port}/#${params.toString()}`);
        }
      }).then((u) => (unlistenHostStatus = u));
    }

    const onPagehide = () => {
      void flushViewState();
      void flushSettings();
    };
    const onCopy = () => rememberCopy();
    // Which-key discovery: holding the app modifier fades in the ⌘1–9 badges.
    const stopChordHints = initChordHints();
    window.addEventListener("keydown", onKeydown, true);
    window.addEventListener("pagehide", onPagehide);
    document.addEventListener("copy", onCopy);
    return () => {
      window.removeEventListener("keydown", onKeydown, true);
      window.removeEventListener("pagehide", onPagehide);
      unlistenMenu?.();
      unlistenDaemonMoved?.();
      unlistenHostStatus?.();
      document.removeEventListener("copy", onCopy);
      stopChordHints();
      setReferenceHandler(null);
      events.close();
      pool.disposePool();
    };
  });

  // --- context bridge: selection -> reference --------------------------------

  /** xterm selection changes (pool callback): publish/clear per session. */
  function onTermSelection(id: string, text: string): void {
    if (text.trim().length > 0) {
      setSelection(`term:${id}`, { kind: "terminal", sessionId: id, text });
    } else {
      clearSelection(`term:${id}`);
    }
  }

  /**
   * Copy provenance, the paste half: a snippet copied from a tracked view
   * and pasted into an AGENT composer grows a visible ` [from …] ` tag
   * right after it. The pasted bytes are untouched, shells are never
   * touched, and the tag types AFTER xterm forwards the paste (microtask).
   */
  function onTermPaste(id: string, text: string): void {
    const target = sessionsById.get(id);
    if (target === undefined || target.kind !== "agent" || !target.alive) return;
    const source = provenanceFor(text);
    if (source === null) return;
    // Pasting a snippet back into where it came from needs no tag.
    if (source.kind === "terminal" && source.sessionId === id) return;
    const root = workspace?.root;
    const suffix = composeProvenanceSuffix(
      source,
      source.kind === "file" && root !== undefined
        ? workspaceRelative(source.path, root)
        : null,
      source.kind === "terminal"
        ? (displayNames.get(source.sessionId) ?? "terminal")
        : null,
    );
    queueMicrotask(() => pool.sendText(id, suffix));
  }

  /**
   * Type `text` into a session's input: through its pooled socket when the
   * terminal is warm, else a one-shot socket. The composed text never carries
   * a newline, so nothing is ever submitted. If the session is open in a
   * pane, surface it so the typed reference is reviewable at a glance.
   */
  function typeIntoSession(id: string, text: string): void {
    if (!pool.sendText(id, text)) typeIntoDetachedSession(id, text);
    const loc = paneForTab(layout.root, { surface: "terminal", sessionId: id });
    if (loc !== null) {
      layout = activateTab(layout, loc.paneId, loc.index);
      pool.focusTerminal(id);
    }
  }

  /**
   * The one reference entry point (chord, floating chips, pane-bar button —
   * parity principle): compose the active selection and type it into the
   * target agent's input, never submitting.
   */
  function referenceSelection(): void {
    const sel = get(activeSelection);
    const target = refTargetSession;
    if (sel === null || target === null) return;
    let text: string;
    if (sel.kind === "file") {
      const root = workspace?.root;
      const rel = root !== undefined ? workspaceRelative(sel.path, root) : sel.path;
      text = composeFileReference(rel, sel.startLine, sel.endLine, sel.text);
    } else {
      const src = sessionsById.get(sel.sessionId);
      const name =
        displayNames.get(sel.sessionId) ?? (src !== undefined ? displayName(src) : "terminal");
      text = composeTerminalReference(name, sel.text);
    }
    // A reference never lands out of sight: surface the target agent first,
    // splitting beside the selection's own pane when it is not open anywhere.
    if (sessionPaneId(layout, target.id) === null) {
      const beside =
        (sel.kind === "terminal" ? sessionPaneId(layout, sel.sessionId) : null) ??
        layout.focusedPaneId;
      layout = splitPane(layout, beside, "row");
      layout = openSession(layout, target.id);
    }
    typeIntoSession(target.id, text);
  }

  /**
   * Drag-to-reference drop: type the dropped file's path into the pane's
   * session — claude agents get the native @mention (workspace-relative),
   * plain terminals get the shell-escaped path relative to their live cwd.
   */
  function referenceFileDrop(paneId: string, path: string): void {
    const p = findPane(layout.root, paneId);
    if (p === null) return;
    const active = p.tabs[p.active];
    if (active === undefined || active.surface !== "terminal") return;
    const s = sessionsById.get(active.sessionId);
    if (s === undefined || !s.alive) return;
    const root = workspace?.root;
    const text =
      s.kind === "agent"
        ? composeAgentPathReference(root !== undefined ? workspaceRelative(path, root) : path)
        : composeShellPathReference(path, s.cwd_current ?? s.cwd);
    typeIntoSession(active.sessionId, text);
  }

  // --- clickable paths: the bridge's return direction ------------------------

  /** Resolution context for a session's terminal link provider. */
  function linkContext(id: string): { cwd: string | null; root: string | null } {
    const s = sessionsById.get(id);
    const root = workspaces.find((w) => w.id === s?.workspace_id)?.root ?? workspace?.root ?? null;
    return { cwd: s?.cwd_current ?? s?.cwd ?? null, root };
  }

  /** A confirmed terminal path link was activated. */
  function onOpenPath(id: string, path: string, kind: PathKind, newSplit: boolean): void {
    const loc = paneForTab(layout.root, { surface: "terminal", sessionId: id });
    const fromPane = loc?.paneId ?? layout.focusedPaneId;
    if (kind === "dir") {
      // Directory names open in the Finder (browsing anywhere, in or out of the
      // workspace); an in-workspace dir is ALSO revealed in the FILES side-tree
      // (revealInTree is a no-op outside the root).
      openDirInFinder(fromPane, path, newSplit);
      revealInTree(path);
      return;
    }
    openFileFromPane(fromPane, path, newSplit);
  }

  /**
   * Open a file surfaced FROM a pane (terminal link, touched-files popover):
   * an already-open tab is focused wherever it lives (no duplicates);
   * otherwise it lands in the adjacent pane, or a fresh split to the right
   * when the window has one pane or Cmd/Ctrl forced a new split.
   */
  function openFileFromPane(paneId: string, path: string, newSplit: boolean): void {
    const existing = paneForTab(layout.root, { surface: "file", path });
    if (existing !== null) {
      layout = activateTab(layout, existing.paneId, existing.index);
    } else {
      const neighbor = newSplit ? null : adjacentPane(layout, paneId);
      if (neighbor !== null) {
        layout = openFile(focusPane(layout, neighbor), path);
      } else {
        layout = splitPane(layout, paneId, "row");
        layout = openFile(layout, path);
      }
    }
    // A file surface took focus: pull DOM focus off the terminal so plain
    // keys stop reaching a PTY that is no longer the focused view.
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** Same grammar as openFileFromPane, for a diff opened from the git panel. */
  function openDiffFromPane(paneId: string, path: string, mode: DiffMode, newSplit: boolean): void {
    const existing = paneForTab(layout.root, { surface: "diff", path, mode });
    if (existing !== null) {
      layout = activateTab(layout, existing.paneId, existing.index);
    } else {
      const neighbor = newSplit ? null : adjacentPane(layout, paneId);
      if (neighbor !== null) {
        layout = openDiff(focusPane(layout, neighbor), path, mode);
      } else {
        layout = splitPane(layout, paneId, "row");
        layout = openDiff(layout, path, mode);
      }
    }
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** Open (or focus) the source-control panel in the focused pane. */
  function openGitPanel(): void {
    if (activeWsId === null || !layoutReady) return;
    layout = openGit(layout);
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** Dir links: open the FILES section and reveal the path in the tree. */
  let revealNonce = 0;
  function revealInTree(path: string): void {
    filesOpen = true;
    treeReveal = { path, nonce: ++revealNonce };
  }

  // --- the Finder (Miller-columns file browser) ------------------------------

  /** Finder instances this window focused, most recent first (dir-link reuse). */
  let finderMru = $state<string[]>([]);
  $effect(() => {
    const fp = findPane(layout.root, layout.focusedPaneId);
    const active = fp?.tabs[fp.active];
    if (active === undefined || active.surface !== "finder") return;
    const id = active.id;
    if (finderMru[0] === id) return;
    finderMru = [id, ...finderMru.filter((x) => x !== id)].slice(0, 16);
  });

  /** Every open Finder instance's id, in tree order. */
  function allFinderIds(): string[] {
    const out: string[] = [];
    for (const p of panesOf(layout.root)) {
      for (const t of p.tabs) if (t.surface === "finder") out.push(t.id);
    }
    return out;
  }

  /**
   * The Finder a directory link should drive: the one in the focused pane, else
   * the most-recently-focused still-open one, else any open one. Null when none
   * is open (the caller opens a fresh Finder instead).
   */
  function targetFinderId(): string | null {
    const fp = findPane(layout.root, layout.focusedPaneId);
    const active = fp?.tabs[fp.active];
    if (active !== undefined && active.surface === "finder") return active.id;
    for (const fid of finderMru) if (findFinder(layout, fid) !== null) return fid;
    return allFinderIds()[0] ?? null;
  }

  /** The FILES-header button / menu: open a fresh Finder at the workspace root. */
  function openFinderSurface(): void {
    if (activeWsId === null || !layoutReady || workspace === null) return;
    layout = openFinder(layout, workspace.root).layout;
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /**
   * A directory link from a pane: reuse an open Finder (unless Cmd/Ctrl forced a
   * split), else open a fresh Finder beside the source terminal — adjacent pane,
   * or a split to the right when the window has one pane.
   */
  function openDirInFinder(paneId: string, path: string, newSplit: boolean): void {
    const targetId = newSplit ? null : targetFinderId();
    if (targetId !== null) {
      layout = setFinderPath(layout, targetId, path);
      const loc = findFinder(layout, targetId);
      if (loc !== null) layout = activateTab(layout, loc.paneId, loc.index);
    } else {
      const neighbor = newSplit ? null : adjacentPane(layout, paneId);
      if (neighbor !== null) {
        layout = openFinder(focusPane(layout, neighbor), path).layout;
      } else {
        layout = splitPane(layout, paneId, "row");
        layout = openFinder(layout, path).layout;
      }
    }
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** Guards overlapping boots (initial load racing a workspace switch). */
  let bootSeq = 0;

  /** Restore this (window, workspace)'s layout from the daemon; anything
   *  missing/invalid (including a not-yet-upgraded daemon) falls back to the
   *  default. Blobs written before per-workspace keys are read from the bare
   *  window key when they match the active workspace. */
  async function bootViewState(): Promise<void> {
    const seq = ++bootSeq;
    const wsAtBoot = activeWsId;
    const matches = (raw: unknown): boolean =>
      typeof raw === "object" &&
      raw !== null &&
      (raw as { v?: unknown }).v === 1 &&
      (raw as { ws?: unknown }).ws === wsAtBoot;
    // A slow/hung daemon must never leave the stage blank forever: after 3s
    // the default layout renders and a late restore is simply dropped.
    const timeout = new Promise<null>((resolve) => setTimeout(() => resolve(null), 3000));
    let raw = await Promise.race([loadViewState(stateKey(wsAtBoot)), timeout]);
    if (!matches(raw) && wsAtBoot !== null) {
      // pre-composite-key blob migration
      raw = await Promise.race([loadViewState(winKey), timeout]);
    }
    if (seq !== bootSeq) return; // a later switch superseded this boot
    if (matches(raw)) {
      const restored = deserializeLayout((raw as { layout?: unknown }).layout);
      if (restored !== null) layout = restored;
    }
    layoutReady = true;
    pruneAndAutoOpen();
    pruneDeadFiles();
  }

  /** Drop restored file tabs whose files are definitively gone (400/404);
   *  an unreachable or not-yet-upgraded daemon never wipes tabs. */
  function pruneDeadFiles(): void {
    const paths = allFilePaths(layout);
    if (paths.length === 0) return;
    void Promise.all(paths.map(async (p) => [p, await fsProbe(p)] as const)).then((results) => {
      const dead = new Set(results.filter(([, r]) => r === "dead").map(([p]) => p));
      if (dead.size > 0) layout = pruneFiles(layout, dead);
    });
  }

  /**
   * The chords intercepted even when a terminal has focus (capture phase).
   * Modifier policy: Cmd-based on macOS, Ctrl+Shift-based elsewhere — the
   * terminal owns bare Ctrl on every platform (tmux Ctrl+B, EOF Ctrl+D,
   * zsh/vim Ctrl+O all reach the PTY untouched). The second layer is Shift
   * on macOS and Alt elsewhere (see keys.ts):
   *   mod+O picker toggle · mod+1..9 open Nth session · mod+E terminal /
   *   mod2+E agent · mod+D / mod2+D splits · mod+Backspace close view ·
   *   mod+Alt+arrows focus · mod+Alt+[ ] tabs · mod2+Enter zoom · mod+B
   *   focus mode. Cmd+W/Cmd+T/Cmd+Shift+W stay unbound (browser-reserved).
   */
  function onKeydown(e: KeyboardEvent): void {
    // Per-pane text size (Cmd/Ctrl +/−/0, spec-pinned chords): intercepted
    // ONLY while the focused pane shows a font-sizable surface (a terminal or
    // a rendered markdown document), so browser zoom keeps working elsewhere.
    if (!pickerOpen && !quickOpenOpen && layoutReady) {
      const step = fontChord(e);
      if (step !== null) {
        const p = findPane(layout.root, layout.focusedPaneId);
        const active = p?.tabs[p.active];
        const sizable =
          active !== undefined &&
          (active.surface === "terminal" ||
            (active.surface === "file" && viewKindFor(active.path) === "markdown"));
        if (p !== null && sizable) {
          e.preventDefault();
          e.stopPropagation();
          adjustFont(p.id, step);
          return;
        }
      }
    }
    if (!isAppChord(e)) return;
    const l2 = isLayer2(e);
    const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
    // On macOS, Alt is reserved for the navigation layer, so letter/digit
    // chords must not fire with Alt held. Elsewhere Alt IS the second layer.
    const plain = !isMac || !e.altKey;
    const intercept = () => {
      e.preventDefault();
      e.stopPropagation();
    };

    if (key === "o" && !l2 && plain) {
      intercept();
      if (pickerOpen) closePicker();
      else openPicker();
      return;
    }
    if (pickerOpen) return;
    if (activeWsId === null || !layoutReady) return;

    // Quick-open palette (files + sessions). Toggles like the folder picker;
    // while open it owns the keyboard, so all other chords stand down.
    if (key === "p" && !l2 && plain) {
      intercept();
      if (quickOpenOpen) closeQuickOpen();
      else openQuickOpen();
      return;
    }
    if (quickOpenOpen) return;

    // Navigation layer (Alt on both platforms): arrows move pane focus,
    // brackets cycle the focused pane's tabs.
    if (e.altKey) {
      const dirs: Record<string, FocusDir> = {
        ArrowLeft: "left",
        ArrowRight: "right",
        ArrowUp: "up",
        ArrowDown: "down",
      };
      const dir = dirs[e.key];
      if (dir !== undefined) {
        intercept();
        focusDirection(dir);
        return;
      }
      if (e.code === "BracketLeft") {
        intercept();
        cycle(-1);
        return;
      }
      if (e.code === "BracketRight") {
        intercept();
        cycle(1);
        return;
      }
    }

    // Context bridge: reference the current selection in the target agent.
    // Spec-pinned chord — ⇧⌘R on macOS, Ctrl+Shift+R elsewhere. Intercepts
    // only while a selection exists, so the browser's reload chord survives
    // when there is nothing to reference. Plain Cmd+C is never touched.
    if (key === "r" && (isMac ? l2 : !l2) && plain) {
      if (get(activeSelection) !== null) {
        intercept();
        referenceSelection();
      }
      return;
    }

    if (key === "d" && plain) {
      intercept();
      split(l2 ? "col" : "row");
      return;
    }
    if (key === "e" && plain) {
      intercept();
      // mod2+E does what the split button's main surface does — spawn the
      // persisted default agent, or install it when it's missing; the
      // launcher popover stays mouse-opened, nothing chord-new.
      if (l2) newAgentPrimary();
      else newShell();
      return;
    }
    if (key === "Enter" && l2) {
      intercept();
      layout = toggleZoom(layout);
      return;
    }
    if (key === "Backspace" && !l2 && plain) {
      intercept();
      closeView(layout.focusedPaneId);
      return;
    }
    if (key === "b" && !l2 && plain) {
      intercept();
      layout = { ...layout, focusMode: !layout.focusMode };
      return;
    }
    if (key === "," && !l2 && plain) {
      intercept();
      openSettingsSurface();
      return;
    }
    const n = chordDigit(e);
    if (n !== null && !l2 && plain && n <= railSessions.length) {
      intercept();
      openSess(railSessions[n - 1].id);
    }
  }

  $effect(() => {
    const base = workspace ? `${workspace.name} — chimaera` : "chimaera";
    document.title = needsYou > 0 ? `(${needsYou}) ${base}` : base;
  });

  /**
   * Refresh the workspace list; if the tab's stored workspace no longer
   * exists on the daemon, clear it and fall back to the empty state.
   */
  function refreshWorkspaces(): void {
    void listWorkspaces()
      .then((list) => {
        workspaces = list;
        if (activeWsId !== null && !list.some((w) => w.id === activeWsId)) {
          activeWsId = null;
          setActiveWorkspaceId(null);
        }
      })
      .catch(() => {
        // daemon unreachable; health polling surfaces this
      });
  }

  function openPicker(): void {
    if (pickerOpen) return;
    pickerRestoreEl = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    refreshWorkspaces();
    pickerOpen = true;
  }

  /** Close the picker and put focus back where it was (or on the focused
   *  session's terminal), so keystrokes never die on <body>. */
  function closePicker(): void {
    pickerOpen = false;
    const el = pickerRestoreEl;
    pickerRestoreEl = null;
    void tick().then(() => {
      if (el !== null && el.isConnected) {
        el.focus();
        return;
      }
      const sid = focusedSessionOf(layout);
      if (sid !== null) pool.focusTerminal(sid);
    });
  }

  function openQuickOpen(): void {
    if (quickOpenOpen || activeWsId === null) return;
    quickOpenRestoreEl =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    quickOpenOpen = true;
  }

  /** Close the palette and put focus back where it was (or the focused terminal). */
  function closeQuickOpen(): void {
    quickOpenOpen = false;
    const el = quickOpenRestoreEl;
    quickOpenRestoreEl = null;
    void tick().then(() => {
      if (el !== null && el.isConnected) {
        el.focus();
        return;
      }
      const sid = focusedSessionOf(layout);
      if (sid !== null) pool.focusTerminal(sid);
    });
  }

  /** Quick-open: open a file in the focused pane, or in a fresh split. The
   *  palette's own onClose fires right after; drop the restore target so it
   *  doesn't yank focus back to a now-hidden terminal. */
  function quickOpenFile(path: string, split: boolean): void {
    quickOpenRestoreEl = null;
    if (split) splitAt(layout.focusedPaneId, "row");
    openFilePath(path);
  }

  /** Quick-open: focus a session in the focused pane, or in a fresh split. */
  function quickOpenSession(id: string, split: boolean): void {
    quickOpenRestoreEl = null;
    if (split) splitAt(layout.focusedPaneId, "row");
    openSess(id);
  }

  // beforeunload guard: warn before losing unsaved edits (any dirty file).
  $effect(() => {
    if ($dirtyFiles.size === 0) return;
    const handler = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  });

  /** Scope this window to `w` (open in THIS window) — unless it's already
   *  open in another window, in which case raise that one instead. */
  async function activateWorkspace(w: Workspace): Promise<void> {
    if (isNativeShell() && (await focusWindow(scopeAlias, w.id))) {
      closePicker();
      return;
    }
    workspaces = workspaces.some((x) => x.id === w.id)
      ? workspaces.map((x) => (x.id === w.id ? w : x))
      : [w, ...workspaces];
    const switched = activeWsId !== w.id;
    closePicker();
    createError = null;
    // Stamp recency for the home screen (fire-and-forget; old daemons 404).
    void touchWorkspace(w.id).catch(() => {});
    if (!switched) return;
    // Flush the outgoing workspace's pending layout write under its own key,
    // then restore (or default) the incoming workspace's tree.
    void flushViewState();
    activeWsId = w.id;
    setActiveWorkspaceId(w.id);
    layoutReady = false;
    layout = defaultLayout();
    autoOpened = false;
    void bootViewState();
  }

  /** Home screen: unregister a workspace (the folder itself is untouched). */
  function removeWorkspace(w: Workspace): void {
    workspaces = workspaces.filter((x) => x.id !== w.id);
    void deleteWorkspace(w.id)
      .catch(() => {
        // already gone or unreachable; the refresh below reconciles
      })
      .finally(refreshWorkspaces);
  }

  /** Agent ids in the previous snapshot. A vanished agent just retired into
   *  the workspace's recents; an appeared one may be a resumed conversation
   *  leaving them — either way the rail section refetches. */
  let prevAgentIds = new Set<string>();

  function applySessions(list: Session[]): void {
    list.sort((a, b) => a.created_at - b.created_at || a.id.localeCompare(b.id));
    sessions = list;
    gotSessions = true;
    // An install session that has exited (gone from the roster, or alive:false)
    // means the catalog may have changed: re-probe so the button reflects it.
    if (pendingInstalls.size > 0) {
      for (const id of pendingInstalls) {
        const s = list.find((x) => x.id === id);
        if (s === undefined || !s.alive) {
          pendingInstalls.delete(id);
          void refreshAgents(true);
        }
      }
    }
    const agentIds = new Set(list.filter((s) => s.kind === "agent").map((s) => s.id));
    const changed =
      agentIds.size !== prevAgentIds.size || [...prevAgentIds].some((id) => !agentIds.has(id));
    prevAgentIds = agentIds;
    if (changed) refreshRecents(true);
    pruneAndAutoOpen();
  }

  /**
   * Sessions created by this window in the last few seconds. A sessions
   * snapshot fetched BEFORE the create but arriving AFTER it would otherwise
   * prune the fresh tab right out of the layout (stale-poll race).
   */
  const recentlyCreated = new Map<string, number>();
  const RECENT_MS = 10_000;

  /**
   * Once both the persisted layout and the first session snapshot are in:
   * drop tabs whose sessions vanished (also on every later snapshot), and —
   * exactly once — populate a pristine layout with the first session.
   */
  function pruneAndAutoOpen(): void {
    if (!layoutReady || !gotSessions) return;
    const live = new Set(sessions.map((s) => s.id));
    const now = Date.now();
    for (const [id, ts] of recentlyCreated) {
      if (now - ts > RECENT_MS) recentlyCreated.delete(id);
      else live.add(id);
    }
    layout = pruneSessions(layout, live);
    if (!autoOpened) {
      autoOpened = true;
      if (tabCount(layout) === 0 && railSessions.length > 0) {
        layout = openSession(layout, railSessions[0].id);
      }
    }
  }

  function onTitle(id: string, title: string): void {
    const s = sessions.find((x) => x.id === id);
    if (s) s.title = title;
  }

  function onExited(id: string, _status: number | null): void {
    // Exited sessions vanish, tmux-style — the daemon has already reaped
    // them; drop the row without waiting for the next poll.
    applySessions(sessions.filter((s) => s.id !== id));
  }

  function onSocketError(id: string, message: string): void {
    // Session-socket protocol errors never enter the scrollback. A token
    // mismatch raises the blocking re-auth overlay; anything else is logged
    // and the next sessions snapshot reconciles the rail.
    if (message === "unauthorized") {
      notifyUnauthorized();
    } else {
      console.warn(`session ${id}: ${message}`);
    }
  }

  /** Open/focus a session in the layout (rail click, strip chip, mod+N). */
  function openSess(id: string): void {
    layout = openSession(layout, id);
    pool.focusTerminal(id);
  }

  /** Open/focus a file preview tab (FILES tree click). */
  function openFilePath(path: string): void {
    layout = openFile(layout, path);
    // The pane now shows a file: pull DOM focus off any terminal so plain
    // keys stop reaching a PTY that is no longer visible.
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** Open/focus the settings surface (gear button, ⌘,). Needs a workspace —
   *  the settings tab lives in the layout like any other surface. */
  function openSettingsSurface(): void {
    if (activeWsId === null || !layoutReady) return;
    layout = openSettings(layout);
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  function focusDirection(dir: FocusDir): void {
    layout = moveFocus(layout, dir);
    const sid = focusedSessionOf(layout);
    if (sid !== null) pool.focusTerminal(sid);
  }

  function cycle(delta: number): void {
    layout = cycleTab(layout, delta);
    const sid = focusedSessionOf(layout);
    if (sid !== null) pool.focusTerminal(sid);
  }

  function split(dir: SplitDir): void {
    splitAt(layout.focusedPaneId, dir);
  }

  /** Split `paneId`; focus lands INSIDE the new pane on a real focusable
   *  target (the pane root), so chords and Escape keep working. */
  function splitAt(paneId: string, dir: SplitDir): void {
    layout = splitPane(layout, paneId, dir);
    const newPaneId = layout.focusedPaneId;
    // Pull DOM focus off the old terminal so typing doesn't land in a pane
    // that no longer looks focused; then land it on the new pane, which is
    // a real focusable target (tabindex=-1) so chords/Escape keep working.
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
    void tick().then(() => paneRootEl(newPaneId)?.focus());
  }

  /** Close the focused view: detach the pane's active tab, or collapse the
   *  pane entirely when it is empty (the last pane just stays). */
  function closeView(paneId: string): void {
    const p = findPane(layout.root, paneId);
    if (p === null) return;
    layout = p.tabs.length > 0 ? detachTab(layout, paneId, p.active) : closePane(layout, paneId);
    const sid = focusedSessionOf(layout);
    const focusedId = layout.focusedPaneId;
    // After the flush: closing a pane restructures the tree, which can
    // re-parent (and blur) the surviving terminal — focus once it settles.
    void tick().then(() => {
      if (sid !== null) pool.focusTerminal(sid);
      else paneRootEl(focusedId)?.focus();
    });
  }

  /**
   * Step (or reset, delta 0) a pane's terminal font size — the chords and
   * the pane-bar A−/A+ controls both land here; persisted with the layout.
   */
  function adjustFont(paneId: string, delta: 1 | -1 | 0): void {
    const p = findPane(layout.root, paneId);
    if (p === null) return;
    if (delta === 0) {
      layout = setPaneFont(layout, paneId, undefined);
      return;
    }
    layout = setPaneFont(layout, paneId, (p.fontSize ?? pool.baseFontSize()) + delta);
  }

  /** The focused pane's fitted size, for spawning sessions at the right grid. */
  function spawnSize(): { cols: number; rows: number } | null {
    const pane = findPane(layout.root, layout.focusedPaneId);
    if (pane === null) return null;
    const active = pane.tabs[pane.active];
    if (active !== undefined && active.surface === "terminal") {
      const exact = pool.getSize(active.sessionId);
      if (exact !== null) return exact;
    }
    const el = paneContentEl(pane.id);
    return el !== null ? pool.estimateSize(el) : null;
  }

  /** Spawn + surface a session; the shared tail of every create path. */
  async function spawnSession(
    kind: "shell" | "agent",
    spawn: AgentSpawn = {},
  ): Promise<Session | null> {
    if (activeWsId === null) {
      openPicker();
      return null;
    }
    createError = null;
    try {
      const s = await createSession(activeWsId, kind, null, spawnSize(), spawn);
      recentlyCreated.set(s.id, Date.now());
      // A racing events snapshot may already have delivered the session.
      if (!sessions.some((x) => x.id === s.id)) sessions.push(s);
      // The new session opens as the active tab in the focused pane,
      // focused, immediately — never an invisible rail-only row.
      openSess(s.id);
      return s;
    } catch (e) {
      // Inline error (409 when the agent binary is missing,
      // "unauthorized"/network noise otherwise).
      const what = kind === "agent" ? (spawn.agent ?? "agent") : "terminal";
      createError = e instanceof ApiError ? e.message : `failed to start ${what}`;
      return null;
    }
  }

  function newShell(): void {
    void spawnSession("shell");
  }

  /** The split button's main surface / Cmd+Shift+E: the persisted default
   *  agent, instantly — no popover in the way. */
  function spawnDefaultAgent(): void {
    void spawnSession("agent", { agent: agentDefault.agent });
  }

  /** What the main surface / new-agent shortcut does: normally spawn the
   *  default instantly, but when it isn't installed, don't launch a doomed
   *  pane — install it in place (managed) or, if there's no managed install,
   *  open the launcher to pick an agent that has one. */
  function newAgentPrimary(): void {
    launcherOpen = false;
    if (defaultMissing) {
      if (defaultInstallable && defaultAgentInfo !== null) launcherInstall(defaultAgentInfo);
      else openLauncher();
      return;
    }
    spawnDefaultAgent();
  }

  /** Re-probe the host's agent catalog. `force` bypasses the daemon's
   *  detection cache (after an install, or when the launcher opens). Failures
   *  keep the last-known catalog rather than blanking the button. */
  async function refreshAgents(force = false): Promise<void> {
    try {
      agents = await listAgents(force);
    } catch {
      // keep the last catalog; the next trigger retries
    }
  }

  // --- the agent launcher popover ---

  /** When the popover opened, so a chevron click landing right after a
   *  hover-open confirms it instead of flash-closing it. */
  let launcherOpenedAt = 0;

  function openLauncher(): void {
    if (launcherOpen || activeWsId === null || newSplitEl === null) return;
    launcherAnchor = newSplitEl.getBoundingClientRect();
    launcherOpenedAt = Date.now();
    launcherOpen = true;
  }

  function closeLauncher(): void {
    launcherOpen = false;
    const sid = focusedSessionOf(layout);
    if (sid !== null) pool.focusTerminal(sid);
  }

  /** Hover intent (~150ms) on the CHEVRON opens the launcher — the main
   *  surface never does; it is a pure instant spawn (field feedback: a
   *  popover chasing every hover of the button read as intrusive). */
  function armLauncherHover(): void {
    if (launcherOpen || activeWsId === null) return;
    disarmLauncherHover();
    launcherHoverTimer = setTimeout(() => {
      launcherHoverTimer = null;
      openLauncher();
    }, 150);
  }

  function disarmLauncherHover(): void {
    if (launcherHoverTimer !== null) {
      clearTimeout(launcherHoverTimer);
      launcherHoverTimer = null;
    }
  }

  /** Every launcher selection becomes the new default agent and spawns. */
  function launcherPick(pick: LaunchPick): void {
    launcherOpen = false;
    agentDefault = { agent: pick.agent };
    setAgentDefault(agentDefault);
    void spawnSession("agent", pick);
  }

  /** Install/update flow (managed runtimes): the daemon builds its own
   *  curated command — official artifacts only, into ~/.chimaera/agents —
   *  and runs it as an ordinary shell session here. The chip's tooltip
   *  said exactly what runs; this one explicit click executes it, and the
   *  session opens like any other so the output streams in a visible pane. */
  function launcherInstall(a: AgentInfo): void {
    launcherOpen = false;
    const ws = activeWsId;
    if (ws === null) {
      openPicker();
      return;
    }
    createError = null;
    void installAgent(a.id, ws)
      .then(async (sessionId) => {
        recentlyCreated.set(sessionId, Date.now());
        // Watch this install session: when it exits, re-probe the catalog so
        // the split button flips from "install" to a plain spawn.
        pendingInstalls.add(sessionId);
        // The daemon spawned the session; a racing events snapshot may not
        // carry it yet, so fetch the roster before surfacing it exactly
        // like any new session (active tab in the focused pane).
        if (!sessions.some((s) => s.id === sessionId)) {
          try {
            applySessions(await listSessions());
          } catch {
            // roster fetch hiccup; the next events snapshot reconciles
          }
        }
        openSess(sessionId);
      })
      .catch((e) => {
        // Inline error, same surface as any create failure (404 unknown
        // agent, 409 an install for it is already running).
        createError =
          e instanceof ApiError ? e.message : `failed to start the ${a.name} install`;
      });
  }

  // --- the rail's Recents section ---

  /** Monotone fetch counter: a slow early /recents response must not
   *  clobber a newer one (the follow-up fetch exists precisely to carry
   *  fresher data than its predecessor). */
  let recentsSeq = 0;

  /** Reload the workspace's ended agent conversations. `follow` schedules a
   *  second fetch: a killed agent retires into recents on the daemon's ~2s
   *  watch tick, after the session already vanished from the snapshot. */
  function refreshRecents(follow = false): void {
    const ws = activeWsId;
    if (ws === null) {
      recents = [];
      return;
    }
    const seq = ++recentsSeq;
    listRecents(ws)
      .then((r) => {
        if (activeWsId === ws && seq === recentsSeq) recents = r;
      })
      .catch(() => {
        // rail stays on its last list; the next snapshot retries
      });
    if (follow) {
      if (recentsTimer !== null) clearTimeout(recentsTimer);
      recentsTimer = setTimeout(() => {
        recentsTimer = null;
        refreshRecents();
      }, 2600);
    }
  }

  $effect(() => {
    void activeWsId;
    recentsExpanded = false;
    refreshRecents();
  });

  /** A Recents row: resume when the CLI supports it (claude), else an
   *  honest fresh start of the same agent (the tooltip says which). */
  function openRecent(r: RecentConvo): void {
    void spawnSession(
      "agent",
      r.resume !== null ? { agent: r.kind, resume: r.resume } : { agent: r.kind },
    );
  }

  function recentTooltip(r: RecentConvo): string {
    return r.resume !== null
      ? `resume “${r.title}”`
      : `${r.kind} can't resume a finished conversation yet — starts a fresh one`;
  }

  /** Kill the session's process on the daemon and drop it locally. */
  async function killSession(id: string): Promise<void> {
    confirmKillId = null;
    try {
      await deleteSession(id);
    } catch {
      // already gone or unreachable; fall through and drop it locally
    }
    applySessions(sessions.filter((s) => s.id !== id));
  }

  /** Inline rename (double-click / F2 on a rail row): chimaera owns the
   *  pin — it works for every session kind, not just claude's /rename. */
  function startRename(s: Session): void {
    confirmKillId = null;
    renamingId = s.id;
    renameDraft = displayNames.get(s.id) ?? displayName(s);
  }

  function commitRename(): void {
    const id = renamingId;
    if (id === null) return;
    renamingId = null;
    const name = renameDraft.trim();
    if (name === "") return; // empty = cancel, never un-pin by accident
    renameSession(id, name).catch(() => {
      // next sessions snapshot restores the truth
    });
  }

  /** The × on a rail row: live sessions get an inline confirm first. */
  function requestKill(s: Session): void {
    if (s.alive) {
      confirmKillId = s.id;
    } else {
      void killSession(s.id);
    }
  }

  async function retryAuth(): Promise<void> {
    if (authRetrying) return;
    authRetrying = true;
    authRetryMsg = null;
    refreshTokenFromHash();
    try {
      await fetchHealth();
      // Token works again: a clean reload re-auths every socket and
      // restores the layout from the daemon.
      location.reload();
    } catch {
      authRetryMsg = "still unauthorized — paste a fresh URL from `chimaera connect`, then retry";
    } finally {
      authRetrying = false;
    }
  }

  // --- layout controller (invoked by the pane tree) -------------------------

  const ctrl: LayoutCtrl = {
    focusPane(paneId) {
      layout = focusPane(layout, paneId);
    },
    activateTab(paneId, index) {
      layout = activateTab(layout, paneId, index);
      const sid = focusedSessionOf(layout);
      if (sid !== null) pool.focusTerminal(sid);
    },
    closeTab(paneId, index) {
      // Detaches the view only — the session stays alive in the rail.
      layout = detachTab(layout, paneId, index);
    },
    setRatio(splitId, ratio) {
      layout = setRatio(layout, splitId, ratio);
    },
    dragTab(e, paneId, index, tab) {
      beginDrag(e, tab, () => ctrl.activateTab(paneId, index));
    },
    dragSurface(e, tab, onClick) {
      // The link icon: a link-intent drag — dropping anywhere but an agent
      // (rail row or pane band) is a no-op, never a tab move.
      beginDrag(e, tab, onClick, true);
    },
    dividerDrag(active) {
      pool.setDragging(active);
    },
    splitPaneAt(paneId, dir) {
      splitAt(paneId, dir);
    },
    zoomPane(paneId) {
      layout = toggleZoom(focusPane(layout, paneId));
      const sid = focusedSessionOf(layout);
      // Zoom swaps the pane between tree and fullscreen rendering, which
      // re-parents the terminal; restore its focus after the flush.
      if (sid !== null) void tick().then(() => pool.focusTerminal(sid));
    },
    closeView(paneId) {
      closeView(paneId);
    },
    openFileFrom(paneId, path, newSplit) {
      openFileFromPane(paneId, path, newSplit);
    },
    navigateFinder(id, path) {
      layout = setFinderPath(layout, id, path);
    },
    openDiffFrom(paneId, path, mode, newSplit) {
      openDiffFromPane(paneId, path, mode, newSplit);
    },
    adjustFont(paneId, delta) {
      adjustFont(paneId, delta);
    },
  };

  /**
   * Panes whose input band should read "link to this agent" while dragging
   * `tab`: panes showing a live agent session, when the payload is a shell
   * terminal (files and agents themselves never link).
   */
  function linkTargetsFor(tab: Tab): ReadonlyMap<string, string> | undefined {
    if (tab.surface !== "terminal") return undefined;
    if (sessionsById.get(tab.sessionId)?.kind !== "shell") return undefined;
    const targets = new Map<string, string>();
    const walk = (n: typeof layout.root): void => {
      if (n.type === "pane") {
        const active = n.tabs[n.active];
        if (active !== undefined && active.surface === "terminal") {
          const s = sessionsById.get(active.sessionId);
          if (s !== undefined && s.kind === "agent" && s.alive) targets.set(n.id, s.id);
        }
        return;
      }
      walk(n.a);
      walk(n.b);
    };
    walk(layout.root);
    return targets.size > 0 ? targets : undefined;
  }

  /**
   * The live agent sessions in `tab`'s workspace whose RAIL ROWS are link
   * targets while dragging that shell terminal (the always-present target —
   * the agent needn't be open in a pane). Undefined for non-shell payloads.
   */
  function linkSessionsFor(tab: Tab): ReadonlySet<string> | undefined {
    if (tab.surface !== "terminal") return undefined;
    const term = sessionsById.get(tab.sessionId);
    if (term === undefined || term.kind !== "shell") return undefined;
    const out = new Set<string>();
    for (const s of sessions) {
      if (s.kind === "agent" && s.alive && s.workspace_id === term.workspace_id) out.add(s.id);
    }
    return out.size > 0 ? out : undefined;
  }

  /**
   * Shared drag start for rail rows and pane tabs (any surface). `linkIntent`
   * (set when the drag starts from a pane's link icon) restricts a shell
   * terminal to link-only drops — anywhere but an agent is a no-op, never a
   * tab move.
   */
  function beginDrag(e: PointerEvent, tab: Tab, onClick: () => void, linkIntent = false): void {
    const label =
      tab.surface === "terminal"
        ? (displayNames.get(tab.sessionId) ??
          sessionsById.get(tab.sessionId)?.name ??
          tab.sessionId.slice(0, 8))
        : tab.surface === "file"
          ? (fileTitles.get(tab.path) ?? basename(tab.path))
          : tab.surface === "finder"
            ? (basename(tab.path) || "Finder")
            : tab.surface === "diff"
              ? `${basename(tab.path)} (diff)`
              : tab.surface === "git"
                ? "Source Control"
                : "Settings";
    // Arm the bottom bands for this drag: reference targets for file drags,
    // link targets for shell-terminal drags. Drives the partitioned zone
    // previews (the band region is reserved, never flashed over).
    const armed = new Set<string>();
    const linkTargets = linkTargetsFor(tab);
    const linkSessions = linkSessionsFor(tab);
    if (linkTargets !== undefined) for (const id of linkTargets.keys()) armed.add(id);
    if (tab.surface === "file") {
      for (const p of panesOf(layout.root)) {
        const a = p.tabs[p.active];
        if (
          a !== undefined &&
          a.surface === "terminal" &&
          (sessionsById.get(a.sessionId)?.alive ?? false)
        ) {
          armed.add(p.id);
        }
      }
    }
    bandPanes = armed;
    startDrag(
      e,
      { tab, label },
      {
        onSpot: (s) => (dropSpot = s),
        onDrop: (spot) => {
          if (spot.kind === "ref") {
            // Drag-to-reference: type into the session, never open a tab.
            if (tab.surface === "file") referenceFileDrop(spot.paneId, tab.path);
            return;
          }
          if (spot.kind === "link") {
            // Plain tab drag onto an agent pane's band.
            if (tab.surface === "terminal") linkByDrop(tab.sessionId, spot.paneId);
            return;
          }
          if (spot.kind === "linkpane" || spot.kind === "linktab" || spot.kind === "linkrow") {
            // Link-intent drop on an agent (its view, tab, or rail row): link,
            // then surface the agent.
            if (tab.surface === "terminal") linkBySession(tab.sessionId, spot.sessionId);
            return;
          }
          // A link-intent drag (from the link icon) only ever links — a drop
          // anywhere but an agent is a no-op, never a surprise tab move.
          if (linkIntent) return;
          layout =
            spot.kind === "tab"
              ? moveTabToIndex(layout, tab, spot.paneId, spot.index)
              : spot.kind === "edge"
                ? dropTabAtRootEdge(layout, tab, spot.edge)
                : dropTab(layout, tab, spot.paneId, spot.zone);
          if (tab.surface === "terminal") {
            pool.focusTerminal(tab.sessionId);
          } else if (document.activeElement instanceof HTMLElement) {
            // A file surface landed: pull DOM focus off any terminal so
            // plain keys stop reaching a PTY that is no longer visible.
            document.activeElement.blur();
          }
        },
        onClick,
        onEnd: () => {
          dropSpot = null;
          bandPanes = new Set();
        },
        // The "@ reference" band exists over panes showing a LIVE session
        // (dnd only consults this for file drags).
        acceptsRef: (paneId) => {
          const p = findPane(layout.root, paneId);
          if (p === null) return false;
          const a = p.tabs[p.active];
          return (
            a !== undefined &&
            a.surface === "terminal" &&
            (sessionsById.get(a.sessionId)?.alive ?? false)
          );
        },
      },
      { linkTargets, linkSessions, linkIntent },
    );
  }

  // --- linked terminals ------------------------------------------------------

  /**
   * Create/move a link with an optimistic local update; the next /ws/events
   * snapshot is authoritative either way (on failure we resync explicitly).
   */
  async function doLink(terminalId: string, agentId: string): Promise<void> {
    links = [
      ...links.filter((l) => l.terminal_id !== terminalId),
      { terminal_id: terminalId, agent_id: agentId },
    ];
    try {
      await putLink(terminalId, agentId);
    } catch {
      links = await listLinks().catch(() => links);
    }
  }

  /** Reveal a session's view, splitting beside `besidePaneId` if not open. */
  function revealSession(sessionId: string, besidePaneId: string): void {
    if (sessionPaneId(layout, sessionId) === null) {
      layout = splitPane(layout, besidePaneId, "row");
    }
    layout = openSession(layout, sessionId);
  }

  /**
   * The drop on an agent pane's link band: link the terminal, type its
   * @term: reference into the composer (never submits), reveal the terminal
   * beside the agent, and hand focus to the agent so Enter sends the prompt.
   */
  function linkByDrop(terminalId: string, agentPaneId: string): void {
    const pane = findPane(layout.root, agentPaneId);
    const agentTab = pane?.tabs[pane.active];
    if (agentTab === undefined || agentTab.surface !== "terminal") return;
    const agentId = agentTab.sessionId;
    void doLink(terminalId, agentId);
    const name = displayNames.get(terminalId) ?? terminalId;
    // The context bridge's typing path: pooled socket when warm, one-shot
    // socket otherwise — the reference lands even in a cold agent pane.
    typeIntoSession(agentId, `${termReference(name)} `);
    revealSession(terminalId, agentPaneId);
    pool.focusTerminal(agentId);
  }

  /**
   * The drop on an agent's RAIL ROW: link the terminal, surface the agent
   * (splitting beside the terminal when the agent isn't open anywhere), type
   * its @term: reference into the composer, and focus the agent. Works even
   * when the agent has no pane — the rail row is the always-present target.
   */
  function linkBySession(terminalId: string, agentId: string): void {
    const agent = sessionsById.get(agentId);
    if (agent === undefined || agent.kind !== "agent" || !agent.alive) return;
    void doLink(terminalId, agentId);
    if (sessionPaneId(layout, agentId) === null) {
      const beside = sessionPaneId(layout, terminalId) ?? layout.focusedPaneId;
      layout = splitPane(layout, beside, "row");
      layout = openSession(layout, agentId);
    }
    const name = displayNames.get(terminalId) ?? terminalId;
    typeIntoSession(agentId, `${termReference(name)} `);
    pool.focusTerminal(agentId);
  }

  /** Link mutations + reveal for the pane top bars (chips and the menu). */
  const linkCtrl: LinkCtrl = {
    reveal(sessionId, besidePaneId) {
      revealSession(sessionId, besidePaneId);
      pool.focusTerminal(sessionId);
    },
    link(terminalId, agentId) {
      void doLink(terminalId, agentId);
    },
    unlink(terminalId) {
      links = links.filter((l) => l.terminal_id !== terminalId);
      deleteLink(terminalId).catch(async () => {
        links = await listLinks().catch(() => links);
      });
    },
  };

  function onRailRowDown(e: PointerEvent, sessionId: string): void {
    beginDrag(e, { surface: "terminal", sessionId }, () => openSess(sessionId));
  }

  /** FILES tree entries drag exactly like rail rows (surface parity). */
  function onTreeEntryDown(e: PointerEvent, path: string): void {
    beginDrag(e, { surface: "file", path }, () => openFilePath(path));
  }

  /** Svelte action: focus the node as soon as it mounts (confirm buttons). */
  function focusOnMount(node: HTMLElement): void {
    node.focus();
  }

  /** Svelte action: register an agent rail row as a link-drop target, so a
   *  shell-terminal drag (from the link icon or a tab) can drop on it to link.
   *  No-op for non-agent rows. */
  function linkRow(node: HTMLElement, s: Session): { destroy(): void } | void {
    if (s.kind !== "agent") return;
    registerLinkRow(s.id, node);
    return {
      destroy() {
        unregisterLinkRow(s.id, node);
      },
    };
  }

  /**
   * FILES section resize: a quiet divider above the section header; drag
   * moves the boundary (fraction of the rail, clamped), Escape restores.
   */
  function onFilesDividerDown(e: PointerEvent): void {
    if (e.button !== 0 || railEl === null || daemonEl === null) return;
    e.preventDefault();
    const divider = e.currentTarget as HTMLElement;
    const rail = railEl;
    const daemon = daemonEl;
    const pointerId = e.pointerId;
    const startFrac = filesFrac;
    let raf = 0;
    let lastY = e.clientY;
    let done = false;

    try {
      divider.setPointerCapture(pointerId);
    } catch {
      // capture unavailable; window-level listeners still track the drag
    }
    filesDividerActive = true;

    const apply = () => {
      raf = 0;
      const railH = rail.getBoundingClientRect().height;
      const daemonTop = daemon.getBoundingClientRect().top;
      if (railH <= 0) return;
      const h = daemonTop - lastY;
      filesFrac = Math.min(Math.max(h / railH, FILES_FRAC_MIN), FILES_FRAC_MAX);
    };

    const onMove = (ev: PointerEvent) => {
      if (ev.pointerId !== pointerId) return;
      lastY = ev.clientY;
      if (raf === 0) raf = requestAnimationFrame(apply);
    };

    const finish = (cancel: boolean) => {
      if (done) return;
      done = true;
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
      window.removeEventListener("keydown", onKey, true);
      if (raf !== 0) cancelAnimationFrame(raf);
      if (cancel) filesFrac = startFrac;
      filesDividerActive = false;
    };

    const onUp = (ev: PointerEvent) => {
      if (ev.pointerId === pointerId) finish(false);
    };
    const onCancel = (ev: PointerEvent) => {
      if (ev.pointerId === pointerId) finish(true);
    };
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        ev.preventDefault();
        ev.stopPropagation();
        finish(true);
      }
    };

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
    window.addEventListener("keydown", onKey, true);
  }

  /**
   * Sidebar width resize: a quiet vertical handle on the rail's right edge.
   * Drag moves the boundary (clamped to [RAIL_MIN, RAIL_MAX]); Escape restores.
   * The stage grows/shrinks with the rail, so terminals are told to hold their
   * fits until the drag ends (setDragging) to avoid per-frame reflow jank.
   */
  function onRailResizeDown(e: PointerEvent): void {
    if (e.button !== 0 || railEl === null) return;
    e.preventDefault();
    const handle = e.currentTarget as HTMLElement;
    const rail = railEl;
    const pointerId = e.pointerId;
    const startX = e.clientX;
    const startWidth = rail.getBoundingClientRect().width;
    const startRail = railWidth;
    let raf = 0;
    let lastX = e.clientX;
    let done = false;

    try {
      handle.setPointerCapture(pointerId);
    } catch {
      // capture unavailable; window-level listeners still track the drag
    }
    railDividerActive = true;
    pool.setDragging(true);

    const apply = () => {
      raf = 0;
      railWidth = Math.min(Math.max(startWidth + (lastX - startX), RAIL_MIN), RAIL_MAX);
    };

    const onMove = (ev: PointerEvent) => {
      if (ev.pointerId !== pointerId) return;
      lastX = ev.clientX;
      if (raf === 0) raf = requestAnimationFrame(apply);
    };

    const finish = (cancel: boolean) => {
      if (done) return;
      done = true;
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
      window.removeEventListener("keydown", onKey, true);
      if (raf !== 0) cancelAnimationFrame(raf);
      if (cancel) railWidth = startRail;
      railDividerActive = false;
      // Flush the fits deferred during the drag now that the width is settled.
      pool.setDragging(false);
    };

    const onUp = (ev: PointerEvent) => {
      if (ev.pointerId === pointerId) finish(false);
    };
    const onCancel = (ev: PointerEvent) => {
      if (ev.pointerId === pointerId) finish(true);
    };
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") {
        ev.preventDefault();
        ev.stopPropagation();
        finish(true);
      }
    };

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
    window.addEventListener("keydown", onKey, true);
  }

  /** Double-click the width handle to snap back to the default sidebar width. */
  function resetRailWidth(): void {
    railWidth = RAIL_DEFAULT;
  }
</script>

<div class="shell">
  {#if activeWsId === null}
    <!-- Home: a real launcher, not an empty IDE. The rail and stage only
         exist once a workspace scopes this window. -->
    <HomeScreen
      {workspaces}
      {sessions}
      hostLabel={getHostLabel()}
      {health}
      connected={eventsUp}
      onOpen={activateWorkspace}
      onRemove={removeWorkspace}
      onOpenFolder={openPicker}
    />
  {:else}
  <div class="body">
    <aside
      class="rail"
      class:collapsed={layout.focusMode}
      class:resizing={railDividerActive}
      style:width={layout.focusMode ? undefined : `${railWidth}px`}
      bind:this={railEl}
    >
      <div class="workspace">
        <button
          class="ws-btn"
          class:placeholder={workspace === null && activeWsId === null}
          title={workspace?.root}
          onclick={openPicker}
        >
          <span class="ws-label">
            {workspace ? workspace.name : activeWsId !== null ? "—" : "Open a folder"}
          </span>
          <svg class="ws-chev" viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
            <path
              d="M4 6l4 4 4-4"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
        {#if needsYou > 0}
          <span class="needs" title="{needsYou} need{needsYou === 1 ? 's' : ''} you">
            {needsYou}
          </span>
        {/if}
        <button
          class="rail-collapse"
          title="hide sidebar ({KEYS.focusMode})"
          aria-label="hide sidebar"
          onclick={() => (layout = { ...layout, focusMode: true })}
        >
          <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
            <rect
              x="1.75"
              y="2.75"
              width="12.5"
              height="10.5"
              rx="2"
              fill="none"
              stroke="currentColor"
              stroke-width="1.3"
            />
            <line x1="6.5" y1="2.75" x2="6.5" y2="13.25" stroke="currentColor" stroke-width="1.3" />
            <path
              d="M11.4 6.2 9.4 8l2 1.8"
              fill="none"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
      </div>

      <nav class="sessions">
        {#snippet sessionRow(s: Session)}
          {#if confirmKillId === s.id}
            <div
              class="row confirm"
              role="alertdialog"
              tabindex="-1"
              aria-label="kill session?"
              onkeydown={(e) => {
                if (e.key === "Escape") {
                  e.preventDefault();
                  e.stopPropagation();
                  confirmKillId = null;
                }
              }}
            >
              <SessionGlyph kind={s.kind} agentKind={s.agent_kind} state={dotState(s)} size={11} />
              <span class="confirm-label">kill session?</span>
              <button class="confirm-kill" use:focusOnMount onclick={() => void killSession(s.id)}>
                kill
              </button>
              <button class="confirm-cancel" onclick={() => (confirmKillId = null)}>cancel</button>
            </div>
          {:else}
            <div
              class="row"
              class:active={s.id === focusedSessionId}
              class:link-target={dropSpot?.kind === "linkrow" && dropSpot.sessionId === s.id}
              style:--hue={s.kind === "agent" ? agentHue(s.id) : null}
              use:linkRow={s}
              role="button"
              tabindex="0"
              onpointerdowncapture={(e) => {
                // Capture-phase (directly attached); the close button and
                // the rename input stay plain interactive targets.
                if (
                  e.target instanceof Element &&
                  e.target.closest(".close, .rename-input")
                )
                  return;
                onRailRowDown(e, s.id);
              }}
              onkeydown={(e) => {
                if (renamingId === s.id) return; // the input owns keys
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  openSess(s.id);
                } else if (e.key === "F2") {
                  e.preventDefault();
                  startRename(s);
                }
              }}
            >
              <!-- Session-type glyph carrying the state color — the same
                   mark as the pane tab (surface parity, rail included). -->
              <SessionGlyph
                kind={s.kind}
                agentKind={s.agent_kind}
                state={dotState(s)}
                size={11}
                title={dotTitle(s)}
              />
              {#if renamingId === s.id}
                <!-- svelte-ignore a11y_autofocus -->
                <input
                  class="rename-input"
                  type="text"
                  autofocus
                  bind:value={renameDraft}
                  onkeydown={(e) => {
                    e.stopPropagation();
                    if (e.key === "Enter") {
                      e.preventDefault();
                      commitRename();
                    } else if (e.key === "Escape") {
                      e.preventDefault();
                      renamingId = null;
                    }
                  }}
                  onblur={commitRename}
                />
              {:else}
                <!-- svelte-ignore a11y_no_static_element_interactions --
                     dblclick is the mouse path; keyboard rename lives on
                     the row itself (F2), which carries the button role. -->
                <span
                  class="labels"
                  title="double-click to rename (F2)"
                  ondblclick={() => startRename(s)}
                >
                  <span class="name">{displayNames.get(s.id) ?? displayName(s)}</span>
                  <!-- Second line only when it adds something over the name.
                       Shells never do: the name already resolves to the title
                       (program-set) or the cwd (the shell's "user@host:dir"
                       prompt title is dropped server-side). Agents show their
                       own PTY title as context when it differs from the name. -->
                  {#if s.kind === "agent" && s.title && s.title !== displayName(s) && s.title !== s.agent_title}
                    <span class="title">{s.title}</span>
                  {/if}
                </span>
              {/if}
              {#if hintsActive() && renamingId !== s.id && chordDigits.has(s.id)}
                <!-- Which-key discovery: the ⌘1–9 digit for this row, faded in
                     while the modifier is held. Pure teaching chrome. -->
                <span class="kbd-badge" aria-hidden="true">{chordDigits.get(s.id)}</span>
              {/if}
              <button
                class="close"
                aria-label="kill session"
                title="kill session"
                onclick={(e) => {
                  e.stopPropagation();
                  requestKill(s);
                }}>&times;</button
              >
            </div>
          {/if}
        {/snippet}

        <!-- Terminals first (there are few), agents below (there are many);
             this order is also the mod+1–9 order and the strip order. -->
        <div class="rail-sec">terminals</div>
        {#each shellSessions as s (s.id)}
          {@render sessionRow(s)}
        {/each}
        <button
          class="row new"
          title="open a terminal ({KEYS.newTerminal})"
          onclick={newShell}>+ terminal</button
        >

        <div class="rail-sec agents-sec">agents</div>
        {#each agentSessions as s (s.id)}
          {@render sessionRow(s)}
        {/each}
        <!-- Split button: the main surface spawns the persisted default
             agent instantly — always. Only the CHEVRON opens the launcher
             popover (hover ~150ms or click). -->
        <div class="new-split" role="group" aria-label="new agent" bind:this={newSplitEl}>
          <button
            class="row new primary main"
            class:want-install={defaultMissing}
            title={defaultMissing
              ? defaultInstallable
                ? `${agentDefault.agent} isn’t installed — download the official build into ~/.chimaera/agents, in a terminal you can watch`
                : `${agentDefault.agent} isn’t installed — choose an agent to set up`
              : `start ${agentDefault.agent} (${KEYS.newAgent})`}
            onclick={newAgentPrimary}
          >
            <!-- When the default isn't installed the surface installs it
                 (managed) rather than spawning a pane that would just print
                 the shim's error — so the label becomes the action and the
                 agent name takes the accent. -->
            <span class="new-label"
              >{defaultMissing ? (defaultInstallable ? "install" : "set up") : "+ new agent"}</span
            >
            <span class="new-default" class:accent={defaultMissing}>{agentDefault.agent}</span>
          </button>
          <button
            class="new-chev"
            aria-haspopup="menu"
            aria-expanded={launcherOpen}
            aria-label="choose an agent"
            title="choose an agent"
            onpointerenter={armLauncherHover}
            onpointerleave={disarmLauncherHover}
            onclick={() => {
              if (launcherOpen && Date.now() - launcherOpenedAt > 400) closeLauncher();
              else openLauncher();
            }}
          >
            <svg viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
              <path
                d="M4 6l4 4 4-4"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
          </button>
        </div>
        {#if createError}
          <div class="create-error">{createError}</div>
        {/if}

        <!-- Recents: ended agent conversations, any agent type, newest
             first — the daemon remembers them across restarts. Click
             resumes (claude) or honestly starts fresh (the tooltip says). -->
        {#if recents.length > 0}
          <div class="recents">
            <div class="recents-head">recent</div>
            <div class="recents-list" class:expanded={recentsExpanded}>
              {#each recentsExpanded ? recents : recents.slice(0, 3) as r (r.resume ?? `${r.kind}:${r.title}`)}
                <button class="recent-row" title={recentTooltip(r)} onclick={() => openRecent(r)}>
                  <SessionGlyph kind="agent" agentKind={r.kind} size={11} />
                  <span class="recent-title">{r.title}</span>
                  <span class="recent-age">{relativeAge(r.lastActive)}</span>
                </button>
              {/each}
            </div>
            {#if recents.length > 3}
              <button
                class="recents-more"
                onclick={() => (recentsExpanded = !recentsExpanded)}
              >
                {recentsExpanded ? "show less" : `all ${recents.length}`}
              </button>
            {/if}
          </div>
        {/if}
      </nav>

      {#if workspace !== null}
        {#if filesOpen}
          <div
            class="files-divider"
            class:active={filesDividerActive}
            role="separator"
            aria-orientation="horizontal"
            aria-label="resize files section"
            onpointerdown={onFilesDividerDown}
          ></div>
        {/if}
        <section class="files" class:open={filesOpen} style:flex-basis={filesOpen ? `${filesFrac * 100}%` : "auto"}>
          <div class="files-head">
            <button
              class="files-header"
              aria-expanded={filesOpen}
              onclick={() => (filesOpen = !filesOpen)}
            >
              <svg class="files-chev" class:open={filesOpen} viewBox="0 0 16 16" width="9" height="9" aria-hidden="true">
                <path
                  d="M6 4l4 4-4 4"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.6"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
              </svg>
              <span>files</span>
            </button>
            <button
              class="files-finder"
              title="new finder"
              aria-label="new finder"
              onclick={openFinderSurface}
            >
              <!-- A columns glyph: the Miller-columns file browser. -->
              <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
                <rect x="2" y="3" width="12" height="10" rx="1.6" fill="none" stroke="currentColor" stroke-width="1.3" />
                <line x1="6.7" y1="3" x2="6.7" y2="13" stroke="currentColor" stroke-width="1.3" />
                <line x1="11" y1="3" x2="11" y2="13" stroke="currentColor" stroke-width="1.3" />
              </svg>
            </button>
          </div>
          {#if filesOpen}
            <div class="files-body">
              <FileTree
                root={workspace.root}
                onOpen={openFilePath}
                onDragStart={onTreeEntryDown}
                activePath={focusedFilePath}
                reveal={treeReveal}
              />
            </div>
          {/if}
        </section>
      {/if}

      <div class="daemon" bind:this={daemonEl}>
        <span
          class="daemon-dot"
          class:ok={eventsUp}
          class:pulse={$reconnectingSockets > 0}
          role="status"
          title={eventsUp ? "connected" : "disconnected"}
          aria-label={eventsUp ? "connected" : "disconnected"}
        ></span>
        <span class="daemon-host" class:remote={isRemoteWindow} title={health?.hostname}
          >{getHostLabel()}</span
        >
        {#if $gitStatus !== null}
          <!-- Always-on orientation: what branch you're on and how dirty the
               tree is; one click opens the source-control panel. -->
          <button
            class="daemon-git"
            onclick={openGitPanel}
            title={`${$gitStatus.detached ? `detached at ${$gitStatus.head ?? "?"}` : ($gitStatus.branch ?? "unborn branch")}${$gitStatus.upstream ? ` · ${$gitStatus.upstream}` : ""} — open source control`}
          >
            <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
              <path
                d="M5 4v5.2M11 4v2a2.4 2.4 0 0 1-2.4 2.4H5"
                fill="none"
                stroke="currentColor"
                stroke-width="1.4"
                stroke-linecap="round"
              />
              <circle cx="5" cy="12" r="1.7" fill="none" stroke="currentColor" stroke-width="1.4" />
              <circle cx="5" cy="2.6" r="1.7" fill="none" stroke="currentColor" stroke-width="1.4" />
              <circle cx="11" cy="2.6" r="1.7" fill="none" stroke="currentColor" stroke-width="1.4" />
            </svg>
            <span class="dg-branch"
              >{$gitStatus.detached
                ? ($gitStatus.head ?? "detached")
                : ($gitStatus.branch ?? "unborn")}</span
            >
            {#if $gitStatus.ahead > 0}<span class="dg-ab">↑{$gitStatus.ahead}</span>{/if}
            {#if $gitStatus.behind > 0}<span class="dg-ab">↓{$gitStatus.behind}</span>{/if}
            {#if $gitStatus.counts.total > 0}
              <span class="dg-dirty" title="{$gitStatus.counts.total} changed">
                ●{$gitStatus.counts.total}
              </span>
            {/if}
          </button>
        {/if}
        {#if activeWsId !== null}
          <button
            class="daemon-settings"
            title="settings ({KEYS.settings})"
            aria-label="settings"
            onclick={openSettingsSurface}
          >
            <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
              <circle cx="8" cy="8" r="2.2" fill="none" stroke="currentColor" stroke-width="1.4" />
              <path
                d="M8 1.8v2M8 12.2v2M1.8 8h2M12.2 8h2M3.6 3.6l1.4 1.4M11 11l1.4 1.4M12.4 3.6L11 5M5 11l-1.4 1.4"
                fill="none"
                stroke="currentColor"
                stroke-width="1.4"
                stroke-linecap="round"
              />
            </svg>
          </button>
        {/if}
      </div>
    </aside>

    {#if !layout.focusMode}
      <!-- Sidebar width handle: a quiet vertical splitter on the rail's edge. -->
      <div
        class="rail-resize"
        class:active={railDividerActive}
        role="separator"
        aria-orientation="vertical"
        aria-label="resize sidebar"
        title="drag to resize · double-click to reset"
        onpointerdown={onRailResizeDown}
        ondblclick={resetRailWidth}
      ></div>
    {/if}

    <main class="stage" bind:this={stageEl}>
      {#if layoutReady}
        {#if zoomedPane !== null}
          <Pane
            node={zoomedPane}
            focusedPaneId={layout.focusedPaneId}
            zoomed
            {dropSpot}
            sessions={sessionsById}
            names={displayNames}
            fileNames={fileTitles}
            links={linksByTerminal}
            {linkCtrl}
            wsRoot={workspace?.root ?? null}
            wsId={activeWsId}
            {bandPanes}
            {ctrl}
          />
        {:else}
          <SplitTree
            node={layout.root}
            focusedPaneId={layout.focusedPaneId}
            {dropSpot}
            sessions={sessionsById}
            names={displayNames}
            fileNames={fileTitles}
            links={linksByTerminal}
            {linkCtrl}
            wsRoot={workspace?.root ?? null}
            wsId={activeWsId}
            {bandPanes}
            {ctrl}
          />
        {/if}
        {#if dropSpot?.kind === "edge"}
          <!-- Window-edge preview: the root split's new pane, full height/width. -->
          <div class="edge-drop {dropSpot.edge}"></div>
        {/if}
      {/if}
    </main>
  </div>

  {#if layout.focusMode}
    <!-- Focus-mode session strip: the rail is gone, but the window always
         says where you are. Hidden whenever the rail is visible. -->
    <footer class="strip">
      <button
        class="strip-show"
        title="show sidebar ({KEYS.focusMode})"
        aria-label="show sidebar"
        onclick={() => (layout = { ...layout, focusMode: false })}
      >
        <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
          <rect
            x="1.75"
            y="2.75"
            width="12.5"
            height="10.5"
            rx="2"
            fill="none"
            stroke="currentColor"
            stroke-width="1.3"
          />
          <line x1="6.5" y1="2.75" x2="6.5" y2="13.25" stroke="currentColor" stroke-width="1.3" />
          <path
            d="M9.4 6.2 11.4 8l-2 1.8"
            fill="none"
            stroke="currentColor"
            stroke-width="1.3"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      </button>
      <button
        class="strip-ws"
        title="show sidebar ({KEYS.focusMode})"
        onclick={() => (layout = { ...layout, focusMode: false })}
        >{workspace?.name ?? "chimaera"}</button
      >
      <div class="chips">
        {#each railSessions as s (s.id)}
          <button
            class="chip"
            class:focused={s.id === focusedSessionId}
            title={s.title ?? undefined}
            onclick={() => openSess(s.id)}
          >
            <!-- The type glyph replaces both the dot and the old "$ "
                 prefix — same mark as tabs and rail rows (parity). -->
            <SessionGlyph
              kind={s.kind}
              agentKind={s.agent_kind}
              state={dotState(s)}
              size={10}
              title={dotTitle(s)}
            />
            <span class="chip-name">{displayNames.get(s.id) ?? displayName(s)}</span>
            {#if hintsActive() && chordDigits.has(s.id)}
              <span class="chip-badge" aria-hidden="true">{chordDigits.get(s.id)}</span>
            {/if}
          </button>
        {/each}
      </div>
      {#if needsYou > 0}
        <span class="strip-needs">{needsYou} need{needsYou === 1 ? "s" : ""} you</span>
      {/if}
      <span class="strip-host" class:remote={isRemoteWindow} title={health?.hostname}
        >{getHostLabel()}</span
      >
    </footer>
  {/if}
  {/if}
</div>

{#if launcherOpen && activeWsId !== null && launcherAnchor !== null}
  <Launcher
    anchor={launcherAnchor}
    onPick={launcherPick}
    onInstall={launcherInstall}
    onClose={closeLauncher}
    onAgents={(a) => (agents = a)}
  />
{/if}

{#if pickerOpen}
  <FolderPicker recents={workspaces} onOpened={activateWorkspace} onClose={closePicker} />
{/if}

{#if quickOpenOpen && activeWsId !== null}
  <QuickOpen
    workspaceId={activeWsId}
    sessions={wsSessions}
    sessionNames={displayNames}
    onOpenFile={quickOpenFile}
    onOpenSession={quickOpenSession}
    onClose={closeQuickOpen}
  />
{/if}

{#if $unauthorized}
  <!-- Blocking re-auth overlay: the daemon rejected this window's token
       (restart or expiry). Nothing behind it is trustworthy until re-auth. -->
  <div class="auth-overlay" role="alertdialog" aria-modal="true" aria-label="reconnect">
    <div class="auth-panel">
      <div class="auth-title">disconnected — unauthorized</div>
      <p class="auth-body">
        The daemon rejected this window's token (it likely restarted). Paste a fresh URL from
        <code>chimaera connect</code> into the address bar, then retry.
      </p>
      <button class="auth-retry" use:focusOnMount disabled={authRetrying} onclick={() => void retryAuth()}>
        {authRetrying ? "retrying…" : "retry"}
      </button>
      {#if authRetryMsg}
        <div class="auth-msg">{authRetryMsg}</div>
      {/if}
    </div>
  </div>
{/if}

<!-- SSH auth prompt (password / 2FA), app-wide so a mid-session reconnect on
     the workbench can prompt just like the home screen. Self-gating. -->
<AskpassModal />

{#if showReconnect}
  <!-- A remote window's tunnel dropped: we re-run the SSH connect (auth modal
       appears above this only if ssh re-asks). A same-port heal resumes this
       window in place; a moved daemon re-homes it. -->
  <div class="reconnect-overlay" role="alertdialog" aria-modal="true" aria-label="reconnecting">
    <div class="reconnect-panel">
      <div class="reconnect-head">
        <span class="reconnect-spinner" class:spin={reconnecting} aria-hidden="true"></span>
        <span class="reconnect-title">
          {reconnectError !== null ? `can’t reach ${hostAlias}` : `reconnecting to ${hostAlias}…`}
        </span>
      </div>
      <p class="reconnect-body">
        {reconnectError ??
          "re-establishing the SSH tunnel — this window resumes where it left off."}
      </p>
      <div class="reconnect-actions">
        <button class="reconnect-dismiss" onclick={dismissReconnect}>dismiss</button>
        <button
          class="reconnect-retry"
          use:focusOnMount
          disabled={reconnecting}
          onclick={() => void attemptReconnect()}
        >
          {reconnecting ? "reconnecting…" : "retry"}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .shell {
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
  }

  .body {
    flex: 1;
    display: flex;
    min-height: 0;
  }

  .rail {
    /* Width is inline (draggable, persisted); this is only the pre-hydration
       fallback. min-width guards against a stale/hand-set value wedging it. */
    width: 230px;
    min-width: 0;
    flex: none;
    display: flex;
    flex-direction: column;
    background: var(--rail-bg);
    padding: 16px 0 12px;
    overflow: hidden;
    transition:
      width 0.12s ease,
      opacity 0.1s ease;
  }

  /* Mid-drag the width changes every frame; the ease would smear the handle. */
  .rail.resizing {
    transition: none;
  }

  /* Focus mode: the rail collapses to nothing; the strip carries context. */
  .rail.collapsed {
    width: 0;
    padding-left: 0;
    padding-right: 0;
    opacity: 0;
    visibility: hidden;
  }

  /* Sidebar width handle: a thin invisible hit-strip sitting on the seam, with
     a hairline that warms on hover/drag — the vertical sibling of the FILES
     divider. Negative right margin overlaps the stage so the target straddles
     the edge without stealing layout width. */
  .rail-resize {
    flex: none;
    width: 7px;
    margin: 0 -4px 0 0;
    z-index: 5;
    position: relative;
    cursor: col-resize;
    touch-action: none;
  }

  .rail-resize::after {
    content: "";
    position: absolute;
    inset: 0 3px;
    border-radius: 1px;
    background: transparent;
    transition: background-color 0.12s ease;
  }

  .rail-resize:hover::after {
    background: var(--edge);
  }

  .rail-resize.active::after {
    background: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }

  .workspace {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 16px 12px;
  }

  .needs {
    flex: none;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--warn);
  }

  /* Collapse control: the visible mouse path to focus mode (⌘B), pinned to the
     header's right edge — quiet muted icon, brightens on hover. */
  .rail-collapse {
    flex: none;
    margin-left: auto;
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 22px;
    padding: 0;
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .rail-collapse:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .ws-btn {
    appearance: none;
    border: none;
    background: none;
    padding: 2px 6px;
    margin: -2px -6px;
    border-radius: 5px;
    font: inherit;
    font-size: var(--text-md);
    font-weight: 600;
    letter-spacing: 0.01em;
    color: var(--fg);
    cursor: pointer;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 0.3rem;
    transition: background-color 0.12s ease;
  }

  .ws-btn:hover {
    background: var(--row-hover);
  }

  .ws-btn:hover .ws-chev {
    opacity: 1;
  }

  .ws-btn.placeholder {
    font-weight: 400;
    color: var(--muted);
  }

  .ws-btn.placeholder:hover {
    color: var(--fg);
  }

  .ws-label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .ws-chev {
    flex: none;
    color: var(--muted);
    opacity: 0.7;
    transition: opacity 0.12s ease;
  }

  .sessions {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 0 8px;
    min-height: 0;
  }

  .row {
    position: relative;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 8px;
    border-radius: 5px;
    font-size: var(--text-md);
    cursor: pointer;
    user-select: none;
    transition: background-color 0.12s ease;
  }

  .row:hover {
    background: var(--row-hover);
  }

  /* Inline kill confirmation, swapped in place of the row. */
  .row.confirm {
    cursor: default;
    background: var(--row-active);
  }

  .confirm-label {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--fg);
  }

  .confirm-kill,
  .confirm-cancel {
    appearance: none;
    border: none;
    background: none;
    padding: 2px 6px;
    border-radius: 4px;
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
    color: var(--muted);
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .confirm-kill {
    color: var(--err);
    font-weight: 500;
  }

  .confirm-kill:hover,
  .confirm-kill:focus-visible {
    background: color-mix(in srgb, var(--err) 14%, transparent);
  }

  .confirm-cancel:hover {
    color: var(--fg);
    background: var(--row-hover);
  }

  .row.active {
    background: var(--row-active);
  }

  /* A shell-terminal link drag hovers this agent row: the always-present
     "drop to link" target, lit in the agent's own hue. */
  .row.link-target {
    background: hsl(var(--hue) 55% 55% / 0.16);
    box-shadow: inset 0 0 0 1px hsl(var(--hue) 55% 55% / 0.6);
  }

  .labels {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    line-height: 1.3;
  }

  /* Inline rename: sized like .name so the row doesn't jump. */
  .rename-input {
    flex: 1;
    min-width: 0;
    padding: 1px 4px;
    margin: 0;
    border: 1px solid color-mix(in srgb, var(--accent) 55%, transparent);
    border-radius: 4px;
    background: var(--overlay-bg);
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
    outline: none;
  }

  .name,
  .title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .name {
    font-family: var(--mono);
    font-size: var(--text-sm);
  }

  .title {
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .close {
    appearance: none;
    border: none;
    background: none;
    padding: 0 0.1rem;
    font: inherit;
    font-size: var(--text-md);
    line-height: 1;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    flex: none;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }

  .row:hover .close,
  .row:focus-within .close {
    opacity: 1;
  }

  .close:hover {
    color: var(--fg);
  }

  /* Which-key digit badge: anchored at the row's right so it never nudges the
     label; yields to the close button on hover. Faded in by chordHints only
     while the modifier is held — teaching chrome, never interactive. */
  .kbd-badge {
    position: absolute;
    right: 8px;
    top: 50%;
    transform: translateY(-50%);
    pointer-events: none;
    display: flex;
    align-items: center;
    justify-content: center;
    min-width: 15px;
    height: 15px;
    padding: 0 3px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 38%, transparent);
    font-family: var(--mono);
    font-size: 10px;
    line-height: 1;
    color: var(--fg);
    animation: hintfade 0.14s ease-out;
  }

  .row:hover .kbd-badge {
    opacity: 0;
  }

  @keyframes hintfade {
    from {
      opacity: 0;
    }
  }

  .row.new {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-md);
    color: var(--muted);
    justify-content: flex-start;
    margin-top: 0.15rem;
  }

  .row.new:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .row.new.primary {
    color: var(--fg);
    font-weight: 500;
  }

  /* --- the agent launcher's split button --- */

  .new-split {
    display: flex;
    align-items: stretch;
    gap: 1px;
    margin-top: 0.15rem;
  }

  .new-split .row.new.main {
    flex: 1;
    min-width: 0;
    margin-top: 0;
    gap: 6px;
  }

  .new-label {
    flex: none;
  }

  /* What the main surface will spawn — quiet transparency, not chrome. */
  .new-default {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
  }

  /* Default agent isn't installed: the surface installs instead of spawning,
     so the agent name takes the accent — the label ("install"/"set up") and
     tooltip say what will happen. */
  .new-default.accent {
    color: var(--accent);
  }

  .new-chev {
    appearance: none;
    border: none;
    background: none;
    flex: none;
    width: 22px;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .new-chev:hover,
  .new-chev[aria-expanded="true"] {
    background: var(--row-hover);
    color: var(--fg);
  }

  .new-chev:active {
    background: var(--row-active);
  }

  .create-error {
    padding: 2px 8px 4px;
    font-size: var(--text-xs);
    line-height: 1.35;
    color: var(--err);
  }

  /* Rail section headers: terminals above, agents below. */
  .rail-sec {
    flex: none;
    padding: 4px 8px 3px;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--muted);
    user-select: none;
  }

  .rail-sec.agents-sec {
    margin-top: 0.55rem;
  }

  /* --- Recents: ended agent conversations --- */

  .recents {
    margin-top: 0.55rem;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .recents-head {
    flex: none;
    padding: 4px 8px;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--muted);
    user-select: none;
  }

  .recents-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  /* Expanded: a scrollable window, soft edge fade when it overflows. */
  .recents-list.expanded {
    max-height: 240px;
    overflow-y: auto;
    scrollbar-width: thin;
    mask-image: linear-gradient(to bottom, black calc(100% - 10px), transparent);
  }

  .recent-row {
    appearance: none;
    border: none;
    background: none;
    width: 100%;
    text-align: left;
    font: inherit;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
    border-radius: 5px;
    cursor: pointer;
    user-select: none;
    transition: background-color 0.12s ease;
  }

  .recent-row:hover {
    background: var(--row-hover);
  }

  /* History reads quieter than live rows; hover restores full presence. */
  .recent-row :global(.sglyph) {
    opacity: 0.7;
  }

  .recent-title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--muted);
    transition: color 0.12s ease;
  }

  .recent-row:hover .recent-title {
    color: var(--fg);
  }

  .recent-age {
    flex: none;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--muted);
    opacity: 0.8;
  }

  .recents-more {
    appearance: none;
    border: none;
    background: none;
    align-self: flex-start;
    margin: 1px 0 0;
    padding: 2px 8px;
    border-radius: 4px;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    transition: color 0.12s ease;
  }

  .recents-more:hover {
    color: var(--fg);
    background: var(--row-hover);
  }

  /* --- FILES section --- */

  /* Quiet resize handle between sessions and files; hairline on hover. */
  .files-divider {
    flex: none;
    height: 7px;
    position: relative;
    cursor: row-resize;
    touch-action: none;
  }

  .files-divider::after {
    content: "";
    position: absolute;
    inset: 3px 12px;
    border-radius: 1px;
    background: transparent;
    transition: background-color 0.12s ease;
  }

  .files-divider:hover::after {
    background: var(--edge);
  }

  .files-divider.active::after {
    background: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }

  .files {
    flex: none;
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
  }

  .files.open {
    flex-shrink: 0;
    flex-grow: 0;
  }

  /* Header row: the collapse toggle (left, fills the row) + the new-finder
     button (right), mirroring how .workspace pairs its button with an action. */
  .files-head {
    flex: none;
    display: flex;
    align-items: center;
  }

  .files-header {
    flex: 1;
    min-width: 0;
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 4px 16px;
    font: inherit;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--muted);
    cursor: pointer;
    user-select: none;
  }

  .files-header:hover {
    color: var(--fg);
  }

  .files-finder {
    flex: none;
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 3px;
    margin-right: 10px;
    border-radius: 5px;
    color: var(--muted);
    opacity: 0.75;
    cursor: pointer;
    transition:
      color 0.12s ease,
      opacity 0.12s ease,
      background-color 0.12s ease;
  }

  .files-finder:hover {
    color: var(--fg);
    opacity: 1;
    background: var(--row-hover);
  }

  .files-chev {
    flex: none;
    opacity: 0.7;
    transition: transform 0.1s ease;
  }

  .files-chev.open {
    transform: rotate(90deg);
  }

  .files-body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }

  .daemon {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 16px 0;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .daemon-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--muted);
    opacity: 0.55;
    transition: background-color 0.15s ease;
  }

  .daemon-dot.ok {
    background: var(--accent);
    opacity: 1;
  }

  .daemon-dot.pulse {
    animation: pulse 1.2s ease-in-out infinite;
  }

  @keyframes pulse {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.25;
    }
  }

  .daemon-host {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* A remote window wears its host name in the accent, so you always know the
     work is running on the cluster, not this laptop. */
  .daemon-host.remote,
  .strip-host.remote {
    color: var(--accent);
    font-weight: 600;
  }

  /* Branch + dirty count: always-on git orientation, quiet until it matters. */
  .daemon-git {
    flex: none;
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    gap: 0.22rem;
    max-width: 50%;
    height: 20px;
    padding: 0 0.3rem;
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    font: inherit;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .daemon-git:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .dg-branch {
    font-family: var(--mono);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }

  .dg-ab,
  .dg-dirty {
    flex: none;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
  }

  .dg-dirty {
    color: var(--git-modified);
  }

  /* Settings gear: the always-there mouse path to ⌘, — quiet until hover. */
  .daemon-settings {
    appearance: none;
    border: none;
    background: none;
    margin-left: auto;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 20px;
    padding: 0;
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .daemon-settings:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .stage {
    flex: 1;
    display: flex;
    min-width: 0;
    min-height: 0;
    position: relative;
    background: var(--bg);
    padding: 8px;
  }

  /* Translucent window-edge drop preview: exactly the half the root split's
     new pane will take, full height/width along that edge. */
  .edge-drop {
    position: absolute;
    z-index: 30;
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 42%, transparent);
    border-radius: 7px;
    pointer-events: none;
  }

  .edge-drop.left {
    inset: 8px 50% 8px 8px;
  }
  .edge-drop.right {
    inset: 8px 8px 8px 50%;
  }
  .edge-drop.top {
    inset: 8px 8px 50% 8px;
  }
  .edge-drop.bottom {
    inset: 50% 8px 8px 8px;
  }

  /* --- session strip (focus mode) --- */

  .strip {
    flex: none;
    display: flex;
    align-items: center;
    gap: 12px;
    height: 32px;
    padding: 0 16px;
    background: var(--rail-bg);
    border-top: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
  }

  /* Explicit "show sidebar" icon — the discoverable mouse path back, beside
     the workspace name which does the same. */
  .strip-show {
    flex: none;
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 22px;
    padding: 0;
    margin-left: -4px;
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .strip-show:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  /* The workspace name doubles as the mouse exit from focus mode. */
  .strip-ws {
    flex: none;
    appearance: none;
    border: none;
    background: none;
    padding: 2px 6px;
    margin: 0 -6px;
    border-radius: 4px;
    font: inherit;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg);
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
    transition: background-color 0.12s ease;
  }

  .strip-ws:hover {
    background: var(--row-hover);
  }

  .chips {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 0.25rem;
    overflow-x: auto;
    scrollbar-width: none;
  }

  .chips::-webkit-scrollbar {
    display: none;
  }

  .chip {
    position: relative;
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.4rem;
    appearance: none;
    border: none;
    background: none;
    padding: 0.15rem 0.55rem;
    border-radius: 4px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    max-width: 180px;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  /* Which-key digit on the strip chip — the ⌘1–9 target in focus mode. Corner
     badge so it overlays rather than widening the chip. */
  .chip-badge {
    position: absolute;
    top: -3px;
    right: -3px;
    min-width: 13px;
    height: 13px;
    padding: 0 2px;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 4px;
    background: color-mix(in srgb, var(--accent) 24%, var(--rail-bg));
    border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent);
    font-family: var(--mono);
    font-size: 9px;
    line-height: 1;
    color: var(--fg);
    pointer-events: none;
    animation: hintfade 0.14s ease-out;
  }

  .chip:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  /* The focused session's chip is inverted — findable at a glance. */
  .chip.focused {
    background: var(--fg);
    color: var(--bg);
  }

  .chip.focused:hover {
    background: var(--fg);
    color: var(--bg);
  }

  .chip-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .strip-needs {
    flex: none;
    color: var(--warn);
    font-variant-numeric: tabular-nums;
  }

  .strip-host {
    flex: none;
    max-width: 140px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* --- blocking re-auth overlay --- */

  .auth-overlay {
    position: fixed;
    inset: 0;
    z-index: 200;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    background: var(--scrim);
    animation: authfade 0.1s ease-out;
  }

  @keyframes authfade {
    from {
      opacity: 0;
    }
  }

  .auth-panel {
    margin-top: 20vh;
    width: min(420px, calc(100vw - 2rem));
    padding: 20px;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    box-shadow: 0 12px 36px rgba(0, 0, 0, 0.22);
  }

  .auth-title {
    font-size: var(--text-md);
    font-weight: 600;
    margin-bottom: 8px;
  }

  .auth-body {
    margin: 0 0 12px;
    font-size: var(--text-md);
    line-height: 1.5;
    color: var(--muted);
  }

  .auth-body code {
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
  }

  .auth-retry {
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    padding: 4px 16px;
    border-radius: 5px;
    font: inherit;
    font-size: var(--text-md);
    color: var(--fg);
    cursor: pointer;
    transition: background-color 0.12s ease;
  }

  .auth-retry:hover:enabled {
    background: var(--row-hover);
  }

  .auth-retry:disabled {
    color: var(--muted);
    cursor: default;
  }

  .auth-msg {
    margin-top: 8px;
    font-size: var(--text-xs);
    color: var(--err);
  }

  /* --- remote reconnect overlay --- */

  .reconnect-overlay {
    position: fixed;
    inset: 0;
    z-index: 190;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    background: var(--scrim);
    animation: authfade 0.1s ease-out;
  }

  .reconnect-panel {
    margin-top: 20vh;
    width: min(420px, calc(100vw - 2rem));
    padding: 20px;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    box-shadow: 0 12px 36px rgba(0, 0, 0, 0.22);
  }

  .reconnect-head {
    display: flex;
    align-items: center;
    gap: 9px;
    margin-bottom: 8px;
  }

  .reconnect-spinner {
    flex: none;
    width: 13px;
    height: 13px;
    border-radius: 50%;
    border: 2px solid color-mix(in srgb, var(--accent) 30%, transparent);
    border-top-color: var(--accent);
  }

  .reconnect-spinner.spin {
    animation: reconnect-spin 0.7s linear infinite;
  }

  @keyframes reconnect-spin {
    to {
      transform: rotate(360deg);
    }
  }

  .reconnect-title {
    font-size: var(--text-md);
    font-weight: 600;
  }

  .reconnect-body {
    margin: 0 0 12px;
    font-size: var(--text-md);
    line-height: 1.5;
    color: var(--muted);
  }

  .reconnect-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .reconnect-dismiss,
  .reconnect-retry {
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    padding: 4px 16px;
    border-radius: 5px;
    font: inherit;
    font-size: var(--text-md);
    color: var(--fg);
    cursor: pointer;
    transition: background-color 0.12s ease;
  }

  .reconnect-dismiss {
    color: var(--muted);
  }

  .reconnect-retry:hover:enabled,
  .reconnect-dismiss:hover {
    background: var(--row-hover);
  }

  .reconnect-retry:disabled {
    color: var(--muted);
    cursor: default;
  }
</style>

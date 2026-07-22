<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import {
    ApiError,
    getActiveWorkspaceId,
    getHostLabel,
    getJobContext,
    getToken,
    notifyUnauthorized,
    pollHealth,
    setActiveWorkspaceId,
    unauthorized,
    type Health,
  } from "./lib/net/api";
  import {
    backgrounded,
    createSession,
    deleteSession,
    deleteWorkspace,
    displayName,
    dotState,
    dotTitle,
    isBusy,
    listSessions,
    listWorkspaces,
    renameSession,
    needsAttention,
    pollSessions,
    switchingViews,
    switchSessionView,
    touchWorkspace,
    ViewSwitchConflict,
    type AgentSpawn,
    type Session,
    type Workspace,
    isMastermind,
  } from "./lib/workspace/sessions";
  import { foldUnread, isUnread, markSeen } from "./lib/workspace/unread.svelte";
  import {
    getAgentDefault,
    installAgent,
    listAgents,
    listRecents,
    relativeAge,
    setAgentDefault,
    updateAgent,
    type AgentInfo,
    type LaunchPick,
    type RecentConvo,
  } from "./lib/workspace/launcher";
  import { EventsSocket } from "./lib/net/events";
  import {
    agentHue,
    deleteLink,
    listLinks,
    putLink,
    termReference,
    type Link,
    type LinkCtrl,
  } from "./lib/workspace/agentLinks";
  import { typeIntoDetachedSession } from "./lib/terminal/ws";
  import { reconnectingSockets } from "./lib/net/reconnect";
  import { selectRemoteReconnectSurface } from "./lib/net/remoteReconnect";
  import { attachImageToComposer, insertIntoComposer } from "./lib/chat/composerBus";
  import { volatileChatDrafts } from "./lib/chat/drafts";
  import { imageToAttachment } from "./lib/chat/images";
  import {
    reportUploadError,
    setUploadPathInserter,
    trackFileOp,
    uploadAndInsert,
    uploadJobs,
    uploadToDir,
  } from "./lib/net/uploads";
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
  } from "./lib/shared/reference";
  import { provenanceFor, rememberCopy } from "./lib/shared/provenance";
  import { asyncDisposer } from "./lib/shared/asyncDisposer";
  import { modalFocus } from "./lib/shared/modalFocus";
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
    movePane,
    movePaneToIndex,
    movePaneToRootEdge,
    openChanges,
    openDiff,
    openFile,
    pinTab,
    pinPaths,
    openFinder,
    findFinder,
    freshFinderTab,
    setFinderPath,
    openBrowser,
    openTab,
    freshBrowserTab,
    setBrowserPath,
    setBrowserTarget,
    openDashboard,
    openGit,
    openSession,
    openSettings,
    paneForTab,
    panes as panesOf,
    pruneDeletedPath,
    pruneFiles,
    pruneSessions,
    rewriteTabPaths,
    serializeLayout,
    sessionPaneId,
    setPaneFont,
    setRatio,
    splitPane,
    moveTabDirection,
    tabCount,
    toggleZoom,
    type FocusDir,
    type Layout,
    type SplitDir,
    type Tab,
  } from "./lib/layout/layout";
  import type { PathKind } from "./lib/terminal/links";
  import type { UrlTarget } from "./lib/terminal/urlLinks";
  import { sweepProxies } from "./lib/browser/proxy";
  import { basename, fileTabTitles, fsProbe, viewKindFor } from "./lib/previews/files";
  import { dirtyFiles } from "./lib/shared/editing";
  import {
    activateGitWorkspace,
    gitEnv,
    gitRepoError,
    gitStatus,
    onGitNudge,
    workspacesChanged,
    type DiffMode,
  } from "./lib/workspace/git";
  import { computeStatus, initCompute, queuedJobCount } from "./lib/workspace/compute";
  import ComputeStrip from "./lib/workspace/ComputeStrip.svelte";
  import {
    paneContentEl,
    paneIdAt,
    paneRootEl,
    registerLinkRow,
    registerStage,
    startDrag,
    unregisterLinkRow,
    unregisterStage,
    type DropSpot,
    type LayoutCtrl,
  } from "./lib/layout/dnd";
  import { chordDigit, fontChord, isMac, matchChord, REFERENCE_CHORD } from "./lib/shared/keys";
  import {
    activeModLabel,
    isCapturing,
    keyHint,
    matchAction,
    modifierSetting,
  } from "./lib/shared/keybindings";
  import {
    askpassActive,
    caffeinateState,
    closeThisWindow,
    connectComputeSession,
    connectHost,
    isNativeShell,
    onAppUpdate,
    onCaffeinateChanged,
    onHostStatus,
    onLocalDaemonUpdated,
    onMenu,
    reportWindowScope,
    setCaffeinate,
    setNativeWindowTitle,
    shellBuild,
  } from "./lib/net/native";
  import UpdateToast from "./lib/workspace/UpdateToast.svelte";
  import { currentOffer, updateState } from "./lib/workspace/update.svelte";
  import * as pool from "./lib/terminal/termPool";
  import * as chatPool from "./lib/chat/chatPool";
  import {
    applyRemoteSettings,
    flushSettings,
    getSetting,
    loadSettings,
    setSetting,
    settingsLoaded,
  } from "./lib/settings/store.svelte";
  import type { DashCtx } from "./lib/dashboard/dash";
  import { flushViewState, loadViewState, saveViewState, windowKey } from "./lib/layout/viewState";
  import {
    FILES_FRAC_MAX,
    FILES_FRAC_MIN,
    RAIL_DEFAULT,
    RAIL_MAX,
    RAIL_MIN,
    loadRailChrome,
    saveRailChrome,
  } from "./lib/layout/railState";
  import { hintsActive, initChordHints } from "./lib/shared/chordHints.svelte";
  import FolderPicker from "./lib/workspace/FolderPicker.svelte";
  import HomeScreen from "./lib/workspace/HomeScreen.svelte";
  import AskpassModal from "./lib/workspace/AskpassModal.svelte";
  import ContextMenuHost from "./lib/shared/ContextMenuHost.svelte";
  import { contextMenu } from "./lib/shared/contextMenu.svelte";
  import ConfirmDialog from "./lib/shared/ConfirmDialog.svelte";
  import { fsDeleteOp, lastFsMutation, notifyCreated, pendingDelete } from "./lib/workspace/fsEvents";
  import {
    currentDiskWatches,
    diskWatchDirs,
    diskWatchFiles,
    lastDiskChange,
    notifyDiskChange,
  } from "./lib/workspace/diskWatch";
  import ReauthOverlay from "./lib/workspace/ReauthOverlay.svelte";
  import AssetTransitionNotice from "./lib/layout/AssetTransitionNotice.svelte";
  import {
    assetTransition,
    BUILD_META_NAME,
    buildSource,
    clearChunkFailure,
    documentBuildSource,
    noteChunkFailure,
    rearmAssetNavigation,
    requestAssetReload,
    requireAssetNavigation,
  } from "./lib/layout/assetTransition";
  import { focusOnMount } from "./lib/shared/focusOnMount";
  import Launcher from "./lib/workspace/Launcher.svelte";
  import SessionGlyph from "./lib/shared/SessionGlyph.svelte";
  import QuickOpen from "./lib/workspace/QuickOpen.svelte";
  import FileTree from "./lib/workspace/FileTree.svelte";
  import SplitTree from "./lib/layout/SplitNode.svelte";
  import Pane from "./lib/layout/Pane.svelte";

  let health = $state<Health | null>(null);
  /** Source identity of the daemon that supplied this document. A different
   *  source means its immutable lazy-chunk namespace changed underneath us. */
  let servedBuild = documentBuildSource(
    document.querySelector<HTMLMetaElement>(`meta[name="${BUILD_META_NAME}"]`)?.content,
  );
  function daemonBuildChanged(next: string | null | undefined): boolean {
    const nextSource = buildSource(next);
    return servedBuild !== null && nextSource !== null && nextSource !== servedBuild;
  }
  /** This app binary's build id (native only); vs health.build = skew. */
  let appBuild = $state<string | null>(null);
  /** Caffeinate (native, LOCAL macOS window only): whole-machine keep-awake.
   *  Shown only on a local native window — a remote window's work runs on the
   *  remote host, so caffeinating this laptop would be pointless — and only on
   *  macOS, where the clamshell/lid-close keep-awake it exists for applies. */
  let caffeinated = $state(false);
  const canCaffeinate = isNativeShell() && isMac && getHostLabel() === "local";
  /** macOS workbench windows put the native controls over the webview instead
   *  of spending a separate row on the repeated window title. */
  const nativeTitlebarOverlay = isNativeShell() && isMac;
  async function toggleCaffeinate(): Promise<void> {
    try {
      caffeinated = await setCaffeinate(!caffeinated);
    } catch (e) {
      // The power assertion can fail to acquire; don't leave the button lying
      // about the state — resync from the real one, and surface the reason.
      console.error("caffeinate toggle failed", e);
      try {
        caffeinated = await caffeinateState();
      } catch {
        /* best-effort resync */
      }
    }
  }
  /** Last recents epoch seen on /ws/events (invalidate-and-pull). */
  let lastRecentsEpoch: number | null = null;
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
  /** Set when this window sits on a compute-node daemon (Mode 2 job). */
  const jobCtx = getJobContext();
  /** The key this window's tunnel reports `host-status` under. A job window
   *  listens ONLY on its composite key — matching the bare login alias made
   *  every login-tunnel blip re-home job windows onto the login daemon
   *  (found live: "opening the session just opens Sherlock"). */
  const statusAlias = jobCtx !== null ? `${hostAlias}#job${jobCtx.jobId}` : hostAlias;
  /** Only the native shell can re-run ssh; a browser tunnel is the CLI's job. */
  const canReconnect = isRemoteWindow && isNativeShell();
  /** This window's scope key for the shell registry (null alias = local). */
  const scopeAlias = isRemoteWindow ? (jobCtx !== null ? statusAlias : hostAlias) : null;
  /** The reconnect status or failure dialog is showing. */
  let showReconnect = $state(false);
  /** A connectHost call is in flight. */
  let reconnecting = $state(false);
  /** Last reconnect failure, surfaced with a Retry. */
  let reconnectError = $state<string | null>(null);
  /** Context from the shell's liveness monitor for the current drop. */
  let reconnectReason = $state<string | null>(null);
  /** The user dismissed the overlay; don't auto-reshow until state changes. */
  let reconnectDismissed = $state(false);
  let reconnectGrace: ReturnType<typeof setTimeout> | null = null;
  /** Dismissing a failed reconnect downgrades it to an ambient Retry instead
   *  of removing the only recovery path. Unauthorized native windows never
   *  fall through to the browser-only manual auth overlay. */
  const reconnectSurface = $derived(
    selectRemoteReconnectSurface({
      open: showReconnect,
      error: reconnectError,
      authBlocked: canReconnect && $unauthorized,
    }),
  );

  /** Re-establish this remote window's ssh tunnel. Idempotent: the shell
   *  reuses a live tunnel, and reuses the old loopback port so a heal needs no
   *  navigation. Surfaces the SSH auth modal (mounted app-wide) only if ssh
   *  actually re-prompts. A job window heals through the COMPUTE path — its
   *  tunnel goes laptop→node, and rebuilding the login tunnel wouldn't touch
   *  it; the shell probes/rebuilds the job tunnel and pings the composite
   *  status key with the (possibly new) port. */
  async function attemptReconnect(): Promise<void> {
    if (!canReconnect || reconnecting) return;
    reconnecting = true;
    reconnectError = null;
    try {
      if (jobCtx !== null) {
        await connectComputeSession(hostAlias, jobCtx.jobId);
      } else {
        await connectHost(hostAlias);
      }
      // Same-port heal → our WebSocket reconnects and eventsUp clears the
      // overlay; a moved port/token re-homes this window via onHostStatus.
    } catch (e) {
      reconnectError = e instanceof Error ? e.message : String(e);
    } finally {
      reconnecting = false;
    }
  }

  function beginReconnect(reason: string | null = null): void {
    // A shell liveness event is authoritative and skips the socket grace even
    // if Svelte has not observed the corresponding events-socket close yet.
    if (!canReconnect || (reason === null && eventsUp)) return;
    if (reason !== null) reconnectReason = reason;
    reconnectDismissed = false;
    showReconnect = true;
    void attemptReconnect();
  }

  function retryReconnect(): void {
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

  // A 401 in a native remote window usually means the daemon restarted and
  // minted a fresh tunnel token. Let the shell reconnect this exact host and
  // re-home the window; the generic "paste a URL" auth page is only correct
  // for browser tunnels, which cannot re-run ssh themselves.
  let handledRemoteUnauthorized = false;
  $effect(() => {
    if (!canReconnect || !$unauthorized || handledRemoteUnauthorized) return;
    handledRemoteUnauthorized = true;
    untrack(() =>
      beginReconnect("The remote daemon changed credentials; reconnecting will refresh this window."),
    );
  });

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
      reconnectReason = null;
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
    if (isNativeShell()) void reportWindowScope(scopeAlias, activeWsId, windowLabel || null);
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
  /** Recents to SHOW: drop any whose conversation is currently LIVE (a live
   *  agent with the same title). A running conversation belongs under AGENTS,
   *  not RECENT — and the server's live-exclusion can briefly miss a fresh
   *  session whose transcript id isn't recorded yet, so a row would otherwise
   *  appear in both sections at once. */
  const visibleRecents = $derived.by(() => {
    const liveTitles = new Set(
      sessions.filter((s) => s.kind === "agent" && s.alive).map((s) => displayName(s)),
    );
    return recents.filter((r) => r.title === "" || !liveTitles.has(r.title));
  });
  /** Rail row currently showing the inline kill confirmation. */
  let confirmKillId = $state<string | null>(null);
  /** Rail row currently in inline rename (double-click or F2). */
  let renamingId = $state<string | null>(null);
  let renameDraft = $state("");
  /** Inline-create request for the file tree (rail-header buttons). */
  let treeCreate = $state<{ kind: "file" | "dir"; nonce: number } | null>(null);
  let treeCreateNonce = 0;
  /** A failed delete's message; keeps the confirm dialog open. */
  let deleteError = $state<string | null>(null);
  /** Element that held focus when the picker opened; restored on close. */
  let pickerRestoreEl: HTMLElement | null = null;

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

  /**
   * Workspace-only view-state key (no window id). The layout is mirrored here so
   * a window that reopens a workspace with a FRESH id — after a deliberate close
   * (the native shell discards the window's identity by macOS convention), or a
   * plain browser reopen — still restores that workspace's last-active layout
   * instead of falling back to the empty default. Consulted only when the
   * window-keyed blob is absent; last-active window on a workspace wins.
   */
  function wsKey(wsId: string): string {
    return `ws_${wsId}`;
  }

  const workspace = $derived(workspaces.find((w) => w.id === activeWsId) ?? null);
  // The Mastermind is the observer, not the observed: its flagged row never
  // enters the workspace roster — the rail, chord map, attention counts,
  // quick-open, and reference targets all derive from here. Lookups by id
  // (sessionsById, the pools' keep-alive sync) stay on the unfiltered list.
  const wsSessions = $derived(
    sessions.filter((s) => s.workspace_id === activeWsId && !isMastermind(s)),
  );

  /** How this window is named in the shell's tray window-list: the workspace
   *  name (with the host on a remote window), or "Home" for the home screen —
   *  NOT the OS titlebar (which appends "| chimaera"). Reported with the scope
   *  so the tray never falls back to the generic app title. Empty while the
   *  workspace is still loading; the tray shows a placeholder until it lands. */
  const windowLabel = $derived(
    activeWsId === null
      ? isRemoteWindow
        ? `Home •${hostAlias}`
        : "Home"
      : (workspace?.name ?? "") + (isRemoteWindow ? ` •${hostAlias}` : ""),
  );

  /** The daemon-wide events socket (created in onMount); used to register the
   *  workspace this window watches, which gates the daemon's git backstop. */
  let eventsSocket: EventsSocket | null = null;

  // Keep the git store pointed at the active workspace. It fetches status on
  // change; the events-socket epoch nudge refetches on subsequent changes. The
  // watch registration tells the daemon to keep polling this repo for
  // out-of-band changes (a terminal `git` command, an external editor).
  $effect(() => {
    const wsId = activeWsId;
    void activateGitWorkspace(wsId);
    eventsSocket?.watch(wsId);
  });

  // Mounted file views + visible listing directories are the complete scope of
  // the daemon's bounded disk monitor. Registrations follow keep-alive mounts,
  // not every tab ever opened, and are re-sent after reconnect by EventsSocket.
  $effect(() => {
    const files = $diskWatchFiles;
    const dirs = $diskWatchDirs;
    eventsSocket?.watchFs(files, dirs);
  });

  // A worktree create/remove changed the daemon's workspace registry: re-fetch
  // the list so the home screen and switcher stay honest (a removed worktree's
  // workspace disappears; a created one appears).
  let lastWsChange = 0;
  $effect(() => {
    const n = $workspacesChanged;
    if (n !== lastWsChange) {
      lastWsChange = n;
      refreshWorkspaces();
    }
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

  /** The focused pane is showing the dashboard (its rail row highlights). */
  const dashboardOpen = $derived.by(() => {
    const p = findPane(layout.root, layout.focusedPaneId);
    return p?.tabs[p.active]?.surface === "dashboard";
  });

  // --- context bridge: reference target resolution ---------------------------

  /** Agent sessions this window focused, most recent first. */
  let agentMru = $state<string[]>([]);
  $effect(() => {
    const sid = focusedSessionId;
    if (sid === null || sessionsById.get(sid)?.kind !== "agent") return;
    if (agentMru[0] === sid) return;
    agentMru = [sid, ...agentMru.filter((x) => x !== sid)].slice(0, 16);
  });

  // Looking at a session clears its unread mark (the rail rows and
  // dashboard cards wear it until then).
  $effect(() => {
    const sid = focusedSessionId;
    if (sid !== null) markSeen(sid);
  });

  /** App-level context the dashboard surface needs beyond the pane props. */
  const dashCtx = $derived<DashCtx>({
    wsName: workspace?.name ?? "",
    ready: gotSessions,
    recents: visibleRecents,
    mru: agentMru,
    mastermind: workspace?.mastermind ?? null,
    refreshWorkspaces,
    onOpenRecent: openRecent,
    onNewTerminal: newShell,
    onNewAgent: newAgentPrimary,
    onOpenGit: openGitPanel,
    onOpenSession: openSess,
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
        if (daemonBuildChanged(h.build)) {
          // The origin can stay stable across a daemon handoff, but the
          // hashed JS namespace cannot. Reload before the user opens a lazy
          // surface whose old URL now correctly 404s.
          requireAssetNavigation("build", null);
          return;
        }
        servedBuild ??= buildSource(h.build);
        health = h;
      },
      () => {
        // unreachable daemon; the events socket state already reflects this
      },
    ),
  );

  // Vite reports every failed production dynamic import here, including the
  // nested PDF/editor/spreadsheet chunks that never pass through Pane's
  // top-level loader. Keep the rejection flowing to its local error boundary
  // while one shared notice offers build-safe recovery.
  $effect(() => {
    const onPreloadError = (event: VitePreloadErrorEvent) => {
      console.error("interface chunk failed to load", event.payload);
      noteChunkFailure();
    };
    window.addEventListener("vite:preloadError", onPreloadError);
    return () => window.removeEventListener("vite:preloadError", onPreloadError);
  });

  // Build/connection transitions navigate automatically only when all local
  // state survives navigation. A blocked transition stays visible instead of
  // looping beforeunload prompts or silently dropping a memory-only draft.
  let handledAssetRevision = 0;
  $effect(() => {
    const transition = $assetTransition;
    if (transition === null || !transition.requested) return;
    const blocked = $dirtyFiles.size > 0 || $volatileChatDrafts.size > 0;
    if (blocked && !transition.forced) return;
    if (transition.revision === handledAssetRevision) return;
    handledAssetRevision = transition.revision;
    untrack(() => {
      if (transition.target === null) location.reload();
      else location.replace(transition.target);
      // This callback can run only if the document survived the navigation
      // call (normally because the user chose Stay in beforeunload). Re-arm
      // with `forced` cleared: dirty state holds the next attempt, and saving
      // it lets the existing effect retry once without a prompt loop.
      setTimeout(() => rearmAssetNavigation(transition.revision), 0);
    });
  });

  // Slurm strip: one probe at boot; the store keeps its own 60s poll gated on
  // "scheduler is slurm" + a visible window (see workspace/compute.ts).
  $effect(() => initCompute());

  // Daemon/app build skew (native): the daemon serving this window is a
  // different build than the app — the update toast offers the restart.
  // Health rides a poll, so this stays current without extra traffic. The
  // build-timestamp suffix is ignored, like core's builds_match: the same
  // commit built at different moments must not read as skew.
  $effect(() => {
    updateState.buildSkew =
      appBuild !== null &&
      typeof health?.build === "string" &&
      health.build.split(".")[0] !== appBuild.split(".")[0];
  });

  /** The one update worth offering in this window right now. */
  const updateOffer = $derived(currentOffer(scopeAlias));

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
    // Mirror under the workspace-only key so a reopened window (fresh id)
    // restores this workspace instead of the empty default (see wsKey).
    if (activeWsId !== null) saveViewState(wsKey(activeWsId), blob);
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

  // Dispose pooled terminals AND pooled chat sessions for sessions that no
  // longer exist (a killed session, a workspace switch).
  $effect(() => {
    const ids = sessions.map((s) => s.id);
    if (!gotSessions) return;
    pool.syncSessions(ids);
    chatPool.syncChatSessions(new Set(ids));
  });

  // Revoke proxy sessions whose last browser tab closed (hygiene — the
  // daemon's idle TTL is the backstop, and other windows self-heal by
  // re-minting).
  $effect(() => {
    const active = new Set<string>();
    for (const p of panesOf(layout.root)) {
      for (const t of p.tabs) {
        if (t.surface === "browser" && t.host !== "") active.add(`${t.host}:${t.port}`);
      }
    }
    sweepProxies(active);
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
      onOpenUrl,
    });
    setReferenceHandler(referenceSelection);
    setUploadPathInserter(insertUploadedPath);
    // OS-desktop file drags: window-level so the navigate-away default is
    // dead EVERYWHERE, not just over accepting panes.
    window.addEventListener("dragenter", onWindowDragEnter);
    window.addEventListener("dragover", onWindowDragOver);
    window.addEventListener("dragleave", onWindowDragLeave);
    window.addEventListener("drop", onWindowDrop);
    const events = new EventsSocket({
      onSessions: (list, linkList) => {
        applySessions(list);
        if (linkList !== undefined) links = linkList;
      },
      onSettings: applyRemoteSettings,
      onGit: onGitNudge,
      onUpdate: (status) => (updateState.daemon = status),
      onRecents: (epoch) => {
        // Invalidate-and-pull, like git: a conversation retired somewhere;
        // refetch this window's workspace list iff the epoch moved.
        if (lastRecentsEpoch !== epoch) {
          lastRecentsEpoch = epoch;
          refreshRecents();
        }
      },
      onFs: notifyDiskChange,
      onStatus: (up) => (eventsUp = up),
      onFatal: (message) => {
        if (message === "unauthorized") notifyUnauthorized();
      },
    });
    eventsSocket = events;
    // Register this window's workspace so the daemon's git backstop polls it.
    events.watch(activeWsId);
    const diskWatches = currentDiskWatches();
    events.watchFs(diskWatches.files, diskWatches.dirs);
    refreshWorkspaces();
    void bootViewState();
    // Re-run the landing one-shot once settings settle: the sessions
    // snapshot may have arrived first, and pruneAndAutoOpen gates on
    // settingsLoaded() so an explicit landing choice is never raced.
    void loadSettings().then(pruneAndAutoOpen);
    void refreshAgents();

    // Native menu items the shell forwards to the focused window. Cmd+W
    // closes the focused VIEW (a home window just closes), reclaiming the
    // chords a browser reserves for tabs.
    let unlistenMenu: (() => void) | null = null;
    let unlistenDaemonMoved: (() => void) | null = null;
    let unlistenHostStatus: (() => void) | null = null;
    let unlistenAppUpdate: (() => void) | null = null;
    let unlistenCaffeinate: (() => void) | null = null;
    if (isNativeShell()) {
      // Build-skew + app-update signals for the update toast.
      void shellBuild().then((b) => (appBuild = b));
      // Caffeinate state: attach the cross-window broadcast FIRST, then read
      // the current value — so an event fired during startup isn't missed, and
      // the initial read can't clobber a fresher value the listener applied.
      if (canCaffeinate) {
        const listening = onCaffeinateChanged((on) => (caffeinated = on));
        unlistenCaffeinate = asyncDisposer(listening);
        void listening.then(
          () => {
            void caffeinateState().then((on) => (caffeinated = on));
          },
          () => {},
        );
      }
      unlistenAppUpdate = asyncDisposer(
        onAppUpdate((version) => (updateState.appVersion = version)),
      );
      unlistenMenu = asyncDisposer(
        onMenu((action) => {
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
            case "settings":
              openSettingsSurface();
              break;
          }
        }),
      );
      // The local daemon was replaced (self-update). Re-home (carrying the
      // window id, so the layout follows) when its origin moved; when the
      // handoff retained port+token, a changed build still requires a reload
      // because the immutable hashed asset namespace changed.
      unlistenDaemonMoved = asyncDisposer(
        onLocalDaemonUpdated(({ port, token, build }) => {
          if (getHostLabel() !== "local") return;
          if (String(port) === location.port && token === getToken()) {
            if (daemonBuildChanged(build)) requireAssetNavigation("build", null);
            return;
          }
          const params = new URLSearchParams();
          params.set("token", token);
          params.set("win", windowKey());
          if (activeWsId !== null) params.set("ws", activeWsId);
          requireAssetNavigation(
            "build",
            `http://127.0.0.1:${port}/#${params.toString()}`,
          );
        }),
      );
      // This remote window's tunnel dropped or came back. "down" → reconnect
      // now (the shell confirmed the forward is dead); "connected" → re-home
      // if the origin moved, or reload if only the daemon build moved. A pure
      // same-build heal stays in place and its WebSocket just reconnects.
      // Matching is on statusAlias: a job window heeds ONLY its composite
      // key's events — the login alias's port/token are another daemon.
      unlistenHostStatus = asyncDisposer(
        onHostStatus((e) => {
          if (!canReconnect || e.alias !== statusAlias) return;
          if (e.status === "down") {
            beginReconnect(e.reason ?? "The remote connection stopped responding.");
            return;
          }
          const port = e.local_port;
          // Compute-status events carry no token (compute tokens stay in the
          // shell's Rust side); a rebuilt job tunnel lands on the SAME daemon,
          // so this window's own token still holds — carry it across the
          // origin move ourselves.
          const token = e.token ?? (jobCtx !== null ? getToken() : null);
          const portMoved = port !== null && String(port) !== location.port;
          const tokenMoved = token !== null && token !== getToken();
          const buildMoved = daemonBuildChanged(e.build);
          if (portMoved || tokenMoved) {
            const params = new URLSearchParams();
            if (token !== null) params.set("token", token);
            params.set("win", windowKey());
            if (activeWsId !== null) params.set("ws", activeWsId);
            params.set("host", hostAlias);
            if (jobCtx !== null) {
              params.set("job", jobCtx.jobId);
              if (jobCtx.node !== null) params.set("node", jobCtx.node);
            }
            requireAssetNavigation(
              buildMoved ? "build" : "connection",
              `http://127.0.0.1:${port ?? location.port}/#${params.toString()}`,
            );
          } else if (buildMoved) {
            requireAssetNavigation("build", null);
          }
        }),
      );
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
      unlistenAppUpdate?.();
      unlistenCaffeinate?.();
      document.removeEventListener("copy", onCopy);
      window.removeEventListener("dragenter", onWindowDragEnter);
      window.removeEventListener("dragover", onWindowDragOver);
      window.removeEventListener("dragleave", onWindowDragLeave);
      window.removeEventListener("drop", onWindowDrop);
      stopChordHints();
      setReferenceHandler(null);
      setUploadPathInserter(null);
      events.close();
      pool.disposePool();
      chatPool.disposeAllChats();
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
    // Chat sessions: the input is the mounted Composer, not a PTY socket.
    // insertIntoComposer buffers when the composer hasn't mounted yet (a chat
    // pane restored on a slow snapshot) and drains on registration, so the
    // grant is never lost to a mount race.
    if (sessionsById.get(id)?.ui === "chat") {
      const loc = paneForTab(layout.root, { surface: "terminal", sessionId: id });
      if (loc !== null) layout = activateTab(layout, loc.paneId, loc.index);
      insertIntoComposer(id, text);
      return;
    }
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
   * Drag-to-reference drop: type the dropped path (file OR folder) into the
   * pane's session — claude agents get the native @mention (workspace-
   * relative), plain terminals get the shell-escaped path relative to their
   * live cwd. Dir mentions carry a trailing slash (the composer/TUI
   * convention — reads unambiguously as "this folder"); shell paths stay
   * bare, matching what a shell command expects.
   */
  function referenceFileDrop(paneId: string, path: string, kind: "file" | "dir" = "file"): void {
    const p = findPane(layout.root, paneId);
    if (p === null) return;
    const active = p.tabs[p.active];
    if (active === undefined || active.surface !== "terminal") return;
    const s = sessionsById.get(active.sessionId);
    if (s === undefined || !s.alive) return;
    const root = workspace?.root;
    const rel = root !== undefined ? workspaceRelative(path, root) : path;
    const text =
      s.kind === "agent"
        ? composeAgentPathReference(kind === "dir" ? `${rel}/` : rel)
        : composeShellPathReference(path, s.cwd_current ?? s.cwd);
    typeIntoSession(active.sessionId, text);
  }

  // --- OS-desktop file drops: upload to the session's host, then reference ---

  /**
   * Uploaded paths type through the same composition as drag-to-reference:
   * agents get @mentions (absolute — uploads live outside the workspace
   * root, which workspaceRelative passes through unchanged), shells get
   * shell-escaped paths. Registered with the uploads module, which owns the
   * upload half (terminal paste routes through it too).
   */
  function insertUploadedPath(sessionId: string, absPath: string): void {
    const s = sessionsById.get(sessionId);
    if (s === undefined || !s.alive) return;
    const root = workspace?.root;
    const text =
      s.kind === "agent"
        ? composeAgentPathReference(root !== undefined ? workspaceRelative(absPath, root) : absPath)
        : composeShellPathReference(absPath, s.cwd_current ?? s.cwd);
    typeIntoSession(sessionId, text);
  }

  /** The live session shown by `paneId`, if any — the OS-drop target gate
   *  (same rule as the pointer-drag "@ reference" band). */
  function osDropSession(paneId: string): Session | null {
    const p = findPane(layout.root, paneId);
    const a = p?.tabs[p.active];
    if (a === undefined || a.surface !== "terminal") return null;
    const s = sessionsById.get(a.sessionId);
    return s !== undefined && s.alive ? s : null;
  }

  /** The FOLDER an OS-desktop file drop at (x, y) should land in — a Finder
   *  column, a FILES-tree dir row, or the tree root — or null when the point
   *  is over neither. Hit-tests the data attributes those surfaces stamp
   *  (elementFromPoint is valid during an HTML5 drag; there is no in-DOM ghost
   *  to occlude). `paneId` is the Finder pane to wash (null for the rail tree). */
  function osFolderTargetAt(x: number, y: number): { paneId: string | null; dir: string } | null {
    const el = document.elementFromPoint(x, y);
    if (!(el instanceof Element)) return null;
    // A FILES-tree dir row (files target their parent; broken links have none).
    const row = el.closest<HTMLElement>("[data-drop-dir]");
    if (row?.dataset.dropDir != null && el.closest(".tree") !== null) {
      return { paneId: null, dir: row.dataset.dropDir };
    }
    // The tree background → the workspace root.
    const treeRoot = el.closest<HTMLElement>("[data-tree-root]");
    if (treeRoot?.dataset.treeRoot != null) {
      return { paneId: null, dir: treeRoot.dataset.treeRoot };
    }
    // A Finder column → that column's directory.
    const col = el.closest<HTMLElement>("[data-finder-dir]");
    if (col?.dataset.finderDir != null) {
      return { paneId: paneIdAt(x, y), dir: col.dataset.finderDir };
    }
    return null;
  }

  /** Depth-counted dragenter/leave so child enter/leave churn never flickers
   *  the drop overlay off mid-drag. */
  let osDragDepth = 0;

  function isOsFileDrag(e: DragEvent): boolean {
    return e.dataTransfer?.types.includes("Files") ?? false;
  }

  function onWindowDragEnter(e: DragEvent): void {
    if (!isOsFileDrag(e)) return;
    e.preventDefault();
    osDragDepth += 1;
  }

  function onWindowDragOver(e: DragEvent): void {
    // Claim only FILE drags: the browser's default for an unhandled file drop
    // is to NAVIGATE AWAY from the app. A native text/URL drag must keep its
    // default drop into an input, so gate BEFORE preventDefault (matching
    // onWindowDragEnter/Leave — an unconditional preventDefault here silently
    // breaks text/URL drop into the composer and every other input).
    if (!isOsFileDrag(e)) return;
    e.preventDefault();
    // A folder target (Finder column / FILES-tree dir) wins over the session
    // pane it may sit inside — dropping onto the file manager uploads THERE.
    const folder = osFolderTargetAt(e.clientX, e.clientY);
    if (folder !== null) {
      if (e.dataTransfer !== null) e.dataTransfer.dropEffect = "copy";
      dropSpot = { kind: "uploadDir", paneId: folder.paneId, dir: folder.dir };
      return;
    }
    const paneId = paneIdAt(e.clientX, e.clientY);
    const ok = paneId !== null && osDropSession(paneId) !== null;
    if (e.dataTransfer !== null) e.dataTransfer.dropEffect = ok ? "copy" : "none";
    // OS drops reuse the dropSpot plumbing: the whole pane is the target
    // (HTML5 dnd has no competing tile gesture to partition against).
    dropSpot = ok && paneId !== null ? { kind: "upload", paneId } : null;
  }

  function onWindowDragLeave(e: DragEvent): void {
    if (!isOsFileDrag(e)) return;
    osDragDepth = Math.max(0, osDragDepth - 1);
    if (osDragDepth === 0 && (dropSpot?.kind === "upload" || dropSpot?.kind === "uploadDir")) {
      dropSpot = null;
    }
  }

  function onWindowDrop(e: DragEvent): void {
    // Only claim FILE drops — see onWindowDragOver. A text/URL drop into an
    // input must keep its native behavior, so gate before preventDefault.
    if (!isOsFileDrag(e)) return;
    e.preventDefault();
    osDragDepth = 0;
    const spot = dropSpot;
    if (spot?.kind === "upload" || spot?.kind === "uploadDir") dropSpot = null;
    const dt = e.dataTransfer;
    if (dt === null) return;
    // Read files AND directory-ness synchronously — webkitGetAsEntry is only
    // valid while the drop event dispatches.
    const picked = [...dt.items]
      .filter((i) => i.kind === "file")
      .map((i) => ({ file: i.getAsFile(), dir: i.webkitGetAsEntry?.()?.isDirectory === true }));
    // A folder target (file manager) uploads INTO that directory; otherwise
    // fall back to the live-session upload+reference flow.
    const folder = osFolderTargetAt(e.clientX, e.clientY);
    if (folder !== null) {
      void dropFilesInDir(folder.dir, picked);
      return;
    }
    const paneId = paneIdAt(e.clientX, e.clientY);
    const s = paneId !== null ? osDropSession(paneId) : null;
    if (s === null) return;
    void dropFilesOnSession(s.id, picked);
  }

  /** Upload each dropped file INTO `dir` (a file-manager folder target), then
   *  nudge the fs bus so the tree/Finder re-list. Same 8-file cap + folders-
   *  rejected rule as the session drop. */
  async function dropFilesInDir(
    dir: string,
    picked: { file: File | null; dir: boolean }[],
  ): Promise<void> {
    let accepted = 0;
    for (const { file, dir: isDir } of picked) {
      if (isDir) {
        reportUploadError(`${file?.name ?? "folder"}: drop files, not folders`);
        continue;
      }
      if (file === null) continue;
      if (accepted >= OS_DROP_MAX_FILES) {
        reportUploadError(`${file.name}: skipped (max ${OS_DROP_MAX_FILES} files per drop)`);
        continue;
      }
      accepted += 1;
      const result = await trackFileOp(`Uploading ${file.name}…`, (progress) =>
        uploadToDir(dir, file, file.name, progress),
      );
      // Re-list the destination so the new file appears without a manual refresh.
      if (result !== null) notifyCreated(result.path);
    }
  }

  /** How many files one drop gesture will accept. */
  const OS_DROP_MAX_FILES = 8;

  async function dropFilesOnSession(
    sessionId: string,
    picked: { file: File | null; dir: boolean }[],
  ): Promise<void> {
    let accepted = 0;
    for (const { file, dir } of picked) {
      if (dir) {
        // Folder uploads need recursive traversal — out of scope for v1.
        reportUploadError(`${file?.name ?? "folder"}: drop files, not folders`);
        continue;
      }
      if (file === null) continue;
      if (accepted >= OS_DROP_MAX_FILES) {
        reportUploadError(`${file.name}: skipped (max ${OS_DROP_MAX_FILES} files per drop)`);
        continue;
      }
      accepted += 1;
      // An image dropped on a CHAT pane also attaches its pixels, so the
      // model sees it immediately; the uploaded path stays the durable,
      // host-side artifact the agent can re-read later.
      const s = sessionsById.get(sessionId);
      if (s?.ui === "chat" && file.type.startsWith("image/")) {
        const attachment = await imageToAttachment(file);
        if (attachment !== null) attachImageToComposer(sessionId, attachment);
      }
      // Sequential: keeps multi-file reference order stable in the input.
      await uploadAndInsert(sessionId, file, file.name);
    }
  }

  // --- clickable paths: the bridge's return direction ------------------------

  /** Resolution context for a session's terminal link provider. */
  function linkContext(id: string): {
    cwd: string | null;
    root: string | null;
    workspaceId: string | null;
  } {
    const s = sessionsById.get(id);
    // The session's own workspace, else the active one — the same preference
    // order the root fallback uses, so root and workspaceId stay in step.
    const ws = workspaces.find((w) => w.id === s?.workspace_id) ?? workspace;
    return {
      cwd: s?.cwd_current ?? s?.cwd ?? null,
      root: ws?.root ?? null,
      workspaceId: ws?.id ?? null,
    };
  }

  /**
   * A proxyable URL link was activated (the browser pane's front door). An
   * existing pane on the same host:port target is focused — clicking
   * Jupyter's printed URL twice lands where you already are — while Cmd/Ctrl
   * forces a fresh split beside the terminal.
   */
  function onOpenUrl(id: string, target: UrlTarget, newSplit: boolean): void {
    const loc = paneForTab(layout.root, { surface: "terminal", sessionId: id });
    const fromPane = loc?.paneId ?? layout.focusedPaneId;
    if (newSplit) {
      layout = splitPane(layout, fromPane, "row");
      layout = openTab(layout, freshBrowserTab(target.host, target.port, target.path));
    } else {
      layout = openBrowser(focusPane(layout, fromPane), target.host, target.port, target.path);
    }
    // The browser surface took focus: pull DOM focus off the terminal so
    // plain keys stop reaching a PTY that is no longer the focused view.
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** The Mod2+B chord / a manual open: a blank browser pane (address entry). */
  function newBrowserSurface(): void {
    layout = openTab(layout, freshBrowserTab("", 0, "/"));
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
   * Open a file surfaced FROM a pane (terminal/chat link, Finder row, session-
   * changes row, touched-files popover): an already-open tab is focused wherever
   * it lives (reuse existing, no duplicates); otherwise it lands in the ACTIVE
   * (focused) pane — the same "opens in the pane you're in" rule the FILES tree
   * and quick-open already follow. Cmd/Ctrl (newSplit) forces a fresh split to
   * the right instead, the escape hatch when you want it beside the source.
   */
  function openFileFromPane(paneId: string, path: string, newSplit: boolean, pinned = false): void {
    const existing = paneForTab(layout.root, { surface: "file", path });
    if (existing !== null) {
      layout = activateTab(layout, existing.paneId, existing.index);
    } else if (newSplit) {
      layout = splitPane(layout, paneId, "row");
      layout = openFile(layout, path, !pinned);
    } else {
      // Open in the active pane (the source pane the click just focused). Opens
      // are PREVIEW tabs (italic, replaced by the next preview open) unless the
      // caller pinned them (a tree double-click, a created file).
      layout = openFile(focusPane(layout, paneId), path, !pinned);
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

  /** The "N files changed" chip: open (or focus) this session's changes review,
   *  beside the source pane — adjacent pane, or a split when it stands alone. */
  function openChangesFromPane(paneId: string, sessionId: string, newSplit: boolean): void {
    const existing = paneForTab(layout.root, { surface: "changes", sessionId });
    if (existing !== null) {
      layout = activateTab(layout, existing.paneId, existing.index);
    } else {
      const neighbor = newSplit ? null : adjacentPane(layout, paneId);
      if (neighbor !== null) {
        layout = openChanges(focusPane(layout, neighbor), sessionId);
      } else {
        layout = splitPane(layout, paneId, "row");
        layout = openChanges(layout, sessionId);
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

  // An out-of-band deletion has no trustworthy rename destination, but it does
  // have exact absence. Close file/diff tabs under it and retarget Finders just
  // like an in-app delete; recreation can be opened as a fresh view later.
  let lastDiskChangeSeq = 0;
  $effect(() => {
    const change = $lastDiskChange;
    const dirty = $dirtyFiles;
    untrack(() => {
      if (change === null || change.seq === lastDiskChangeSeq) return;
      lastDiskChangeSeq = change.seq;
      for (const path of [...change.removed, ...change.removedDirs]) {
        // Unlike a confirmed in-app delete, an external delete must not throw
        // away an editor buffer the user has not saved. Clean tabs still close
        // and Finders still retreat; the retained CodeView shows the missing
        // file as a conflict and can recreate it with overwrite.
        layout = pruneDeletedPath(layout, path, dirty);
      }
    });
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
    if (!matches(raw) && wsAtBoot !== null) {
      // No layout for THIS window yet (a reopened/fresh window): fall back to
      // this workspace's last-active layout so a reopen restores, not resets.
      raw = await Promise.race([loadViewState(wsKey(wsAtBoot)), timeout]);
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
   * Bindings and the base modifier come from the keys.* settings (keys.ts
   * registry + keybindings.ts); the terminal owns bare Ctrl on every
   * platform (tmux Ctrl+B, EOF Ctrl+D, zsh/vim Ctrl+O all reach the PTY
   * untouched). Only the reference chord, Mod+1–9, and the font chords are
   * spec-pinned. Cmd+W/Cmd+T never reach a browser page (the native app
   * menu carries them instead).
   */
  function onKeydown(e: KeyboardEvent): void {
    // A settings row is recording a chord — the press is the recorder's.
    if (isCapturing()) return;
    // Per-pane text size (Cmd/Ctrl +/−/0, spec-pinned chords): intercepted
    // ONLY while the focused pane shows a font-sizable surface (a terminal or
    // a rendered markdown document), so browser zoom keeps working elsewhere.
    if (!pickerOpen && !quickOpenOpen && layoutReady) {
      const step = fontChord(e);
      // The dashboard owns the base-modifier Digit0 chord (the advertised
      // ⌘0 / Ctrl+Shift+0): a font RESET here would swallow it on every
      // terminal/markdown pane — the most common focus state. Font reset
      // keeps its other spellings (⌘Numpad0; plain Ctrl+0 on non-mac,
      // where the dashboard chord carries Shift).
      const dashboardChord = step === 0 && chordDigit(e, modifierSetting()) === 0;
      if (step !== null && !dashboardChord) {
        const p = findPane(layout.root, layout.focusedPaneId);
        const active = p?.tabs[p.active];
        const sizable =
          active !== undefined &&
          ((active.surface === "terminal" &&
            sessionsById.get(active.sessionId)?.ui !== "chat") ||
            (active.surface === "file" && viewKindFor(active.path) === "markdown"));
        if (p !== null && sizable) {
          e.preventDefault();
          e.stopPropagation();
          adjustFont(p.id, step);
          return;
        }
      }
    }
    const intercept = () => {
      e.preventDefault();
      e.stopPropagation();
    };

    // Every rebindable action resolves through the keybindings store; the
    // pinned chords (reference, Mod+1–9) are matched below on a miss.
    const hit = matchAction(e);

    // The folder picker toggles even while open; while it owns the keyboard
    // every other chord stands down.
    if (hit?.id === "picker") {
      intercept();
      if (pickerOpen) closePicker();
      else openPicker();
      return;
    }
    if (pickerOpen) return;
    if (activeWsId === null || !layoutReady) return;

    if (hit?.id === "quickOpen") {
      intercept();
      if (quickOpenOpen) closeQuickOpen();
      else openQuickOpen();
      return;
    }
    if (quickOpenOpen) return;

    if (hit === null) {
      // Context bridge: reference the current selection in the target agent.
      // Spec-pinned chord — ⇧⌘R / Ctrl+Shift+R. Intercepts only while a
      // selection exists, so the browser's reload chord survives when there
      // is nothing to reference. Plain Cmd+C is never touched.
      if (matchChord(e, REFERENCE_CHORD) !== null) {
        if (get(activeSelection) !== null) {
          intercept();
          referenceSelection();
        }
        return;
      }
      // Pinned Mod+1–9: open the Nth rail session; Mod+0 is the dashboard.
      const n = chordDigit(e, modifierSetting());
      if (n === 0) {
        intercept();
        openDashboardSurface();
      } else if (n !== null && n <= railSessions.length) {
        intercept();
        openSess(railSessions[n - 1].id);
      }
      return;
    }

    // Arrow chords in an editable surface belong to the text caret (rename
    // fields, search boxes, the file editor) — xterm's hidden textarea is
    // exempt, terminals don't use modifier-arrows for editing.
    if (hit.dir !== null && isEditableTarget(e.target)) return;

    switch (hit.id) {
      case "settings":
        intercept();
        openSettingsSurface();
        return;
      case "newTerminal":
        intercept();
        newShell();
        return;
      case "newAgent":
        // Does what the split button's main surface does — spawn the
        // persisted default agent, or install it when it's missing.
        intercept();
        newAgentPrimary();
        return;
      case "newBrowser":
        intercept();
        newBrowserSurface();
        return;
      case "splitRight":
        intercept();
        split("row");
        return;
      case "splitDown":
        intercept();
        split("col");
        return;
      case "zoom":
        intercept();
        layout = toggleZoom(layout);
        return;
      case "closeView":
        intercept();
        closeView(layout.focusedPaneId);
        return;
      case "focusMode":
        intercept();
        layout = { ...layout, focusMode: !layout.focusMode };
        return;
      case "cyclePrev":
        intercept();
        cycle(-1);
        return;
      case "cycleNext":
        intercept();
        cycle(1);
        return;
      case "focusArrows":
        intercept();
        focusDirection(hit.dir as FocusDir);
        return;
      case "moveTab":
        intercept();
        layout = moveTabDirection(layout, hit.dir as FocusDir);
        return;
    }
  }

  /**
   * Focus sits in a text-editing surface (inputs, the CodeMirror editor,
   * contenteditable) — arrow chords must stay with the caret there. xterm's
   * helper textarea is NOT editable in this sense: it's the terminal's key
   * sink, and app chords are expected to work over a focused terminal.
   */
  function isEditableTarget(t: EventTarget | null): boolean {
    if (!(t instanceof HTMLElement)) return false;
    if (t.classList.contains("xterm-helper-textarea")) return false;
    if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement) return true;
    return t.isContentEditable || t.closest(".cm-content") !== null;
  }

  $effect(() => {
    // The workspace leads; a remote window wears its host so a wall of similar
    // windows is legible:
    //   "crc_finish •Sherlock | chimaera"  (remote, in a workspace)
    //   "crc_finish | chimaera"            (local — the host is implicit)
    // Home (no workspace) drops the workspace but keeps the host when remote.
    // A compute-node daemon (the snapshot's `self` — daemon truth, not the
    // URL hash) appends its node: "crc_finish •Sherlock › sh02-02n44 | …",
    // so a job window never poses as its login node.
    const node = $computeStatus?.self?.node;
    const hostWithNode = node ? `${hostAlias} › ${node}` : hostAlias;
    const host = isRemoteWindow ? hostWithNode : null;
    const scope = workspace
      ? host
        ? `${workspace.name} •${host}`
        : workspace.name
      : (host ?? "");
    const base = scope ? `${scope} | chimaera` : "chimaera";
    const title = needsYou > 0 ? `(${needsYou}) ${base}` : base;
    document.title = title;
    // The native window title doesn't follow document.title — push it
    // explicitly. Overlay windows hide the text but keep this OS metadata.
    setNativeWindowTitle(title);
  });

  /**
   * Refresh the workspace list; if the tab's stored workspace no longer
   * exists on the daemon, clear it and fall back to the empty state.
   * Resolves once the fresh list is applied (never rejects) — the dashboard's
   * Mastermind dock awaits it after a PUT/DELETE to pick up the binding.
   */
  function refreshWorkspaces(): Promise<void> {
    return listWorkspaces()
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

  // A file that became dirty must never be a PREVIEW tab: an unsaved edit
  // that the next preview open silently replaced would be lost. Promote any
  // dirty preview tab to a permanent one. untrack() so writing `layout` here
  // doesn't loop with the read; pinPaths returns the same reference when
  // nothing matched, so this is inert until an edit actually lands.
  $effect(() => {
    const dirty = $dirtyFiles;
    untrack(() => {
      layout = pinPaths(layout, dirty);
    });
  });

  /** Open workspace `w` in THIS window — the launcher click, the folder
   *  picker's "open here", and worktree-session reveal all mean "here". A
   *  workspace already open in another window is deliberately NOT diverted to:
   *  the daemon owns the sessions, so a second view onto the same workspace is
   *  cheap and independent (each window keys its own view state), and
   *  diverting a "new window, then pick a workspace" to some *other* window —
   *  leaving the fresh one blank on the launcher — was the reported bug.
   *  "New window" and Cmd/Ctrl-click stay the explicit "another window"
   *  gestures. */
  async function activateWorkspace(w: Workspace): Promise<void> {
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

  /**
   * Last-known non-zero created_at per session id. The daemon's mid-switch
   * placeholder row carries created_at:0 (a sentinel); sorting by it verbatim
   * would teleport a switching session to the rail top and renumber every
   * ⌘1–9 chord for the switch's duration. Substituting its last-known
   * created_at keeps the row in place. Pruned to live ids each snapshot.
   */
  const lastCreatedAt = new Map<string, number>();

  function applySessions(list: Session[]): void {
    // A session being optimistically killed is tombstoned: never re-add it
    // from an intervening snapshot (the server hasn't finished the stop yet),
    // so the row can't flicker back before it's really gone.
    if (killing.size > 0) list = list.filter((s) => !killing.has(s.id));
    for (const s of list) {
      if (s.created_at !== 0) lastCreatedAt.set(s.id, s.created_at);
    }
    const ids = new Set(list.map((s) => s.id));
    for (const id of lastCreatedAt.keys()) if (!ids.has(id)) lastCreatedAt.delete(id);
    const sortKey = (s: Session): number =>
      s.created_at !== 0 ? s.created_at : (lastCreatedAt.get(s.id) ?? 0);
    list.sort((a, b) => sortKey(a) - sortKey(b) || a.id.localeCompare(b.id));
    // Unread marks fold BEFORE the swap: the transitions ("was running, now
    // finished") need the previous rows. The focused session is exempt —
    // output that ended under the user's eyes was seen.
    foldUnread(new Map(sessions.map((s) => [s.id, s])), list, focusedSessionId);
    sessions = list;
    gotSessions = true;
    // A session now running as chat has no PTY: dispose any warm terminal
    // for it in THIS window (every window sees the flip on the bus), so a
    // later toggle back to terminal mounts fresh instead of replaying a
    // dead socket's screen. Symmetrically, a session now running as a TUI has
    // no chat driver — drop its pooled chat socket (it flagged `ended` on the
    // degrade frame and stopped reconnecting; it's dead weight).
    for (const s of list) {
      if (s.ui === "chat") pool.disposeSession(s.id);
      else if (s.ui === "term") chatPool.disposeChat(s.id);
    }
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
    if (changed) refreshRecents();
    pruneAndAutoOpen();
  }

  /**
   * Sessions created by this window in the last few seconds. A sessions
   * snapshot fetched BEFORE the create but arriving AFTER it would otherwise
   * prune the fresh tab right out of the layout (stale-poll race).
   */
  const recentlyCreated = new Map<string, number>();
  const RECENT_MS = 10_000;
  /** Sessions being optimistically killed: dropped locally at once, tombstoned
   *  so an in-flight snapshot can't re-add the dying row before the daemon's
   *  stop completes. Cleared when the kill request resolves. */
  const killing = new Set<string>();

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
    // The one-shot waits for the settings to actually load: getSetting
    // returns the schema default ("auto") until GET /settings resolves, and
    // the REST fallback poll can deliver sessions first — latching here on
    // the default would override an explicit "never". loadSettings flips
    // loaded even on failure (defaults then genuinely apply) and re-calls
    // this, so the gate can never wedge the landing.
    if (!autoOpened && settingsLoaded()) {
      autoOpened = true;
      if (tabCount(layout) === 0) {
        // An empty layout lands on the dashboard — the workspace overview —
        // unless the setting restores the old first-session behavior. A
        // NON-empty restored layout is never touched: the dashboard earns
        // the center only when the stage was empty.
        if (getSetting("dashboard.landing") !== "never") {
          layout = openDashboard(layout);
        } else if (railSessions.length > 0) {
          layout = openSession(layout, railSessions[0].id);
        }
      }
    }
  }

  function onTitle(id: string, title: string): void {
    const s = sessions.find((x) => x.id === id);
    if (s) s.title = title;
  }

  function onExited(id: string, _status: number | null): void {
    // An agent PTY dying may BE the chat⇄terminal toggle doing its job, so we
    // can't just drop the row. The events bus (which carries the mid-switch
    // placeholder) is authoritative — but with /ws/events down a genuine exit
    // would linger up to the poll interval, so reconcile immediately from the
    // roster instead. A real exit drops the row; a toggle's snapshot still
    // carries the placeholder. Shells vanish instantly, tmux-style.
    if (sessions.find((s) => s.id === id)?.kind === "agent") {
      void listSessions()
        .then(applySessions)
        .catch(() => {
          // unreachable daemon; the events socket / poll reconciles later
        });
      return;
    }
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

  /**
   * Focus a session that may live in another workspace (a worktree branch). If
   * it is in the active workspace, open it here; otherwise switch to its
   * workspace (fetching it when a just-created worktree isn't in the list yet)
   * — the incoming workspace auto-opens its freshly-spawned session, and a
   * workspace with saved layout restores the session's tab.
   */
  async function revealWorktreeSession(sessionId: string, workspaceId: string): Promise<void> {
    if (workspaceId === activeWsId) {
      openSess(sessionId);
      return;
    }
    let target = workspaces.find((w) => w.id === workspaceId);
    if (target === undefined) {
      const list = await listWorkspaces().catch(() => null);
      if (list !== null) {
        workspaces = list;
        target = list.find((w) => w.id === workspaceId);
      }
    }
    if (target !== undefined) {
      activateWorkspace(target);
      // The session arrives async after the switch, and the incoming layout
      // boots async too — so defer the open to when the layout is ready rather
      // than racing bootViewState (which would clobber an eager openSession).
      pendingReveal = sessionId;
    }
  }

  // Focus a pending session once the incoming workspace's layout has booted.
  let pendingReveal: string | null = null;
  $effect(() => {
    if (layoutReady && pendingReveal !== null) {
      const id = pendingReveal;
      pendingReveal = null;
      untrack(() => openSess(id));
    }
  });

  /** Open/focus a file tab (FILES tree / quick-open). A single click opens a
   *  PREVIEW tab (italic, reused by the next preview open); `pinned` (a tree
   *  double-click, a just-created file) makes it a permanent tab. */
  function openFilePath(path: string, pinned = false): void {
    // Guard unsaved edits: pin any dirty preview tab before a replace can drop
    // it (belt-and-braces with the dirty-edit promotion effect).
    layout = pinPaths(layout, get(dirtyFiles));
    layout = openFile(layout, path, !pinned);
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

  /** Open/focus the workspace dashboard (rail row, ⌘0, the landing default). */
  function openDashboardSurface(): void {
    if (activeWsId === null || !layoutReady) return;
    layout = openDashboard(layout);
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
   * Step (or reset, delta 0) a pane's content font size — the chords and
   * the pane-bar A−/A+ controls both land here; persisted with the layout.
   * An unset override starts from the active surface's own preference, so a
   * Markdown increase never jumps to the unrelated terminal baseline.
   */
  function adjustFont(paneId: string, delta: 1 | -1 | 0): void {
    const p = findPane(layout.root, paneId);
    if (p === null) return;
    if (delta === 0) {
      layout = setPaneFont(layout, paneId, undefined);
      return;
    }
    const active = p.tabs[p.active];
    const base =
      active?.surface === "file" && viewKindFor(active.path) === "markdown"
        ? getSetting("editor.markdownFontSize")
        : pool.baseFontSize();
    layout = setPaneFont(layout, paneId, (p.fontSize ?? base) + delta);
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
      // Route a would-be chat spawn straight to the terminal when the catalog
      // knows this agent's CLI is too old to chat (skips the handshake-watchdog
      // detour); an unknown/unloaded catalog trusts the default view.
      const chatCapable =
        kind === "agent"
          ? agents?.find((a) => a.id === (spawn.agent ?? "claude"))?.chatCapable
          : undefined;
      const s = await createSession(activeWsId, kind, null, spawnSize(), spawn, chatCapable);
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

  /** Every launcher selection becomes the new default agent and spawns. An
   *  EXPLICIT surface pick (the "open"/terminal buttons, ⌘↵) also becomes the
   *  sticky default view, so the split button and the next plain row press
   *  follow it until the user picks the other surface again. */
  function launcherPick(pick: LaunchPick): void {
    launcherOpen = false;
    agentDefault = { agent: pick.agent };
    setAgentDefault(agentDefault);
    if (pick.explicit === true && pick.ui !== undefined) {
      // The setting's vocabulary is "terminal"; the wire/pick term is "term".
      setSetting("agents.defaultView", pick.ui === "term" ? "terminal" : "chat");
    }
    void spawnSession("agent", pick);
  }

  /** Install/update flow (managed runtimes): the daemon builds its own
   *  curated command — official artifacts only, into ~/.chimaera/agents —
   *  and runs it as an ordinary shell session here. The affordance's tooltip
   *  said exactly what runs; this one explicit click executes it, and the
   *  session opens like any other so the output streams in a visible pane. */
  function launcherInstall(a: AgentInfo): void {
    managedRuntimeFlow(a, installAgent, "install");
  }

  /** Update, same trusted shape: the daemon re-runs its curated script
   *  (fetch latest, verify, atomic symlink re-swap) in a visible
   *  "update <agent>" session. Managed binaries only — the launcher and
   *  settings never offer this for the user's own install. */
  function launcherUpdate(a: AgentInfo): void {
    managedRuntimeFlow(a, updateAgent, "update");
  }

  function managedRuntimeFlow(
    a: AgentInfo,
    run: (agentId: string, workspaceId: string) => Promise<string>,
    verb: "install" | "update",
  ): void {
    launcherOpen = false;
    const ws = activeWsId;
    if (ws === null) {
      openPicker();
      return;
    }
    createError = null;
    void run(a.id, ws)
      .then(async (sessionId) => {
        recentlyCreated.set(sessionId, Date.now());
        // Watch this session: when it exits, re-probe the catalog so the
        // split button / rows reflect the new binary.
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
        // agent, 409 an install/update for it is already running).
        createError =
          e instanceof ApiError ? e.message : `failed to start the ${a.name} ${verb}`;
      });
  }

  // --- the rail's Recents section ---

  /** Monotone fetch counter: a slow early /recents response must not
   *  clobber a newer one. */
  let recentsSeq = 0;

  /** Reload the workspace's ended agent conversations. Retire timing needs
   *  no guessing: the daemon pushes a `recents` epoch frame the moment a
   *  conversation lands in the store, and this refetches on it. This path
   *  also runs on agent-set changes — a conversation resumed into a live
   *  session must drop off the list even though the store didn't change. */
  function refreshRecents(): void {
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
  }

  $effect(() => {
    void activeWsId;
    recentsExpanded = false;
    refreshRecents();
  });

  /** A Recents row: resume when the daemon captured a native conversation
   *  handle, else an honest fresh start (the tooltip says why). The row's
   *  title rides along so the restored conversation keeps its name instead of
   *  showing a bare "claude" until a new turn regenerates one. */
  function openRecent(r: RecentConvo): void {
    const titleHint = r.title !== "" ? r.title : undefined;
    // Reopen in the SURFACE it last ran on (TUI vs chat). Null (old entries,
    // scanned transcripts) leaves `ui` undefined so createSession falls back
    // to the launcher's sticky default. createSession's own guards
    // (claude/codex-only + chatCapable) keep a "chat" row honest.
    const ui = r.ui ?? undefined;
    void spawnSession(
      "agent",
      r.resume !== null
        ? { agent: r.kind, resume: r.resume, titleHint, ui }
        : { agent: r.kind, titleHint, ui },
    );
  }

  function recentTooltip(r: RecentConvo): string {
    return r.resume !== null
      ? `resume “${r.title}”`
      : `no saved conversation handle was recorded — starts a fresh ${r.kind} session`;
  }

  /** Kill the session's process on the daemon and drop it locally — OPTIMISTIC:
   *  the row/tab vanishes at once and the ChatView tears down immediately,
   *  while the daemon's stop + retire-to-recents runs in the background. The
   *  tombstone keeps an in-flight snapshot from re-adding the dying row, so
   *  there's no lingering half-dead state, and the recents entry appears once
   *  (via the recents epoch) instead of the row "popping up weirdly". */
  async function killSession(id: string): Promise<void> {
    confirmKillId = null;
    killing.add(id);
    chatPool.disposeChat(id); // stop its socket reconnecting right away
    applySessions(sessions.filter((s) => s.id !== id));
    try {
      await deleteSession(id);
    } catch {
      // already gone or unreachable — it's already dropped locally.
    } finally {
      killing.delete(id);
    }
  }

  /** End every live session in a workspace — the home-screen "stop". The
   *  workspace registration itself is untouched; only its running work ends. */
  async function stopWorkspace(w: Workspace): Promise<void> {
    const live = sessions.filter((s) => s.workspace_id === w.id && s.alive);
    if (live.length === 0) return;
    await Promise.allSettled(live.map((s) => deleteSession(s.id)));
    const killed = new Set(live.map((s) => s.id));
    applySessions(sessions.filter((s) => !killed.has(s.id)));
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

  /** Rail-header "new file"/"new folder": an inline-create row in the tree,
   *  targeting the workspace root. */
  function requestTreeCreate(kind: "file" | "dir"): void {
    filesOpen = true;
    treeCreateNonce += 1;
    treeCreate = { kind, nonce: treeCreateNonce };
  }

  /** The confirmed file-manager delete: on failure the dialog stays open
   *  with the error inline; on success fsEvents refreshes every surface and
   *  the mutation subscription below closes tabs under the path. */
  function confirmDelete(): void {
    const pending = $pendingDelete;
    if (pending === null) return;
    deleteError = null;
    fsDeleteOp(pending.path)
      .then(() => pendingDelete.set(null))
      .catch((err) => {
        deleteError = err instanceof Error ? err.message : "delete failed";
      });
  }

  // Keep open tabs coherent with fs mutations from ANY surface (tree, Finder,
  // tab rename): a rename rewrites file/diff/finder tab paths, a delete
  // closes tabs under the path (finders retarget to the parent).
  let lastFsMutationSeq = 0;
  $effect(() => {
    const m = $lastFsMutation;
    untrack(() => {
      if (m === null || m.seq === lastFsMutationSeq) return;
      lastFsMutationSeq = m.seq;
      if (m.kind === "rename") layout = rewriteTabPaths(layout, m.from, m.to);
      else if (m.kind === "delete") layout = pruneDeletedPath(layout, m.path);
    });
  });

  /** The × on a rail row: only a session doing ACTIVE work (a running command
   *  or an agent mid-turn) gets an inline confirm — an idle/empty shell or a
   *  waiting/finished/not-running agent closes straight away. (A hook-less TUI
   *  reads as not-busy until its activity signal lands — a separate change.) */
  function requestKill(s: Session): void {
    if (isBusy(s)) {
      confirmKillId = s.id;
    } else {
      void killSession(s.id);
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
    dragPane(e, paneId) {
      beginPaneDrag(e, paneId);
    },
    pinTab(paneId, index) {
      layout = pinTab(layout, paneId, index);
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
    openPathFrom(paneId, path, kind, newSplit) {
      if (kind === "dir") {
        openDirInFinder(paneId, path, newSplit);
        revealInTree(path);
      } else {
        openFileFromPane(paneId, path, newSplit);
      }
    },
    openChangesFrom(paneId, sessionId, newSplit) {
      openChangesFromPane(paneId, sessionId, newSplit);
    },
    revealPathInTree(path) {
      revealInTree(path);
    },
    navigateFinder(id, path) {
      layout = setFinderPath(layout, id, path);
    },
    navigateBrowser(id, path) {
      layout = setBrowserPath(layout, id, path);
    },
    retargetBrowser(id, host, port, path) {
      layout = setBrowserTarget(layout, id, host, port, path);
    },
    openDiffFrom(paneId, path, mode, newSplit) {
      openDiffFromPane(paneId, path, mode, newSplit);
    },
    revealWorktreeSession(sessionId, workspaceId) {
      void revealWorktreeSession(sessionId, workspaceId);
    },
    adjustFont(paneId, delta) {
      adjustFont(paneId, delta);
    },
    switchView(sessionId, target) {
      void switchView(sessionId, target);
    },
  };

  /**
   * The chat⇄terminal toggle: the daemon stops the current process and
   * resumes the same conversation in the other mode; the session row's `ui`
   * flips on the events bus and every pane follows. A mid-task agent 409s
   * (busy) until the user confirms the interruption.
   *
   * Guarded against double-fire: the toggle button and its ⌘-chord both call
   * here, so a switch already in flight for this id is ignored, and the button
   * disables itself via the `switchingViews` store meanwhile. The server's own
   * concurrent-switch 409 (without `busy`) is the backstop, dropped silently.
   */
  async function switchView(sessionId: string, target: "chat" | "term"): Promise<void> {
    if (get(switchingViews).has(sessionId)) return;
    switchingViews.update((s) => new Set(s).add(sessionId));
    try {
      try {
        await switchSessionView(sessionId, target, false);
      } catch (e) {
        // A concurrent switch already owns this id (409 without busy): the
        // duplicate is a no-op, no toast, no confirm.
        if (e instanceof ViewSwitchConflict && !e.busy) return;
        if (!(e instanceof ViewSwitchConflict)) {
          console.error("view switch failed", e);
          return;
        }
        const go = confirm(
          "The agent is mid-task. Switching restarts it via resume and interrupts the current turn — switch anyway?",
        );
        if (!go) return;
        try {
          await switchSessionView(sessionId, target, true);
        } catch (err) {
          console.error("forced view switch failed", err);
          return;
        }
      }
      // Success. Flip the local row's `ui` optimistically in the same tick, so
      // the pane swaps to the new surface immediately (ChatView mounts now,
      // instead of blanking until the events bus confirms) — then dispose the
      // pooled xterm, whose screen is a dead PTY once chat owns the id.
      const s = sessions.find((x) => x.id === sessionId);
      if (s !== undefined) s.ui = target;
      if (target === "chat") pool.disposeSession(sessionId);
      else chatPool.disposeChat(sessionId); // the chat driver is gone now
    } finally {
      switchingViews.update((s) => {
        const next = new Set(s);
        next.delete(sessionId);
        return next;
      });
    }
  }

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
    // Arm the bottom bands for this drag: reference targets for path drags
    // (file previews and Finder/dir payloads), link targets for shell-
    // terminal drags. Drives the partitioned zone previews (the band region
    // is reserved, never flashed over).
    const refPath = tab.surface === "file" || tab.surface === "finder" ? tab.path : undefined;
    const armed = new Set<string>();
    const linkTargets = linkTargetsFor(tab);
    const linkSessions = linkSessionsFor(tab);
    if (linkTargets !== undefined) for (const id of linkTargets.keys()) armed.add(id);
    if (refPath !== undefined) {
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
      { tab, label, refPath },
      {
        onSpot: (s) => (dropSpot = s),
        onDrop: (spot) => {
          if (spot.kind === "ref") {
            // Drag-to-reference: type into the session, never open a tab.
            if (tab.surface === "file") referenceFileDrop(spot.paneId, tab.path, "file");
            else if (tab.surface === "finder") referenceFileDrop(spot.paneId, tab.path, "dir");
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
          // `upload` never reaches this pointer-drag callback (it is only ever
          // set on dropSpot by App's OS-drop handlers), so the zone arm is
          // explicit and the impossible case is a no-op.
          layout =
            spot.kind === "tab"
              ? moveTabToIndex(layout, tab, spot.paneId, spot.index)
              : spot.kind === "edge"
                ? dropTabAtRootEdge(layout, tab, spot.edge)
                : spot.kind === "zone"
                  ? dropTab(layout, tab, spot.paneId, spot.zone)
                  : layout;
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

  /** The pane grip's whole-pane drag: move the ENTIRE pane (all tabs) to
   *  another split position, reusing the tab-drag drop zones. A plain click
   *  focuses the pane. No refPath/link arming — only tab/edge/zone spots fire,
   *  and any spot targeting the dragged pane itself is suppressed. */
  function beginPaneDrag(e: PointerEvent, paneId: string): void {
    const pane = findPane(layout.root, paneId);
    if (pane === null || panesOf(layout.root).length < 2) return; // last pane never moves
    const active = pane.tabs[pane.active];
    const base = active !== undefined ? tabLabel(active) : "pane";
    const extra = pane.tabs.length > 1 ? ` +${pane.tabs.length - 1}` : "";
    startDrag(
      e,
      { label: `${base}${extra}` },
      {
        onSpot: (s) => {
          // Never advertise a drop onto the pane being dragged (self-move).
          dropSpot = s !== null && "paneId" in s && s.paneId === paneId ? null : s;
        },
        onDrop: (spot) => {
          if ("paneId" in spot && spot.paneId === paneId) return;
          layout =
            spot.kind === "tab"
              ? movePaneToIndex(layout, paneId, spot.paneId, spot.index)
              : spot.kind === "edge"
                ? movePaneToRootEdge(layout, paneId, spot.edge)
                : spot.kind === "zone"
                  ? movePane(layout, paneId, spot.paneId, spot.zone)
                  : layout;
          const sid = focusedSessionOf(layout);
          if (sid !== null) pool.focusTerminal(sid);
        },
        onClick: () => ctrl.focusPane(paneId),
        onEnd: () => {
          dropSpot = null;
          bandPanes = new Set();
        },
      },
    );
  }

  /** A tab's display label (shared by tab and whole-pane drags). */
  function tabLabel(tab: Tab): string {
    return tab.surface === "terminal"
      ? (displayNames.get(tab.sessionId) ?? sessionsById.get(tab.sessionId)?.name ?? tab.sessionId.slice(0, 8))
      : tab.surface === "file"
        ? (fileTitles.get(tab.path) ?? basename(tab.path))
        : tab.surface === "finder"
          ? (basename(tab.path) || "Finder")
          : tab.surface === "diff"
            ? `${basename(tab.path)} (diff)`
            : tab.surface === "git"
              ? "Source Control"
              : tab.surface === "changes"
                ? "Changes"
                : "Settings";
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

  /**
   * FILES tree entries drag exactly like rail rows (surface parity). Files
   * drag as file tabs; dirs drag as FRESH Finder tabs, so a zone/tab drop
   * opens a legitimate browsing surface (never a broken file preview) while
   * the "@ reference" band accepts both. The sub-threshold click action
   * (open / expand) comes from the tree, which owns its expansion state.
   */
  function onTreeEntryDown(
    e: PointerEvent,
    path: string,
    kind: "file" | "dir",
    onEntryClick: () => void,
  ): void {
    const tab: Tab = kind === "dir" ? freshFinderTab(path) : { surface: "file", path };
    beginDrag(e, tab, onEntryClick);
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

<div class="shell" class:native-titlebar-overlay={nativeTitlebarOverlay}>
  {#if nativeTitlebarOverlay && (activeWsId === null || !layout.focusMode)}
    <!-- Overlay chrome keeps the native traffic lights but needs an explicit
         drag target. It is control-height over the rail/home's reserved blank
         corner. Focus mode uses the context strip's own full-height drag
         target instead, so pane tabs never collide with native chrome. Browser
         windows never render either target. -->
    <div
      class="native-drag-region"
      style:right={activeWsId !== null
        ? `calc(100% - ${railWidth}px)`
        : "48px"}
      data-tauri-drag-region
      aria-hidden="true"
    ></div>
  {/if}
  {#if activeWsId === null}
    <!-- Home: a real launcher, not an empty IDE. The rail and stage only
         exist once a workspace scopes this window. A Mastermind is never a
         worker: keep it out of the per-workspace live/attention rollups. -->
    <HomeScreen
      {workspaces}
      sessions={sessions.filter((s) => !isMastermind(s))}
      hostLabel={getHostLabel()}
      {health}
      connected={eventsUp}
      onOpen={activateWorkspace}
      onRemove={removeWorkspace}
      onStop={stopWorkspace}
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
          title="hide sidebar ({keyHint("focusMode")})"
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
              class:unread={renamingId !== s.id && isUnread(s.id)}
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
              oncontextmenu={(e) =>
                contextMenu.openAt(e, [
                  { label: "Rename…", onSelect: () => startRename(s) },
                ])}
            >
              <!-- Session-type glyph carrying the state color — the same
                   mark as the pane tab (surface parity, rail included). It
                   breathes while alive (pulse), and muted while background
                   work runs past the turn: the rail's activity cue, since the
                   row shows only the glyph, no separate dot. -->
              <SessionGlyph
                kind={s.kind}
                agentKind={s.agent_kind}
                state={dotState(s)}
                size={11}
                title={dotTitle(s)}
                pulse
                backgrounded={backgrounded(s)}
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
                       own PTY title as context when it differs from the name;
                       chat agents (no PTY title) show the agent's own
                       post-turn status line instead. -->
                  {#if s.kind === "agent" && s.title && s.title !== displayName(s) && s.title !== s.agent_title}
                    <span class="title">{s.title}</span>
                  {:else if s.kind === "agent" && s.status_detail && s.status_detail !== displayName(s)}
                    <span class="title" title={s.status_detail}>{s.status_detail}</span>
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

        <!-- The workspace dashboard: the fixed home row above the sessions —
             the overview of everything below it. ⌘0, matching the ⌘1–9 rows. -->
        <button
          class="row dash-row"
          class:dash-active={dashboardOpen}
          title="workspace dashboard ({activeModLabel()}0)"
          onclick={openDashboardSurface}
        >
          <svg class="dash-glyph" viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <path
              d="M8 1.8l5.4 3.1v6.2L8 14.2l-5.4-3.1V4.9z"
              fill="none"
              stroke="currentColor"
              stroke-width="1.4"
              stroke-linejoin="round"
            />
          </svg>
          <span class="dash-label">dashboard</span>
          {#if hintsActive()}
            <span class="kbd-badge" aria-hidden="true">0</span>
          {/if}
        </button>

        <!-- Terminals first (there are few), agents below (there are many);
             this order is also the mod+1–9 order and the strip order. -->
        <div class="rail-sec">terminals</div>
        {#each shellSessions as s (s.id)}
          {@render sessionRow(s)}
        {/each}
        <button
          class="row new"
          title="open a terminal ({keyHint("newTerminal")})"
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
              : `start ${agentDefault.agent} (${keyHint("newAgent")})`}
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
             first — the daemon remembers them across restarts. Click resumes
             when a native handle exists, otherwise starts fresh honestly. -->
        {#if visibleRecents.length > 0}
          <div class="recents">
            <div class="recents-head">recent</div>
            <div class="recents-list" class:expanded={recentsExpanded}>
              {#each recentsExpanded ? visibleRecents : visibleRecents.slice(0, 3) as r (r.resume ?? `${r.kind}:${r.title}`)}
                <button class="recent-row" title={recentTooltip(r)} onclick={() => openRecent(r)}>
                  <SessionGlyph kind="agent" agentKind={r.kind} size={11} />
                  <span class="recent-title">{r.title}</span>
                  <span class="recent-age">{relativeAge(r.lastActive)}</span>
                </button>
              {/each}
            </div>
            {#if visibleRecents.length > 3}
              <button
                class="recents-more"
                onclick={() => (recentsExpanded = !recentsExpanded)}
              >
                {recentsExpanded ? "show less" : `all ${visibleRecents.length}`}
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
              title="new file in the workspace root"
              aria-label="new file"
              onclick={() => requestTreeCreate("file")}
            >
              <!-- A file glyph with a plus. -->
              <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
                <path d="M9.5 2H4.5A1.2 1.2 0 0 0 3.3 3.2v9.6A1.2 1.2 0 0 0 4.5 14H8" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
                <path d="M9.5 2 12.7 5.2V8" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" />
                <line x1="11.8" y1="10.2" x2="11.8" y2="14.2" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
                <line x1="9.8" y1="12.2" x2="13.8" y2="12.2" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
              </svg>
            </button>
            <button
              class="files-finder"
              title="new folder in the workspace root"
              aria-label="new folder"
              onclick={() => requestTreeCreate("dir")}
            >
              <!-- A folder glyph with a plus. -->
              <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
                <path d="M2.3 4.2A1.2 1.2 0 0 1 3.5 3h2.6l1.4 1.5h4.9a1.2 1.2 0 0 1 1.2 1.2V7.5M2.3 4.2v7.6A1.2 1.2 0 0 0 3.5 13H8" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
                <line x1="11.8" y1="9.7" x2="11.8" y2="13.7" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
                <line x1="9.8" y1="11.7" x2="13.8" y2="11.7" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
              </svg>
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
                onOpenPinned={(p) => openFilePath(p, true)}
                onDragStart={onTreeEntryDown}
                activePath={focusedFilePath}
                reveal={treeReveal}
                createRequest={treeCreate}
                dropActive={dropSpot?.kind === "uploadDir" && dropSpot.paneId === null}
              />
            </div>
          {/if}
        </section>
      {/if}

      {#if $computeStatus?.self}
        <!-- This whole window lives inside a Slurm allocation (the daemon's
             own truth, not the URL hash): countdown + resources, stacked
             ABOVE the daemon bar so neither crowds the other. -->
        <ComputeStrip self={$computeStatus.self} receivedAt={$computeStatus.received_at_ms} />
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
        <!-- Inside an allocation the label carries the node ("Sherlock ›
             sh02-02n44") so a compute-node window never poses as its login
             node — derived from the daemon's self block, hash-independent. -->
        <span class="daemon-host" class:remote={isRemoteWindow} title={health?.hostname}
          >{$computeStatus?.self
            ? `${getHostLabel()} › ${$computeStatus.self.node}`
            : getHostLabel()}</span
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
        {:else if $gitEnv?.ok === false}
          <!-- No repo shown because git itself can't run (too old / missing).
               One click opens source control, which explains the fix. -->
          <button
            class="daemon-git bad"
            onclick={openGitPanel}
            title={`git ${$gitEnv.version ? `${$gitEnv.version} is too old (need ≥ ${$gitEnv.min})` : "not found"} — open source control to fix`}
          >
            <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
              <path
                d="M8 1.8 14.6 13H1.4L8 1.8ZM8 6.4v3.1M8 11.2v.1"
                fill="none"
                stroke="currentColor"
                stroke-width="1.4"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
            <span class="dg-branch">git {$gitEnv.version ? "too old" : "missing"}</span>
          </button>
        {:else if $gitRepoError !== null}
          <!-- Git runs, but couldn't read this repo (dubious ownership, a
               permission/filesystem problem). Without this the diagnostic
               would be unreachable — there's no branch header to click. -->
          <button
            class="daemon-git bad"
            onclick={openGitPanel}
            title="git couldn’t read this repository — open source control to fix"
          >
            <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
              <path
                d="M8 1.8 14.6 13H1.4L8 1.8ZM8 6.4v3.1M8 11.2v.1"
                fill="none"
                stroke="currentColor"
                stroke-width="1.4"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
            <span class="dg-branch">can’t read repo</span>
          </button>
        {/if}
        {#if $computeStatus?.scheduler === "slurm" && !$computeStatus?.self}
          {@const queued = queuedJobCount($computeStatus)}
          <!-- Slurm orientation, indicator ONLY (maintainer, 2026-07-15):
               the scheduler exists here + how much of your work is queued.
               Queue browsing/management deliberately does NOT live in the
               rail — that arrives with the agent dashboard; launching onto
               compute nodes belongs to the home screen's Mode 2 flow.
               INSIDE an allocation the chip disappears entirely (maintainer,
               2026-07-16): the strip above this bar IS the compute truth of
               a job window, and a queue count is login-node orientation. -->
          <span
            class="daemon-compute"
            title={`slurm — ${queued} job${queued === 1 ? "" : "s"} in queue`}
          >
            <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
              <rect
                x="2"
                y="2"
                width="5"
                height="5"
                rx="1.2"
                fill="none"
                stroke="currentColor"
                stroke-width="1.4"
              />
              <rect
                x="9"
                y="2"
                width="5"
                height="5"
                rx="1.2"
                fill="none"
                stroke="currentColor"
                stroke-width="1.4"
              />
              <rect
                x="2"
                y="9"
                width="5"
                height="5"
                rx="1.2"
                fill="none"
                stroke="currentColor"
                stroke-width="1.4"
              />
              <rect
                x="9"
                y="9"
                width="5"
                height="5"
                rx="1.2"
                fill="none"
                stroke="currentColor"
                stroke-width="1.4"
              />
            </svg>
            {#if queued > 0}<span class="dc-count">{queued}</span>{/if}
          </span>
        {/if}
        {#if canCaffeinate}
          <button
            class="daemon-settings caffeinate"
            class:on={caffeinated}
            title={caffeinated
              ? "caffeinate on — this Mac won’t sleep (lid-closed needs AC power)"
              : "caffeinate — keep this Mac awake"}
            aria-label="caffeinate"
            aria-pressed={caffeinated}
            onclick={() => void toggleCaffeinate()}
          >
            <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
              <path
                d="M3 6.2h7.5v3.3a2.6 2.6 0 0 1-2.6 2.6H5.6A2.6 2.6 0 0 1 3 9.5V6.2z"
                fill="none"
                stroke="currentColor"
                stroke-width="1.3"
              />
              <path
                d="M10.5 7.1h1.3a1.6 1.6 0 0 1 0 3.2h-1.3"
                fill="none"
                stroke="currentColor"
                stroke-width="1.3"
              />
              <path
                d="M4.9 2.5c0 .8-.7.8-.7 1.6M7 2.5c0 .8-.7.8-.7 1.6M9.1 2.5c0 .8-.7.8-.7 1.6"
                fill="none"
                stroke="currentColor"
                stroke-width="1.1"
                stroke-linecap="round"
              />
            </svg>
          </button>
        {/if}
        {#if activeWsId !== null}
          <button
            class="daemon-settings"
            title="settings ({keyHint("settings")})"
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
            dash={dashCtx}
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
            soloPane={panesOf(layout.root).length === 1}
            dash={dashCtx}
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
        title="show sidebar ({keyHint("focusMode")})"
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
        title="show sidebar ({keyHint("focusMode")})"
        onclick={() => (layout = { ...layout, focusMode: false })}
        >{workspace?.name ?? "chimaera"}</button
      >
      <div class="chips">
        {#each railSessions as s (s.id)}
          <button
            class="chip"
            class:focused={s.id === focusedSessionId}
            class:unread={isUnread(s.id)}
            title={isUnread(s.id) ? "finished — output you haven't looked at" : (s.title ?? undefined)}
            onclick={() => openSess(s.id)}
          >
            <!-- The type glyph replaces both the dot and the old "$ "
                 prefix — same mark as tabs and rail rows (parity), breathing
                 while alive or working in the background. -->
            <SessionGlyph
              kind={s.kind}
              agentKind={s.agent_kind}
              state={dotState(s)}
              size={10}
              title={dotTitle(s)}
              pulse
              backgrounded={backgrounded(s)}
            />
            <span class="chip-name">{displayNames.get(s.id) ?? displayName(s)}</span>
            {#if hintsActive() && chordDigits.has(s.id)}
              <span class="chip-badge" aria-hidden="true">{chordDigits.get(s.id)}</span>
            {/if}
          </button>
        {/each}
      </div>
      {#if nativeTitlebarOverlay}
        <!-- Native focus mode promotes this strip to the titlebar. Keep a
             generous, control-height empty target so dragging never competes
             with the workspace/session buttons. -->
        <div class="strip-drag" data-tauri-drag-region aria-hidden="true"></div>
      {/if}
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
    initial={agents}
    onPick={launcherPick}
    onInstall={launcherInstall}
    onUpdate={launcherUpdate}
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

<!-- Blocking re-auth overlay: the daemon rejected this window's token
     (restart or expiry). Self-gating on the `unauthorized` store. -->
<ReauthOverlay enabled={!canReconnect} />

<!-- SSH auth prompt (password / 2FA), app-wide so a mid-session reconnect on
     the workbench can prompt just like the home screen. Self-gating. -->
<AskpassModal hostAlias={isRemoteWindow ? hostAlias : null} />
<!-- The right-click context-menu singleton (rail rows, file tree, Finder,
     pane tabs all open it via contextMenu.openAt). Self-gating. -->
<ContextMenuHost />

<!-- File-manager delete confirmation: permanent (no server-side trash), so
     always an explicit modal. A failure keeps it open with the error. -->
{#if $pendingDelete !== null}
  <ConfirmDialog
    title={$pendingDelete.kind === "dir" ? "Delete folder" : "Delete file"}
    body={`Permanently delete “${basename($pendingDelete.path)}”${
      $pendingDelete.kind === "dir" ? " and everything inside it" : ""
    }? This cannot be undone.`}
    confirmLabel="delete"
    danger
    error={deleteError}
    onConfirm={confirmDelete}
    onCancel={() => {
      pendingDelete.set(null);
      deleteError = null;
    }}
  />
{/if}

<!-- Ambient update offer (small, snoozable): a newer release, or a daemon
     older than this app. One per window; dismissals are origin-wide. -->
{#if $assetTransition !== null}
  <AssetTransitionNotice
    transition={$assetTransition}
    blockedFiles={$dirtyFiles.size}
    blockedDrafts={$volatileChatDrafts.size}
    onReload={requestAssetReload}
    onDismiss={clearChunkFailure}
  />
{/if}

{#if updateOffer !== null && $assetTransition === null}
  <UpdateToast offer={updateOffer} />
{/if}

<!-- OS-drop / paste uploads in flight (and, briefly, why one failed) —
     multi-MB screenshots over a tunnel take a beat; say so quietly. -->
{#if $uploadJobs.length > 0}
  <div class="upload-jobs" role="status" aria-live="polite">
    {#each $uploadJobs as job (job.id)}
      <div class="upload-job" class:err={job.error !== null}>
        {#if job.error === null}
          {#if job.progress === null}
            <span class="upload-spinner" aria-hidden="true"></span>
          {:else}
            <span class="upload-progress" aria-hidden="true">
              <span style:width={`${Math.round(job.progress * 100)}%`}></span>
            </span>
          {/if}
          <span class="upload-name">{job.name}</span>
          {#if job.progress !== null}
            <span class="upload-percent">{Math.round(job.progress * 100)}%</span>
          {/if}
          {#if job.cancel !== null}
            <button class="upload-cancel" aria-label={`cancel ${job.name}`} onclick={job.cancel}>×</button>
          {/if}
        {:else}
          <span class="upload-name">{job.error}</span>
        {/if}
      </div>
    {/each}
  </div>
{/if}

{#if reconnectSurface !== "hidden" && !$askpassActive && $assetTransition === null}
  <!-- An automatic reconnect is status, not a blocking decision: keep the
       rendered workbench readable while the tunnel heals. Only a failed
       attempt becomes a modal with Retry. A scoped askpass prompt temporarily
       owns this space when this host actually needs authentication. Dismissing
       a failure leaves a compact Retry surface: a native 401 must never become
       an unrecoverable blank state. -->
  {#if reconnectSurface === "status"}
    <div class="reconnect-status" role="status" aria-live="polite">
      <span class="reconnect-spinner" class:spin={reconnecting} aria-hidden="true"></span>
      <span class="reconnect-status-copy">
        <strong>{reconnecting ? `reconnecting to ${hostAlias}…` : `waiting for ${hostAlias}…`}</strong>
        <span>{reconnectReason ?? "the workbench will resume in place"}</span>
      </span>
      <button class="reconnect-status-dismiss" aria-label="dismiss reconnect status" onclick={dismissReconnect}>×</button>
    </div>
  {:else if reconnectSurface === "failure"}
    <div class="reconnect-overlay">
      <div
        class="reconnect-panel"
        role="alertdialog"
        aria-modal="true"
        aria-label="reconnect failed"
        tabindex="-1"
        use:modalFocus
      >
        <div class="reconnect-head">
          <span class="reconnect-spinner" aria-hidden="true"></span>
          <span class="reconnect-title">can’t reach {hostAlias}</span>
        </div>
        <p class="reconnect-body">{reconnectError}</p>
        <div class="reconnect-actions">
          <button class="reconnect-dismiss" onclick={dismissReconnect}>dismiss</button>
          <button
            class="reconnect-retry"
            use:focusOnMount
            disabled={reconnecting}
            onclick={retryReconnect}
          >
            {reconnecting ? "reconnecting…" : "retry"}
          </button>
        </div>
      </div>
    </div>
  {:else}
    <div class="reconnect-status" role="status" aria-live="polite">
      <span class="reconnect-spinner" aria-hidden="true"></span>
      <span class="reconnect-status-copy">
        <strong>reconnect to {hostAlias}</strong>
        <span>
          {reconnectError ?? "this window still needs fresh remote credentials"}
        </span>
      </span>
      <button
        class="reconnect-status-retry"
        disabled={reconnecting}
        onclick={retryReconnect}
      >
        {reconnecting ? "reconnecting…" : "retry"}
      </button>
    </div>
  {/if}
{/if}

<style>
  .shell {
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
  }

  .native-drag-region {
    position: fixed;
    top: 0;
    left: 112px;
    height: 32px;
    z-index: 220;
    user-select: none;
  }

  .body {
    flex: 1;
    display: flex;
    min-height: 0;
  }

  .rail {
    /* Width is inline (draggable, persisted); this is only the pre-hydration
       fallback. min-width guards against a stale/hand-set value wedging it. */
    position: relative;
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

  /* Overlay traffic lights occupy the rail's top-left corner. Reserve that
     corner for them + the control-height drag target; browsers keep the
     ordinary compact 16px inset above. */
  .native-titlebar-overlay .rail {
    padding-top: 36px;
  }

  /* In the native window the sidebar toggle belongs to the titlebar lane,
     beside the traffic lights, rather than trailing the workspace selector
     on the next row. Its higher stacking level keeps it clickable above the
     otherwise draggable lane. Browser windows retain the in-row placement. */
  .native-titlebar-overlay .rail-collapse {
    position: absolute;
    top: 5px;
    left: 82px;
    margin-left: 0;
    z-index: 221;
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

  /* The dashboard home row: fixed above the session sections, quiet until
     hovered or active — furniture, not a session. */
  .dash-row {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    color: var(--muted);
    text-align: left;
    width: 100%;
    margin-bottom: 2px;
  }
  .dash-row:hover {
    color: var(--fg);
  }
  .dash-row.dash-active {
    background: var(--row-active);
    color: var(--fg);
  }
  .dash-glyph {
    flex: none;
    display: block;
  }
  .dash-label {
    /* Match the session rows' name type (mono, text-sm) — the dashboard row
       sits in the same list and must read as one of them, not a stray
       proportional label. */
    font-family: var(--mono);
    font-size: var(--text-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
    font-size: var(--text-xs);
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

  /* Unread output: finished with output you haven't looked at. The quietest
     cue that still reads — a bolder name (the unread-mail convention), no bar
     or wash in the dense rail so it never looks like a hover/active state.
     The dashboard card wears the same bold name over a faint wash. A focused
     row is never unread, so this never fights the active state. */
  .row.unread .name,
  .chip.unread .chip-name {
    color: var(--fg);
    font-weight: 600;
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
    /* Column so the tree can grow to fill — a right-click in the empty area
       below the last row then still lands on the tree's context menu. */
    display: flex;
    flex-direction: column;
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

  .daemon-git.bad {
    color: var(--warn);
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

  /* Slurm chip: scheduler presence + your queued-job count, quiet like the
     git chip (same height/typography); flex:none so a long branch name
     truncates before this disappears. */
  /* Passive indicator (no hover/press affordance): slurm-here + queue count. */
  .daemon-compute {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.22rem;
    height: 20px;
    padding: 0 0.3rem;
    color: var(--muted);
    font: inherit;
  }

  .dc-count {
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
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

  /* When both the caffeinate toggle and the gear show, only the first claims
     the auto-margin; the gear sits just after it instead of splitting the gap. */
  .caffeinate + .daemon-settings {
    margin-left: 4px;
  }

  /* Armed = accented, so the "keeping this Mac awake" state reads at a glance. */
  .caffeinate.on,
  .caffeinate.on:hover {
    color: var(--accent);
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

  /* With the rail hidden, macOS needs a real titlebar lane for its traffic
     lights. Promote the existing focus strip instead of inventing extra
     chrome: the browser keeps the compact footer, while the native app gets
     one Codex-style context row above the pane tabs. */
  .native-titlebar-overlay .strip {
    order: -1;
    position: relative;
    height: 38px;
    padding: 0 12px 0 82px;
    gap: 10px;
    border-top: none;
    border-bottom: 1px solid var(--edge);
  }

  .native-titlebar-overlay .strip .chips {
    flex: 0 1 auto;
  }

  .strip-drag {
    align-self: stretch;
    flex: 1 0 80px;
    min-width: 80px;
    user-select: none;
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
    font-size: var(--text-xs);
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

  /* --- remote reconnect status + failure dialog --- */

  .reconnect-status {
    position: fixed;
    top: 14px;
    left: 50%;
    z-index: 190;
    width: min(430px, calc(100vw - 28px));
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 12px;
    transform: translateX(-50%);
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 9px;
    box-shadow: 0 8px 28px color-mix(in srgb, var(--fg) 12%, transparent);
    animation: reconnect-in 0.1s ease-out;
  }

  .reconnect-status-copy {
    min-width: 0;
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 2px;
    font-size: var(--text-xs);
  }

  .reconnect-status-copy strong {
    overflow: hidden;
    color: var(--fg);
    font-weight: 600;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .reconnect-status-copy span {
    overflow: hidden;
    color: var(--muted);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .reconnect-status-dismiss {
    flex: none;
    padding: 2px 5px;
    border: 0;
    color: var(--muted);
    background: none;
    cursor: pointer;
  }

  .reconnect-status-dismiss:hover {
    color: var(--fg);
  }

  .reconnect-status-retry {
    flex: none;
    appearance: none;
    padding: 4px 10px;
    border: 1px solid var(--edge);
    border-radius: 5px;
    color: var(--fg);
    background: none;
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }

  .reconnect-status-retry:hover:enabled {
    background: var(--row-hover);
  }

  .reconnect-status-retry:disabled {
    color: var(--muted);
    cursor: default;
  }

  @keyframes reconnect-in {
    from {
      opacity: 0;
    }
  }

  .reconnect-overlay {
    position: fixed;
    inset: 0;
    z-index: 190;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    background: var(--scrim);
    animation: reconnect-in 0.1s ease-out;
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

  /* Upload chips: same quiet toast recipe as the update toast, stacked
     bottom-center so they never cover the toast or the rail. */
  .upload-jobs {
    position: fixed;
    left: 50%;
    bottom: 14px;
    transform: translateX(-50%);
    z-index: 170; /* under the update toast (180) and overlays (190+) */
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    pointer-events: none;
  }

  .upload-job {
    display: flex;
    align-items: center;
    gap: 8px;
    max-width: 420px;
    padding: 5px 12px;
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 999px;
    box-shadow: 0 8px 28px color-mix(in srgb, var(--fg) 12%, transparent);
    font-size: var(--text-xs);
    color: var(--muted);
    animation: upload-in 0.16s ease-out;
    pointer-events: auto;
  }

  @keyframes upload-in {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  .upload-job.err {
    color: var(--err);
    border-color: color-mix(in srgb, var(--err) 45%, var(--edge));
  }

  .upload-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .upload-spinner {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    border: 1.5px solid color-mix(in srgb, var(--accent) 35%, transparent);
    border-top-color: var(--accent);
    animation: upload-spin 0.8s linear infinite;
  }

  .upload-progress {
    flex: none;
    width: 44px;
    height: 3px;
    overflow: hidden;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent) 18%, transparent);
  }

  .upload-progress > span {
    display: block;
    height: 100%;
    border-radius: inherit;
    background: var(--accent);
    transition: width 0.12s linear;
  }

  .upload-percent {
    flex: none;
    min-width: 3ch;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
    text-align: right;
  }

  .upload-cancel {
    appearance: none;
    border: none;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-md);
    line-height: 1;
    padding: 0 1px;
    cursor: pointer;
  }

  .upload-cancel:hover,
  .upload-cancel:focus-visible {
    color: var(--fg);
  }

  @keyframes upload-spin {
    to {
      transform: rotate(360deg);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .upload-spinner {
      animation: none;
    }
  }
</style>

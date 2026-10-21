<script lang="ts">
  import { RecentSessions } from "./lib/workspace/recentSessions";
  import AgentSetupDialog from "./lib/workspace/AgentSetupDialog.svelte";
  import { agentSetup } from "./lib/workspace/agentSetup";
  import { agentCatalog } from "./lib/workspace/launcher";
  import { LauncherState } from "./lib/workspace/launcherState.svelte";
  import { RecentsRail } from "./lib/workspace/recents.svelte";
  import { KeptWorktreeNotice } from "./lib/workspace/worktreeNotice.svelte";
  import { needsAppend, SessionRoster } from "./lib/workspace/sessionRoster";
  import { nextInGroup } from "./lib/workspace/sessionCycle";
  import { onMount, tick, untrack } from "svelte";
  import { paneTabHasKeyboardFocus } from "./lib/shared/tabNavigation";
  import { flip } from "svelte/animate";
  import { fade } from "svelte/transition";
  import { runStallDrive, stallDriveSpec } from "./lib/perf/tabSwitchDrive";
  import {
    ApiError,
    getActiveWorkspaceId,
    getHostLabel,
    isDetachedWindow,
    leaveHomeHub,
    takeOpenRequest,
    notifyUnauthorized,
    setActiveWorkspaceId,
  } from "./lib/net/api";
  import { linkRtt, linkRttNow, LINK_RTT_BADGE_MS } from "./lib/net/rtt";
  import { LOCAL_ECHO_MIN_RTT_MS } from "./lib/terminal/localEcho";
  import { pageVisible } from "./lib/shared/visibility";
  import {
    backgrounded,
    branchLabel,
    createSession,
    deleteSession,
    deleteWorkspace,
    displayName,
    sessionLabel,
    dotState,
    isBusy,
    listSessions,
    listWorkspaces,
    renameSession,
    setNewInWorktree,
    wantsWorktree,
    worktreeInUse,
    needsApproval,
    pollSessions,
    switchingViews,
    switchSessionView,
    touchWorkspace,
    ViewSwitchConflict,
    type AgentSpawn,
    type Session,
    type Workspace,
    isMastermind,
    needsAttention,
    scheduledWake,
  } from "./lib/workspace/sessions";
  // Type-only: the panel (and the ChatView it embeds) loads on first open,
  // never in the always-loaded entry bundle.
  import type MastermindPanel from "./lib/dashboard/MastermindPanel.svelte";
  import type { MastermindContext } from "./lib/dashboard/mastermindPanelState.svelte";
  import {
    mastermindPanel,
    setMastermindChrome,
    setMastermindPanelOpen,
    toggleMastermindPanel,
  } from "./lib/dashboard/mastermindPanelState.svelte";
  import { foldUnread, isUnread, markSeen } from "./lib/workspace/unread.svelte";
  import {
    agentRechecks,
    relativeAge,
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
  import { insertIntoComposer } from "./lib/chat/composerBus";
  import { volatileChatDrafts } from "./lib/chat/drafts";
  import { imageToAttachment, type ImageAttachment } from "./lib/chat/images";
  import {
    reportUploadError,
    setUploadPathInserter,
    trackFileOp,
    uploadAndInsert,
    uploadJobs,
    uploadToDir,
    uploadToSession,
  } from "./lib/net/uploads";
  import { get } from "svelte/store";
  import {
    activeSelection,
    clearSelection,
    composeAgentPathReference,
    composeChatQuote,
    composeCommitReference,
    composeSelectionReference,
    composeShellPathReference,
    needsCropUpload,
    composeTerminalReference,
    referenceTarget,
    composeProvenanceSuffix,
    setCommitReferenceHandler,
    setReferenceHandler,
    type CommitSelection,
    setSelection,
    workspaceRelative,
  } from "./lib/shared/reference";
  import { provenanceFor, rememberCopy } from "./lib/shared/provenance";
  import { asyncDisposer } from "./lib/shared/asyncDisposer";
  import { modalFocus, modalOpen } from "./lib/shared/modalFocus";
  import { findInFocusedPane, findLayer, findNavigation, targetIn } from "./lib/shared/find";
  import {
    activateTab,
    adjacentPane,
    allFilePaths,
    closePane,
    cycleTab,
    defaultLayout,
    detachTab,
    dropTab,
    findPane,
    topRightPane,
    focusPane,
    focusedFile as focusedFileOf,
    focusedSession as focusedSessionOf,
    moveFocus,
    moveTabToPane,
    navigateFileHistory,
    openFileLink,
    newPaneSplitDirection,
    MAX_PANES,
    openChanges,
    openSubagent,
    openFile,
    pinTab,
    openTabAs,
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
    openTimeline,
    openKnowledge,
    openPlugins,
    openPluginViewTab,
    openSessionsList,
    openGit,
    openSession,
    openSettings,
    paneForTab,
    panes as panesOf,
    pruneDeletedPath,
    pruneSessions,
    pruneTabs,
    rewriteTabPaths,
    allSessionIds,
    sessionPaneId,
    setPaneFont,
    setRatio,
    splitPane,
    moveTabDirection,
    tabCount,
    tabKey,
    toggleZoom,
    visibleSessionIds,
    type FocusDir,
    type Layout,
    type SplitDir,
    type Tab,
    type DiffTab,
    type GitDetailTab,
  } from "./lib/layout/layout";
  import type { LinkContext } from "./lib/shared/fileRef";
  import { openPath, setPathOpener, type OpenPathOptions, type PathKind } from "./lib/shared/openPath";
  import type { Reveal } from "./lib/shared/reveal";
  import { setChatLinkContext } from "./lib/chat/paths";
  import type { UrlTarget } from "./lib/terminal/urlLinks";
  import { setUrlPaneOpener, urlMenuEntries } from "./lib/shared/urlOpen";
  import { fileTabTitles, fsProbe, viewKindFor } from "./lib/previews/files";
  import { basename, relativeTo } from "./lib/shared/paths";
  import {
    CLOSE_SAVE_DEADLINE_MS,
    dirtyFiles,
    discardDirtyFile,
    saveDirtyFiles,
    unsavedCloseError,
    unsavedDeleteNote,
  } from "./lib/shared/editing";
  import {
    activateGitWorkspace,
    aheadBehindWords,
    gitEnv,
    gitExpandedRepos,
    gitRepoError,
    gitRepos,
    gitRepoStatuses,
    gitStatus,
    onGitNudge,
    openFileHistory,
    openRepoHistory,
    repoForPath,
    revealRepo,
    setGitFocus,
    setGitOpener,
    workspacesChanged,
    type DiffMode,
    type GitStatus,
  } from "./lib/workspace/git";
  import { computeStatus, initCompute, queuedJobCount } from "./lib/workspace/compute";
  import { activateTimelineWorkspace, onTimelineNudge } from "./lib/workspace/timeline.svelte";
  import {
    activateCommsWorkspace,
    glyphState,
    glyphTitle,
    onCommsNudge,
    onCommsReconnect,
  } from "./lib/workspace/comms.svelte";
  import {
    activateKnowledgeWorkspace,
    focusKnowledgeEntry,
    registerKnowledgeOpener,
    setKnowledgeRoot,
  } from "./lib/workspace/knowledge";
  // Register the active workspace's id-reference sources (chat chips,
  // previews): its knowledge, then plugins' `references/1` — side-effect
  // imports.
  import "./lib/knowledge/references";
  import "./lib/plugins/references";
  import { registerAskAgent } from "./lib/shared/askAgent";
  import { flash, showFlash } from "./lib/shared/flash.svelte";
  import {
    activatePluginsWorkspace,
    attachRequest,
    closeAttachSheet,
    knowledgeProviderActive,
    onAgentPluginsChanged,
    workspacePlugins,
  } from "./lib/plugins/store";
  import { platformFrame, pluginViewTitle, setViewOpener } from "./lib/plugins/platform";
  import ExtensionsGlyph from "./lib/plugins/ExtensionsGlyph.svelte";
  import ComputeStrip from "./lib/workspace/ComputeStrip.svelte";
  import type JobWindowNotices from "./lib/workspace/JobWindowNotices.svelte";
  import type JobClusterHome from "./lib/workspace/JobClusterHome.svelte";
  import {
    folderTargetAt,
    paneContentEl,
    paneIdAt,
    paneRootEl,
    registerLinkRow,
    registerStage,
    sameSpot,
    sideWord,
    unregisterLinkRow,
    unregisterStage,
    type DropSpot,
    type LayoutCtrl,
  } from "./lib/layout/dnd";
  import { createCrossWindow } from "./lib/layout/crossWindowController";
  import { createDragStart } from "./lib/layout/dragStart";
  import {
    chordDigit,
    fontChord,
    isMac,
    isReloadWindowKey,
    matchChord,
    REFERENCE_CHORD,
  } from "./lib/shared/keys";
  import {
    activeModLabel,
    isCapturing,
    keyHint,
    matchAction,
    modifierSetting,
  } from "./lib/shared/keybindings";
  import {
    askpassActive,
    closeThisWindow,
    isNativeShell,
    onFocusSession,
    onMenu,
    onUnsavedPrompt,
    openWindow,
    replyUnsaved,
    reportUnsaved,
    reportWindowScope,
    reportWindowView,
    setNativeWindowTitle,
    takePendingFocus,
  } from "./lib/net/native";
  import { clearBrowserNotices, deliverBrowserNotices } from "./lib/workspace/notices";
  import {
    placeAgentBrowser,
    shouldActOnAgentBrowserOpen,
    type AgentBrowserOpen,
  } from "./lib/browser/agentOpen";
  import UpdateToast from "./lib/workspace/UpdateToast.svelte";
  import {
    applyAppStatus,
    checkForUpdates,
    currentNotice,
    updateState,
  } from "./lib/workspace/update.svelte";
  import * as pool from "./lib/terminal/termPool";
  import * as chatPool from "./lib/chat/chatPoolRegistry";
  import {
    appearanceBootstrapForNavigation,
    applyRemoteSettings,
    flushSettings,
    getSetting,
    loadSettings,
    settingsLoaded,
  } from "./lib/settings/store.svelte";
  import type { DashCtx } from "./lib/dashboard/dash";
  import { flushViewState, windowKey } from "./lib/layout/viewState";
  import { loadRailChrome, saveRailChrome } from "./lib/layout/railState";
  import { createRailResize } from "./lib/layout/railResize";
  import {
    createViewPersistence,
    pruneDeadFiles as pruneDeadFileTabs,
  } from "./lib/layout/windowPersistence";
  import { hintsActive, initChordHints } from "./lib/shared/chordHints.svelte";
  import FolderPicker from "./lib/workspace/FolderPicker.svelte";
  import HomeNavigation from "./lib/workspace/HomeNavigation.svelte";
  import { loadPaneView, retryPaneView } from "./lib/layout/lazyViews";
  import AskpassModal from "./lib/workspace/AskpassModal.svelte";
  import ContextMenuHost from "./lib/shared/ContextMenuHost.svelte";
  import SlowLoad from "./lib/shared/SlowLoad.svelte";
  import { contextMenu } from "./lib/shared/contextMenu.svelte";
  import ConfirmDialog from "./lib/shared/ConfirmDialog.svelte";
  import CloseDirtyDialog from "./lib/layout/CloseDirtyDialog.svelte";
  import { WindowCloseGuard } from "./lib/layout/windowClose.svelte";
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
    BUILD_META_NAME,
    assetTransition,
    buildSource,
    clearChunkFailure,
    documentBuildSource,
    requestAssetReload,
    requestWindowReload,
    requireAssetNavigation,
    cancelWindowReload,
  } from "./lib/layout/assetTransition";
  import { installAssetGate } from "./lib/layout/assetGate.svelte";
  import { Connection } from "./lib/net/connection.svelte";
  import { focusOnMount } from "./lib/shared/focusOnMount";
  import { InlineRename } from "./lib/shared/inlineRename.svelte";
  import Launcher from "./lib/workspace/Launcher.svelte";
  import SessionGlyph from "./lib/shared/SessionGlyph.svelte";
  import BranchChip from "./lib/shared/BranchChip.svelte";
  import WakeRing from "./lib/shared/WakeRing.svelte";
  import { nudgeHistory } from "./lib/workspace/history";
  import { sameFile, setSameFileOpener } from "./lib/workspace/sameFile.svelte";
  import { requestSettingsSection, settingsOpen } from "./lib/settings/jump";
  import { setAgentNames } from "./lib/chat/toolLabels";
  import QuickOpen from "./lib/workspace/QuickOpen.svelte";
  import FileTree from "./lib/workspace/FileTree.svelte";
  import SplitTree from "./lib/layout/SplitNode.svelte";
  import Pane from "./lib/layout/Pane.svelte";

  /** macOS workbench windows put the native controls over the webview instead
   *  of spending a separate row on the repeated window title. */
  const nativeTitlebarOverlay = isNativeShell() && isMac;
  /** Last recents epoch seen on /ws/events (invalidate-and-pull). */
  let lastRecentsEpoch: number | null = null;
  let workspaces = $state<Workspace[]>([]);
  let sessions = $state<Session[]>([]);
  // Agent-communication tool rows carry the session id an agent passed;
  // they say the agent's name (read when a row renders — no subscription).
  setAgentNames((id) => {
    const s = sessions.find((x) => x.id === id);
    return s ? displayName(s) : null;
  });
  // Two live sessions writing one file: the chat line's and the dashboard
  // card's quiet notice (never the rail) reads this roster.
  $effect(() => {
    const list = sessions;
    const names = displayNames;
    untrack(() => sameFile.update(list, names));
  });
  /** Sessions whose shell provably sits at its OSC 133 prompt AND isn't
   *  running anything via the exec engine — the set predictive local echo
   *  may arm for. Recomputed per sessions snapshot (a few times a minute)
   *  so the per-keystroke echoArmed check is one Set lookup, not a walk of
   *  the $state proxy array. */
  const echoReadyIds = $derived(
    new Set(sessions.filter((s) => s.phase === "ready" && !isBusy(s)).map((s) => s.id)),
  );
  /** Linked-terminal edges, mirrored from /ws/events snapshots. */
  let links = $state<Link[]>([]);
  let activeWsId = $state<string | null>(getActiveWorkspaceId());
  let pickerOpen = $state(false);
  let quickOpenOpen = $state(false);
  /** Element focused when the quick-open palette opened; restored on close. */
  let quickOpenRestoreEl: HTMLElement | null = null;
  let createError = $state<string | null>(null);

  // --- the daemon connection: health, events, remote reconnect --------------
  /** Reachability, the remote window's SSH reconnect, the 401 latch and the
   *  shell's window-scope report (net/connection.svelte.ts). Constructed here:
   *  its 401 and window-scope effects keep this place in App's effect order. */
  const conn: Connection = new Connection({
    activeWsId: () => activeWsId,
    detachedWindow: () => detachedWindow,
    windowLabel: (): string => windowLabel,
    eventsSocket: () => eventsSocket,
    onEventsRecovered: onCommsReconnect,
    onAppStatus: applyAppStatus,
    onAppVersion: (version) => {
      updateState.appVersion = version;
    },
    appearanceForNavigation: appearanceBootstrapForNavigation,
    windowKey,
    assets: {
      servedBuild: () =>
        documentBuildSource(
          document.querySelector<HTMLMetaElement>(`meta[name="${BUILD_META_NAME}"]`)?.content,
        ),
      buildSource,
      requireNavigation: requireAssetNavigation,
    },
  });
  const { hostAlias, isRemoteWindow, jobCtx, scopeAlias } = conn;
  const eventsUp = $derived(conn.eventsUp);
  /** The job window's notices — their own chunk, loaded only in job windows. */
  let JobNoticesView = $state<typeof JobWindowNotices | null>(null);
  if (jobCtx !== null) {
    import("./lib/workspace/JobWindowNotices.svelte").then(
      (m) => {
        JobNoticesView = m.default;
      },
      (err: unknown) => console.error("job window notices failed to load", err),
    );
  }
  /** A cluster workspace window's cluster page (its own chunk), shown over
   *  the workspace in place of the folder picker: this window's chimaera
   *  knows only its one workspace; the cluster page knows them all. */
  const clusterWs = isNativeShell() ? (jobCtx?.cws ?? null) : null;
  let clusterHomeOpen = $state(false);
  let ClusterHomeView = $state<typeof JobClusterHome | null>(null);

  function openClusterHome(): void {
    clusterHomeOpen = true;
    if (ClusterHomeView !== null) return;
    import("./lib/workspace/JobClusterHome.svelte").then(
      (m) => {
        ClusterHomeView = m.default;
      },
      (err: unknown) => {
        clusterHomeOpen = false;
        console.error("cluster page failed to load", err);
      },
    );
  }

  // --- the agent launcher (split button + popover) ---
  /** The new-agent split button, its popover and the agent catalog
   *  (workspace/launcherState.svelte.ts); every start comes back through
   *  `startAgent` → `spawnSession`, which writes the roster and layout here. */
  const launcher = new LauncherState({
    activeWsId: () => activeWsId,
    startAgent: (spawn) => startAgent(spawn),
    openPicker: () => openPicker(),
    openSettingsSurface: () => openSettingsSurface(),
    refocusTerminal: () => {
      const sid = focusedSessionOf(layout);
      if (sid !== null) pool.focusTerminal(sid);
    },
  });
  $effect(() => { launcher.adoptCatalog($agentCatalog); });
  // --- the rail's Recents section (ended agent conversations) ---
  /** Recents rows, their fit, expand, archive and reopen
   *  (workspace/recents.svelte.ts). Its fit observer is declared here; the
   *  workspace-switch refetch further down, after the kept-worktree notice. */
  const recentsRail = new RecentsRail({
    activeWsId: () => activeWsId,
    sessions: () => sessions,
    startAgent: (spawn) => startAgent(spawn),
    offerWorktreeRemoval: (ws, path, branch) => keptWorktree.offer(ws, path, branch),
  });
  recentsRail.trackFit();
  /** Rail row currently showing the inline kill confirmation. */
  let confirmKillId = $state<string | null>(null);
  /** Inline rename of a rail row (double-click / F2), keyed by session id:
   *  chimaera owns the pin, so it works for every session kind, not just
   *  claude's /rename. No unchanged check: re-submitting the shown name pins
   *  it. The PATCH is fire-and-forget; the next sessions snapshot restores
   *  the truth on failure. Focus is left where the unmounted input leaves it. */
  const railRename = new InlineRename<string>({
    submit(id, name, close) {
      close();
      renameSession(id, name).catch(() => {
        // next sessions snapshot restores the truth
      });
    },
    errorText: () => "",
  });
  /** Inline-create request for the file tree (rail-header buttons). */
  let treeCreate = $state<{ kind: "file" | "dir"; nonce: number } | null>(null);
  let treeCreateNonce = 0;
  /** A failed delete's message; keeps the confirm dialog open. */
  let deleteError = $state<string | null>(null);
  /** File tabs whose close waits on "save changes?" (held by identity — the
   *  indices shift as other tabs close around them). */
  let pendingClose = $state<Tab[]>([]);
  let closeSaving = $state(false);
  let closeError = $state<string | null>(null);
  /** Bumped by Cancel: a "Save" still waiting then touches nothing. */
  let closeAttempt = 0;
  /** The native window close / app quit over this window's unsaved files. */
  const windowClose = new WindowCloseGuard(replyUnsaved);
  /** Element that held focus when the picker opened; restored on close. */
  let pickerRestoreEl: HTMLElement | null = null;

  // In-window layout: the tree is daemon-owned per-window view state; until
  // the GET resolves the stage stays blank (fast, local) so a restored tree
  // never flashes through the single-pane default.
  let layout = $state<Layout>(defaultLayout());
  let layoutReady = $state(false);
  let gotSessions = $state(false);
  let autoOpened = false;
  // Raw: a spot is an immutable value replaced wholesale (dnd + the OS-drop
  // handlers gate rewrites with sameSpot), never mutated in place.
  let dropSpot = $state.raw<DropSpot | null>(null);
  /** Panes whose bottom band is armed for the CURRENT drag (reference or
   *  link targets) — zone previews stop above the band on these. */
  let bandPanes = $state<ReadonlySet<string>>(new Set());

  /** File-tree reveal request (terminal dir links); nonce distinguishes repeats. */
  let treeReveal = $state<{ path: string; nonce: number } | null>(null);

  // A window born by tearing a pane out of another window. The authority is
  // the `dt=1` hash param (persisted registry-side on WindowRecord.detached,
  // so restore/re-home rebuild it), adopted into sessionStorage before boot —
  // which is what keeps a solo window off the workspace-mirror restore
  // fallback; the view-state blob's `dt:1` is the belt-and-braces second
  // signal. Being detached means: no rail (ever), no ws_ mirror write, no
  // workspace-roster chrome, self-close when emptied, tab-led title.
  let detachedWindow = $state(isDetachedWindow());
  /** The winKey of the window this one detached from (re-attach target). */
  let detachOrigin = $state<string | null>(null);
  /** Self-close was requested but the browser refused (a popup we didn't
   *  open script-side): show the "this window is empty" state instead. */
  let detachedEmpty = $state(false);

  // --- cross-window moves (crossWindow.ts is the protocol) -------------------

  /** Sender ledger, transport, adopt, detach-out, re-attach and the move
   *  menu; it never holds the layout (App's getters and setters below). */
  const xw = createCrossWindow({
    winKey: () => winKey,
    getLayout: () => layout,
    setLayout: (next) => (layout = next),
    getWs: () => activeWsId,
    getWsRoot: () => workspace?.root ?? null,
    isReady: () => layoutReady,
    isJobWindow: () => jobCtx !== null,
    getOrigin: () => detachOrigin,
    windowLabel: () => windowLabel,
    isDetached: () => detachedWindow,
    getDropSpot: () => dropSpot,
    setDropSpot: (spot) => (dropSpot = spot),
    dirtyFiles: () => get(dirtyFiles),
    volatileDrafts: () => get(volatileChatDrafts),
    flash: showFlash,
    onAdopted: () => {
      const sid = focusedSessionOf(layout);
      if (sid !== null) pool.focusTerminal(sid);
    },
  });

  // One transport per (window, workspace scope); incoming adoption and acks
  // ride it. The effect keys on the workspace only — handlers read live
  // state at event time, not at subscription time. The teardown nulls
  // dropSpot (a leave that raced a workspace switch).
  $effect(() => xw.subscribe(activeWsId));

  /** Drag starts (tabs, rail rows, tree entries, pane grips); owns the
   *  per-window drag id the cross-window track/cancel calls are fenced by. */
  const { beginDrag, beginPaneDrag } = createDragStart({
    getLayout: () => layout,
    setLayout: (next) => (layout = next),
    setDropSpot: (spot) => (dropSpot = spot),
    setBandPanes: (next) => (bandPanes = next),
    xw,
    isNative: isNativeShell,
    tabLabel: (tab) => tabLabel(tab),
    sessionAlive: (id) => sessionsById.get(id)?.alive ?? false,
    paneSessionId: (paneId) => osDropSession(paneId)?.id ?? null,
    linkTargetsFor: (tab) => linkTargetsFor(tab),
    linkSessionsFor: (tab) => linkSessionsFor(tab),
    referenceFileDrop: (paneId, path, kind) => referenceFileDrop(paneId, path, kind),
    referenceCommitDrop: (paneId, tab) => referenceCommitDrop(paneId, tab),
    linkByDrop: (terminalId, agentPaneId) => linkByDrop(terminalId, agentPaneId),
    linkBySession: (terminalId, agentId) => linkBySession(terminalId, agentId),
    focusTerminal: (sid) => pool.focusTerminal(sid),
    focusFocusedSession: () => {
      const sid = focusedSessionOf(layout);
      if (sid !== null) pool.focusTerminal(sid);
    },
    focusPane: (paneId) => ctrl.focusPane(paneId),
  });

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

  const { onFilesDividerDown, onRailResizeDown, resetRailWidth } = createRailResize({
    getRailEl: () => railEl,
    getDaemonEl: () => daemonEl,
    getFilesFrac: () => filesFrac,
    setFilesFrac: (frac) => (filesFrac = frac),
    setFilesDividerActive: (active) => (filesDividerActive = active),
    getRailWidth: () => railWidth,
    setRailWidth: (width) => (railWidth = width),
    setRailDividerActive: (active) => (railDividerActive = active),
    setDragging: pool.setDragging,
  });

  const winKey = windowKey();

  /**
   * The window's view-state persistence (lib/layout/windowPersistence.ts):
   * keyed by (window, workspace) with a workspace-only mirror. It reads App's
   * state through getters and writes `layout` only through the setter below,
   * so App stays the sole owner of the live layout.
   */
  const viewPersistence = createViewPersistence({
    winKey,
    getWs: () => activeWsId,
    getWsRoot: () => workspace?.root ?? null,
    getLayout: () => layout,
    setLayout: (next) => {
      layout = next;
    },
    isReady: () => layoutReady,
    setReady: (ready) => {
      layoutReady = ready;
    },
    isDetached: () => detachedWindow,
    getOrigin: () => detachOrigin,
    setDetached: (detached, origin) => {
      detachedWindow = detached;
      detachOrigin = origin;
    },
    onBooted: () => {
      pruneAndAutoOpen();
      pruneDeadFiles();
      if (__CHIMAERA_PERF__) {
        const spec = stallDriveSpec();
        if (spec !== null) {
          void runStallDrive(
            {
              get: () => layout,
              set: (next) => {
                layout = next;
              },
            },
            spec,
          );
        }
      }
    },
  });

  const workspace = $derived(workspaces.find((w) => w.id === activeWsId) ?? null);
  /** The workspace root sits in a git repository — the only place a new
   *  agent can get its own worktree. */
  const workspaceIsRepo = $derived(
    $gitStatus !== null && $gitStatus.repo && $gitStatus.workspace_id === activeWsId,
  );
  /** New agents here start in their own worktree (the remembered choice). */
  const newInWorktree = $derived(workspaceIsRepo && workspace?.new_in_worktree === true);
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
  const windowLabel = $derived.by(() => {
    if (activeWsId === null) return isRemoteWindow ? `Home •${hostAlias}` : "Home";
    const base = (workspace?.name ?? "") + (isRemoteWindow ? ` •${hostAlias}` : "");
    if (!detachedWindow) return base;
    // A detached solo window is named for what it shows, so the tray and the
    // "Move to ⟨window⟩" menu can tell it apart from the real workspace
    // window (which wears the bare workspace name).
    const p = findPane(layout.root, layout.focusedPaneId);
    const t = p?.tabs[p.active];
    return t !== undefined && base !== "" ? `${tabLabel(t)} — ${base}` : base;
  });

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
    // The Timeline / Knowledge / plugin-status stores follow the same
    // activate-on-switch, nudge-to-refetch discipline (one small GET each).
    void activateTimelineWorkspace(wsId);
    activateCommsWorkspace(wsId);
    void activateKnowledgeWorkspace(wsId);
    void activatePluginsWorkspace(wsId);
    eventsSocket?.watch(wsId);
  });

  $effect(() => {
    setKnowledgeRoot(workspace?.root ?? null);
  });

  // Knowledge's two app-level hooks: open an entry beside a pane (a chat's
  // id chip) and "Ask an agent" (draft into the working agent's composer).
  $effect(() => {
    const offOpen = registerKnowledgeOpener((ekey, from) =>
      openKnowledgeFromPane(from.paneId ?? layout.focusedPaneId, ekey, from.newSplit),
    );
    const offAsk = registerAskAgent((text) => askAgentFrom(layout.focusedPaneId, text));
    return () => {
      offOpen();
      offAsk();
    };
  });

  // Mounted file views + visible listing directories are the complete scope of
  // the daemon's bounded disk monitor. Registrations follow keep-alive mounts,
  // not every tab ever opened, and are re-sent after reconnect by EventsSocket.
  $effect(() => {
    const files = $diskWatchFiles;
    const dirs = $diskWatchDirs;
    eventsSocket?.watchFs(files, dirs);
  });

  // The repositories below the root this window is looking at — sections
  // open in the Source Control panel, and those holding a mounted file. Only
  // these ride the daemon's 12 s git backstop; the rest refresh on events.
  $effect(() => {
    const below = $gitRepos.filter((r) => r.kind === "nested" || r.kind === "submodule");
    const watched = new Set<string>($gitExpandedRepos);
    for (const f of $diskWatchFiles) {
      const r = repoForPath(below, f);
      if (r) watched.add(r.path);
    }
    eventsSocket?.watchGitRepos([...watched]);
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
  // The rail and focus-mode strip share the terminal-then-agent order.
  const shellSessions = $derived(wsSessions.filter((s) => s.kind !== "agent"));
  const agentSessions = $derived(wsSessions.filter((s) => s.kind === "agent"));
  const railSessions = $derived([...shellSessions, ...agentSessions]);
  const sessionsById = $derived(new Map(sessions.map((s) => [s.id, s])));
  /** terminal session id -> agent session id (one agent per terminal). */
  const linksByTerminal = $derived(new Map(links.map((l) => [l.terminal_id, l.agent_id])));
  const focusedSessionId = $derived(focusedSessionOf(layout));

  /** Sessions on screen in this window (each pane's active tab). */
  const visibleSessions = $derived(
    activeWsId !== null && layoutReady ? visibleSessionIds(layout) : [],
  );
  // Tell the notifier what this window shows, so it never alerts about a
  // session the user is looking at (a torn-off window's whole scope; a
  // workspace window also covers its hidden tabs) — and clears alerts for
  // ones they now are. Keyed on the joined ids so a layout write that changes
  // nothing on screen costs no IPC.
  const visibleKey = $derived(visibleSessions.join(" "));
  $effect(() => {
    const key = visibleKey;
    const ids = key === "" ? [] : key.split(" ");
    if (isNativeShell()) {
      const timer = setTimeout(() => void reportWindowView(ids).catch(() => {}), 120);
      return () => clearTimeout(timer);
    }
    if (document.hasFocus()) clearBrowserNotices(ids);
  });
  // Browser: coming back to this tab is looking at what it shows.
  $effect(() => {
    if (isNativeShell()) return;
    const onFocus = (): void => clearBrowserNotices(untrack(() => visibleSessions));
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  });
  const focusedFilePath = $derived(focusedFileOf(layout));

  /**
   * The status strip's branch chip follows focus: the repository holding the
   * focused file, else the focused session's checkout (or folder). Nothing
   * focused inside one: the workspace's own repository, or — a folder of
   * several — how many there are.
   */
  const gitFocusPath = $derived.by((): string | null => {
    const s = focusedSessionId !== null ? sessionsById.get(focusedSessionId) : undefined;
    return focusedFilePath ?? s?.git?.worktree ?? s?.cwd_current ?? s?.cwd ?? null;
  });
  // The Source Control panel auto-expands the repository holding this.
  $effect(() => {
    setGitFocus(gitFocusPath);
  });
  const stripGit = $derived.by(
    (): { status: GitStatus; label: string | null; path: string | null } | null => {
      const repos = $gitRepos;
      if (repos.length > 1) {
        const repo = repoForPath(repos, gitFocusPath);
        if (repo !== null) {
          const status =
            repo.kind === "root" || repo.kind === "enclosing"
              ? $gitStatus
              : ($gitRepoStatuses.get(repo.path) ?? null);
          if (status !== null && status.repo) {
            const name = basename(repo.path) || repo.path;
            return { status, label: repo.kind === "root" ? null : name, path: repo.path };
          }
        }
        return null;
      }
      return $gitStatus !== null ? { status: $gitStatus, label: null, path: null } : null;
    },
  );
  /** Repositories in the workspace, for the "N repos" chip. */
  const stripRepoCount = $derived($gitRepos.length);
  /** Open file tabs' display titles (basename, disambiguated by parent dir). */
  const fileTitles = $derived(fileTabTitles(allFilePaths(layout)));
  const zoomedPane = $derived(
    layout.zoomedPaneId !== null ? findPane(layout.root, layout.zoomedPaneId) : null,
  );

  // --- the window's Mastermind panel ------------------------------------------
  // One right-hand panel per window, on every view; its only entry points are
  // the corner icon (in the pane touching the window's top-right), ⌘J and
  // Quick Open. A workspace without a Mastermind shows no icon at all.
  const mmCfg = $derived(workspace?.mastermind ?? null);
  const mmSession = $derived(
    mmCfg !== null ? (sessionsById.get(mmCfg.session_id) ?? null) : null,
  );
  const mmCornerPaneId = $derived(zoomedPane?.id ?? topRightPane(layout.root).id);
  let bodyWidth = $state(0);
  let MastermindPanelView = $state<typeof MastermindPanel | null>(null);
  $effect(() => {
    if (!mastermindPanel.open || MastermindPanelView !== null) return;
    import("./lib/dashboard/MastermindPanel.svelte").then(
      (m) => {
        MastermindPanelView = m.default;
      },
      (err: unknown) => {
        // A chunk that can't load (stale assets after an update) must not
        // leave an "open" panel nobody can see: close it so the corner icon
        // and ⌘J work again.
        console.error("Mastermind panel failed to load", err);
        setMastermindPanelOpen(false);
      },
    );
  });
  $effect(() => {
    setMastermindChrome({
      available: mmCfg !== null,
      cornerPaneId: mmCornerPaneId,
      attention:
        mmSession !== null &&
        mmSession.alive &&
        (needsAttention(mmSession) || isUnread(mmSession.id)),
    });
  });
  // A reply that lands while the panel is open was seen — it must not light
  // the icon's dot the moment the panel closes.
  $effect(() => {
    const id = mmSession?.id;
    if (id !== undefined && mastermindPanel.open && $pageVisible && isUnread(id)) markSeen(id);
  });
  /** What the focused tab IS, for the panel's one context question ("How's
   *  <session> doing?", "What changed in <file>?") — null when the focused
   *  tab isn't something the Mastermind can read about. */
  const mmContext = $derived.by((): MastermindContext | null => {
    const pane = findPane(layout.root, layout.focusedPaneId);
    const tab = pane !== null ? pane.tabs[pane.active] : undefined;
    if (tab === undefined) return null;
    const root = workspace?.root;
    const rel = (p: string) => (root !== undefined ? workspaceRelative(p, root) : p);
    switch (tab.surface) {
      case "file":
      case "diff":
        return { kind: "file", name: rel(tab.path).split("/").pop() ?? tab.path, ref: rel(tab.path) };
      case "finder": {
        const r = rel(tab.path);
        // The root is "." — the chip names the workspace instead.
        const name = r === "." ? (workspace?.name ?? "this workspace") : r.split("/").slice(-2).join("/");
        return { kind: "folder", name, ref: r };
      }
      case "terminal":
      case "changes": {
        const s = sessionsById.get(tab.sessionId);
        if (s === undefined) return null;
        const name = displayNames.get(s.id) ?? s.name;
        const kind = tab.surface === "changes" ? "changes" : s.kind === "agent" ? "session" : "terminal";
        return { kind, name, ref: s.id };
      }
      default:
        return null;
    }
  });

  /** Sessions in the active workspace blocked on the user's approval. */
  // A detached solo window badges only ITS OWN sessions: workspace-wide
  // attention is the main window's wayfinding, and a torn-off pane wearing
  // the whole roster's badge reads as a false alarm (found live).
  // Alive-gated like the dashboard lane: the daemon keeps a crashed chat
  // driver registered (alive:false, state errored) until the user deletes it,
  // so its row still wears the red glyph — but nobody can answer a dead
  // session, and a pill/title saying 1 while the dashboard says 0 is exactly
  // the stale state this count exists to prevent.
  const needsYou = $derived.by(() => {
    if (!detachedWindow) return wsSessions.filter((s) => s.alive && needsApproval(s)).length;
    const mine = new Set(allSessionIds(layout));
    return wsSessions.filter((s) => s.alive && mine.has(s.id) && needsApproval(s)).length;
  });

  /** The focused pane is showing the dashboard (its rail row highlights). */
  const dashboardOpen = $derived.by(() => {
    const p = findPane(layout.root, layout.focusedPaneId);
    return p?.tabs[p.active]?.surface === "dashboard";
  });
  /** … or Knowledge (the conditional rail row below it). */
  const knowledgeOpen = $derived.by(() => {
    const p = findPane(layout.root, layout.focusedPaneId);
    return p?.tabs[p.active]?.surface === "knowledge";
  });
  const pluginsOpen = $derived.by(() => {
    const p = findPane(layout.root, layout.focusedPaneId);
    return p?.tabs[p.active]?.surface === "plugins";
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
    recents: recentsRail.visible,
    mru: agentMru,
    mastermind: workspace?.mastermind ?? null,
    refreshWorkspaces,
    onOpenRecent: recentsRail.open,
    onNewTerminal: newShell,
    onNewAgent: launcher.newAgentPrimary,
    onOpenGit: openGitPanel,
    onOpenSession: openSess,
    onOpenTimeline: openTimelineSurface,
    onOpenKnowledge: openKnowledgeSurface,
    onOpenExtensions: openPluginsSurface,
    onOpenActivity: openActivitySurface,
  });

  /**
   * Where a reference lands, most-explicit first: a chat transcript's
   * selection goes to that same chat (quoted into its reply — never to
   * another agent; null while it cannot take a message), the agent LINKED
   * to the selection's source terminal (the leash is the bridge the user
   * built), else the focused agent session, else the workspace's most
   * recently active agent, else its newest live agent. Null (no agent
   * session at all) renders every reference affordance disabled.
   */
  const refTargetSession = $derived.by(() => {
    const sel = $activeSelection;
    if (sel !== null && sel.kind === "chat") {
      const own = sessionsById.get(sel.sessionId);
      return own !== undefined && own.kind === "agent" && own.ui === "chat" && own.alive ? own : null;
    }
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

  // The health poll, then its kick on every events up/down transition (the
  // kick reads the handle the poll effect assigns; net/connection.svelte.ts).
  conn.trackHealth();

  // While this document is hidden nobody sees the presence animations: a
  // root class lets each component pause its own infinite keyframes with an
  // enumerated `html.app-hidden` rule (see app.css) — box-shadow/opacity
  // pulses otherwise burn frames for hours in a backgrounded window.
  $effect(() => {
    document.documentElement.classList.toggle("app-hidden", !$pageVisible);
    return () => document.documentElement.classList.remove("app-hidden");
  });

  // The asset navigation gate (layout/assetGate.svelte.ts): failed-chunk
  // reports, then build/connection navigation, its hidden-window catch-up,
  // its timer cleanup and the native Reload Window claim. Declared before
  // the beforeunload guard below, which re-reads the live dirty set.
  installAssetGate();

  // Slurm strip: one probe at boot; the store keeps its own 60s poll gated on
  // "scheduler is slurm" + a visible window (see workspace/compute.ts).
  $effect(() => initCompute());

  // Daemon/app build skew (native) for the update toast.
  conn.trackBuildSkew((skew) => {
    updateState.buildSkew = skew;
  });

  /** The one update worth offering in this window right now — or, after an
   *  explicit check, its answer. */
  const updateNotice = $derived(currentNotice(scopeAlias));
  updateState.scope = scopeAlias;

  // The 5s sessions + links poll, only while /ws/events is down.
  conn.pollWhileEventsDown({
    sessions: pollSessions,
    listLinks,
    applySessions,
    setLinks: (l: Link[]) => (links = l),
  });

  // Persist the layout (debounced in viewState) whenever it changes, keyed
  // by (window, workspace) so each workspace keeps its own tree.
  // The blob is built before the layoutReady gate, so the effect tracks the
  // layout, workspace and detachedness even while not ready.
  $effect(() => {
    viewPersistence.persist();
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

  // Proxy sessions are NOT revoked on tab close: mint is idempotent per
  // host:port, so a second window onto the same app shares the id, and a
  // client-side sweep would DELETE it out from under that window (its iframe
  // then gets expired responses until it re-mints). The daemon's idle TTL is
  // the reaper — a mounted pane's health ping keeps its own session alive; a
  // closed one just stops refreshing. See browser/proxy.ts::revokeProxy.
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
      onUrlMenu: (event, url) => contextMenu.openAt(event, urlMenuEntries(url)),
      // Predictive local echo (localEcho.ts): only on a remote window, only
      // once the link is measurably slow, only while the events bus is live
      // (a frozen sessions snapshot must not vouch for a shell's phase), and
      // only while the shell sits at its OSC 133 prompt AND isn't busy via
      // the exec engine — agent TUIs (phase "unknown") and running commands
      // (which may read without echoing) never ghost. echoReadyIds keeps the
      // per-keystroke check O(1) instead of walking the $state proxy array.
      echoArmed: (id) => {
        if (!isRemoteWindow || !eventsUp) return false;
        const rtt = linkRttNow();
        if (rtt === null || rtt < LOCAL_ECHO_MIN_RTT_MS) return false;
        return echoReadyIds.has(id);
      },
    });
    // Every rendered link surface (chat prose, markdown previews, terminal
    // output) resolves a proxyable URL to a browser pane through here; App is
    // the only owner of the layout.
    setUrlPaneOpener((target, newSplit) =>
      openBrowserFromPane(layout.focusedPaneId, target, newSplit),
    );
    // Every path link (terminal, chat prose, tool cards, documents) opens
    // through one opener, so a line reveal and Cmd/Ctrl-click split work
    // wherever the link was; chat resolves against the same per-session
    // context as terminal links.
    setPathOpener(openPathInLayout);
    setViewOpener(openPluginViewSurface);
    setSameFileOpener(openSess);
    setChatLinkContext(linkContext);
    setReferenceHandler(referenceSelection);
    setCommitReferenceHandler(referenceCommitNow);
    setGitOpener((tab) => openGitFromPane(layout.focusedPaneId, tab, false));
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
      onTimeline: onTimelineNudge,
      onComms: onCommsNudge,
      onAgentPlugins: onAgentPluginsChanged,
      onUpdate: (status) => {
        updateState.daemon = status;
      },
      onRecents: (epoch) => {
        // Invalidate-and-pull, like git: a conversation retired somewhere;
        // refetch this window's workspace list iff the epoch moved.
        if (lastRecentsEpoch !== epoch) {
          lastRecentsEpoch = epoch;
          recentsRail.refresh();
          // A session ended: the history surfaces refetch while visible.
          nudgeHistory();
        }
      },
      onFs: notifyDiskChange,
      onPlatform: platformFrame,
      // The browser's notification source. The native app ignores these: its
      // shell consumes the same feed per daemon and posts OS notifications.
      onNotices: (list) => {
        if (isNativeShell()) return;
        deliverBrowserNotices(list, {
          visible: untrack(() => visibleSessions),
          workspaceId: untrack(() => (detachedWindow ? null : activeWsId)),
          onClick: focusFromNotification,
        });
      },
      onBrowserOpen: onAgentBrowserOpen,
      // RTT reset + comms refetch on recovery, then eventsUp, then the
      // dead-link save retry (net/connection.svelte.ts).
      onStatus: (up) => conn.noteEventsStatus(up),
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
    void launcher.refreshAgents();

    // Native menu items the shell forwards to the focused window. Cmd+W
    // closes the focused VIEW (a home window just closes), reclaiming the
    // chords a browser reserves for tabs.
    let unlistenMenu: (() => void) | null = null;
    let unlistenDaemonMoved: (() => void) | null = null;
    let unlistenHostStatus: (() => void) | null = null;
    let unlistenAppUpdate: (() => void) | null = null;
    let unlistenFocusSession: (() => void) | null = null;
    if (isNativeShell()) {
      // A notification was clicked for a session this window should show.
      // Listen first, then ask for a focus the shell may already owe us (a
      // click that OPENED this window can beat the listener).
      const focusListening = onFocusSession(focusFromNotification);
      unlistenFocusSession = asyncDisposer(focusListening);
      void focusListening.then(
        () =>
          takePendingFocus().then((id) => {
            if (id !== null) focusFromNotification(id);
          }),
        () => {},
      );
      // Build-skew + app-update signals for the update toast.
      unlistenAppUpdate = conn.listenAppUpdate();
      unlistenMenu = asyncDisposer(
        onMenu((action) => {
          // Workbench commands wait while a modal asks something.
          if (modalOpen() && action !== "check-updates") return;
          switch (action) {
            case "close-view":
              if (homeSettingsOpen) homeSettingsOpen = false;
              else if (activeWsId === null) closeThisWindow();
              else if (layoutReady) closeView(layout.focusedPaneId);
              break;
            case "new-terminal":
              newShell();
              break;
            case "new-agent":
              launcher.newAgentPrimary();
              break;
            case "settings":
              openSettingsSurface();
              break;
            case "check-updates":
              void checkForUpdates(true);
              break;
          }
        }),
      );
      // The local daemon was replaced (self-update): re-home or reload.
      unlistenDaemonMoved = conn.listenLocalDaemonUpdated();
      // This remote window's tunnel dropped or came back.
      unlistenHostStatus = conn.listenHostStatus();
    }

    const onPagehide = () => {
      void flushViewState();
      void flushSettings();
    };
    const onCopy = () => rememberCopy();
    // Holding the app modifier reveals numbered pane destinations.
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
      unlistenFocusSession?.();
      document.removeEventListener("copy", onCopy);
      window.removeEventListener("dragenter", onWindowDragEnter);
      window.removeEventListener("dragover", onWindowDragOver);
      window.removeEventListener("dragleave", onWindowDragLeave);
      window.removeEventListener("drop", onWindowDrop);
      stopChordHints();
      setReferenceHandler(null);
      setCommitReferenceHandler(null);
      setGitOpener(null);
      setUploadPathInserter(null);
      setPathOpener(null);
      setViewOpener(null);
      setSameFileOpener(null);
      recentsRail.dispose();
      setChatLinkContext(null);
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
    if (source.kind !== "file" && source.sessionId === id) return;
    const root = workspace?.root;
    const suffix = composeProvenanceSuffix(
      source,
      source.kind === "file" && root !== undefined
        ? workspaceRelative(source.path, root)
        : null,
      source.kind !== "file" ? (displayNames.get(source.sessionId) ?? null) : null,
    );
    queueMicrotask(() => pool.sendText(id, suffix));
  }

  /**
   * Type `text` into a session's input: through its pooled socket when the
   * terminal is warm, else a one-shot socket. The composed text never carries
   * a newline, so nothing is ever submitted. If the session is open in a
   * pane, surface it so the typed reference is reviewable at a glance.
   */
  function typeIntoSession(id: string, text: string, image?: ImageAttachment): void {
    // Chat sessions: the input is the mounted Composer, not a PTY socket.
    // insertIntoComposer buffers when the composer hasn't mounted yet (a chat
    // pane restored on a slow snapshot) and drains on registration, so the
    // grant is never lost to a mount race. `image` (chat only) rides along.
    if (sessionsById.get(id)?.ui === "chat") {
      const loc = paneForTab(layout.root, { surface: "terminal", sessionId: id });
      if (loc !== null) layout = activateTab(layout, loc.paneId, loc.index);
      insertIntoComposer(id, text, "inline", undefined, image);
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
    const targetId = target.id;
    if (sel.kind === "chat") {
      // Quoted into the reply of the chat it was selected in: that chat is
      // on screen (the selection is in it, maybe in the Mastermind panel
      // rather than a pane), so nothing is split open or activated.
      const quote = composeChatQuote(sel.text);
      if (targetId === sel.sessionId && quote !== "") insertIntoComposer(targetId, quote, "block", sel.view);
      return;
    }
    const kind = target.ui === "chat" ? "chat" : "terminal";
    // A reference never lands out of sight: surface the target agent first,
    // splitting beside the selection's own pane when it is not open anywhere.
    if (sessionPaneId(layout, targetId) === null) {
      const beside =
        (sel.kind === "terminal" ? sessionPaneId(layout, sel.sessionId) : null) ??
        layout.focusedPaneId;
      layout = splitPane(layout, beside, "row");
      layout = openSession(layout, targetId);
    }
    if (sel.kind === "terminal") {
      const src = sessionsById.get(sel.sessionId);
      const name =
        displayNames.get(sel.sessionId) ?? (src !== undefined ? displayName(src) : "terminal");
      typeIntoSession(targetId, composeTerminalReference(name, sel.text));
      return;
    }
    const root = workspace?.root;
    const rel = root !== undefined ? workspaceRelative(sel.path, root) : sel.path;
    const crop = sel.crop;
    if (crop !== undefined && needsCropUpload(sel, kind)) {
      // Pixels for a terminal agent: a file in the session's landing pad
      // (bounded, pruned with the session), its path typed after the
      // locator. A failed upload still sends the locator (its chip says why).
      const name = `ref-${++refCropSeq}.png`;
      void trackFileOp("Sending the region image…", (progress) =>
        uploadToSession(targetId, crop, name, progress),
      ).then((up) => typeIntoSession(targetId, composeSelectionReference(rel, sel, kind, up?.path ?? null)));
      return;
    }
    const reference = composeSelectionReference(rel, sel, kind);
    if (crop !== undefined && kind === "chat") {
      // Chat sees the pixels as an attachment, through the same downscale
      // and payload caps as a pasted screenshot — delivered WITH the locator
      // in one insert, so both land in the same composer and leave in the
      // same message (a crop alone would reach the agent with no words).
      const label = sel.label;
      void imageToAttachment(crop).then((image) => {
        const attachment = image === null ? undefined : label !== undefined ? { ...image, label } : image;
        typeIntoSession(targetId, reference, attachment);
      });
      return;
    }
    typeIntoSession(targetId, reference);
  }

  /** Names each region image a terminal agent receives (`ref-1.png`, …);
   *  the landing pad dedupes a name that is already there. */
  let refCropSeq = 0;

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
  function insertUploadedPath(sessionId: string, absPath: string, image?: ImageAttachment): void {
    const s = sessionsById.get(sessionId);
    if (s === undefined || !s.alive) return;
    const root = workspace?.root;
    const text =
      s.kind === "agent"
        ? composeAgentPathReference(root !== undefined ? workspaceRelative(absPath, root) : absPath)
        : composeShellPathReference(absPath, s.cwd_current ?? s.cwd);
    typeIntoSession(sessionId, text, image);
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
    const folder = folderTargetAt(e.clientX, e.clientY);
    // dragover fires continuously (every pointer move, and on a timer while
    // still): rewrite dropSpot only on a logical change, or every event
    // re-derives the Finder/tree highlights for nothing.
    if (folder !== null) {
      if (e.dataTransfer !== null) e.dataTransfer.dropEffect = "copy";
      const next: DropSpot = {
        kind: "uploadDir",
        paneId: folder.paneId,
        dir: folder.dir,
        row: folder.row,
        rowKey: folder.rowKey,
      };
      if (!sameSpot(next, dropSpot)) dropSpot = next;
      return;
    }
    const paneId = paneIdAt(e.clientX, e.clientY);
    const ok = paneId !== null && osDropSession(paneId) !== null;
    if (e.dataTransfer !== null) e.dataTransfer.dropEffect = ok ? "copy" : "none";
    // OS drops reuse the dropSpot plumbing: the whole pane is the target
    // (HTML5 dnd has no competing tile gesture to partition against).
    const next: DropSpot | null = ok && paneId !== null ? { kind: "upload", paneId } : null;
    if (!sameSpot(next, dropSpot)) dropSpot = next;
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
    const folder = folderTargetAt(e.clientX, e.clientY);
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
      // host-side artifact the agent can re-read later. Both land in one
      // delivery, so the picture and its mention can never split.
      const s = sessionsById.get(sessionId);
      const image =
        s?.ui === "chat" && file.type.startsWith("image/") ? ((await imageToAttachment(file)) ?? undefined) : undefined;
      // Sequential: keeps multi-file reference order stable in the input.
      await uploadAndInsert(sessionId, file, file.name, image);
    }
  }

  // --- clickable paths: the bridge's return direction ------------------------

  /** Resolution context for a session's path links (terminal and chat). */
  function linkContext(id: string): LinkContext {
    const s = sessionsById.get(id);
    // The session's own workspace, else the active one — the same preference
    // order the root fallback uses, so root and workspaceId stay in step.
    const ws = workspaces.find((w) => w.id === s?.workspace_id) ?? workspace;
    return {
      cwd: s?.cwd_current ?? s?.cwd ?? null,
      // Scrollback and older messages were written from the spawn directory.
      spawnCwd: s?.cwd ?? null,
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
  /**
   * Open a proxyable URL as a browser pane FROM `fromPane`: reuse an existing
   * pane on the same host:port (clicking Jupyter's printed URL twice lands
   * where you already are), or split beside when Cmd/Ctrl is held — the same
   * grammar file links follow. Every link surface routes through here.
   */
  function openBrowserFromPane(paneId: string, target: UrlTarget, newSplit: boolean): void {
    if (newSplit) {
      layout = splitPane(layout, paneId, "row");
      layout = openTab(layout, freshBrowserTab(target.host, target.port, target.path));
    } else {
      layout = openBrowser(focusPane(layout, paneId), target.host, target.port, target.path);
    }
    // The browser surface took focus: pull DOM focus off whatever had it so
    // plain keys stop reaching a PTY that is no longer the focused view.
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** A proxyable URL link activated from a TERMINAL: anchor the open on the
   *  pane showing that session, not merely the focused one. */
  function onOpenUrl(id: string, target: UrlTarget, newSplit: boolean): void {
    const loc = paneForTab(layout.root, { surface: "terminal", sessionId: id });
    openBrowserFromPane(loc?.paneId ?? layout.focusedPaneId, target, newSplit);
  }

  /**
   * An agent asked to show the user a web app (`open_browser`). Unlike a
   * click, nobody here asked for it: the pane opens beside the agent and
   * focus — layout and DOM — stays exactly where the user is typing, so no
   * blur here (`placeAgentBrowser` hands the focused pane back). A layout not
   * restored yet has nothing to anchor on; the frame is not queued.
   */
  function onAgentBrowserOpen(open: AgentBrowserOpen): void {
    if (!layoutReady) return;
    const view = {
      layout,
      workspaceId: activeWsId,
      visible: document.visibilityState === "visible",
    };
    if (!shouldActOnAgentBrowserOpen(open, view)) return;
    layout = placeAgentBrowser(layout, open);
  }

  /** The Mod2+B chord / a manual open: a blank browser pane (address entry). */
  function newBrowserSurface(): void {
    layout = openTab(layout, freshBrowserTab("", 0, "/"));
  }

  /** A confirmed terminal path link was activated: anchor the open on the
   *  pane showing that session, not merely the focused one. */
  function onOpenPath(
    id: string,
    path: string,
    kind: PathKind,
    opts: { split: boolean; reveal?: Reveal },
  ): void {
    const loc = paneForTab(layout.root, { surface: "terminal", sessionId: id });
    openPath(path, kind, { ...opts, fromPane: loc?.paneId });
  }

  /**
   * The workbench's path opener (registered with `shared/openPath.ts`; every
   * link surface calls `openPath`). Directory names open in the Finder
   * (browsing anywhere, in or out of the workspace); an in-workspace dir is
   * ALSO revealed in the FILES side-tree (revealInTree is a no-op outside the
   * root). Files follow `openFileFromPane`; `openPath` has already requested
   * the line reveal, which the file view takes once it shows the path.
   */
  function openPathInLayout(path: string, kind: PathKind, opts: OpenPathOptions): void {
    const source = opts.documentFrom !== undefined
      ? paneForTab(layout.root, { surface: "file", path: opts.documentFrom }) : null;
    // A document gesture captures its pane before validation. Older callers
    // supplying only its path still anchor on that view, never on later focus.
    const from = opts.fromPane ?? source?.paneId;
    const fromPane =
      from !== undefined && findPane(layout.root, from) !== null ? from : layout.focusedPaneId;
    const split = opts.split === true;
    if (kind === "dir") {
      openDirInFinder(fromPane, path, split);
      revealInTree(path);
      return;
    }
    if (!split && source?.paneId === fromPane && findPane(layout.root, fromPane)?.active === source.index) {
      layout = openFileLink(pinPaths(layout, get(dirtyFiles)), source.paneId, path);
      if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
    } else {
      openFileFromPane(fromPane, path, split);
    }
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
    layout = pinPaths(layout, get(dirtyFiles));
    const existing = paneForTab(layout.root, { surface: "file", path });
    if (existing !== null) {
      layout = activateTab(layout, existing.paneId, existing.index);
      if (pinned) layout = pinTab(layout, existing.paneId, existing.index);
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
  function openDiffFromPane(
    paneId: string,
    path: string,
    mode: DiffMode,
    newSplit: boolean,
    keep = false,
  ): void {
    openGitFromPane(paneId, { surface: "diff", path, mode }, newSplit, keep);
  }

  /** Open (or focus) any git view beside `paneId` — the openDiffFrom rule:
   *  the adjacent pane, or a fresh split when there is none / `newSplit`.
   *  A single click opens a PREVIEW tab (one slot per pane, like files: the
   *  next click replaces it); `keep` (a double-click, or a tab carrying
   *  `preview: false`) keeps it. */
  function openGitFromPane(paneId: string, tab: Tab, newSplit: boolean, keep = false): void {
    if (tab.surface !== "diff" && tab.surface !== "gitx") {
      layout = openTab(layout, tab);
      return;
    }
    const view: DiffTab | GitDetailTab = tab;
    const kept = keep || view.preview === false;
    const existing = paneForTab(layout.root, view);
    if (existing !== null) {
      layout = activateTab(layout, existing.paneId, existing.index);
      if (kept) layout = pinTab(layout, existing.paneId, existing.index);
    } else {
      const neighbor = newSplit ? null : adjacentPane(layout, paneId);
      if (neighbor !== null) {
        layout = openTabAs(focusPane(layout, neighbor), view, !kept);
      } else {
        layout = splitPane(layout, paneId, "row");
        layout = openTabAs(layout, view, !kept);
      }
    }
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** A commit dropped on an agent's pane: referenced there like a file (a
   *  shell gets the short sha). */
  function referenceCommitDrop(paneId: string, tab: Tab): void {
    if (tab.surface !== "gitx" || tab.view !== "commit" || tab.sha === undefined) return;
    const p = findPane(layout.root, paneId);
    const active = p?.tabs[p.active];
    if (active === undefined || active.surface !== "terminal") return;
    const s = sessionsById.get(active.sessionId);
    if (s === undefined || !s.alive) return;
    const text =
      s.kind === "agent"
        ? composeCommitReference(
            { kind: "commit", sha: tab.sha, subject: tab.title ?? "", repo: tab.repo },
            workspace?.root ?? null,
          )
        : `${tab.sha.slice(0, 7)} `;
    typeIntoSession(active.sessionId, text);
  }

  /** "Reference in chat" on a commit: it lands in the agent references go to
   *  (surfaced first, beside the focused pane when it isn't open). */
  function referenceCommitNow(sel: CommitSelection): void {
    const target = refTargetSession;
    if (target === null) return;
    if (sessionPaneId(layout, target.id) === null) {
      layout = splitPane(layout, layout.focusedPaneId, "row");
      layout = openSession(layout, target.id);
    }
    typeIntoSession(target.id, composeCommitReference(sel, workspace?.root ?? null));
  }

  /** Knowledge at one entry, beside the source pane (a chat's id chip): an
   *  open Knowledge tab takes the request (moved beside when it hides behind
   *  the source or a split was asked for); else the adjacent pane, or a split
   *  when the source stands alone. The view takes the focus request when it
   *  shows. */
  function openKnowledgeFromPane(paneId: string, ekey: string, newSplit: boolean): void {
    focusKnowledgeEntry(ekey);
    if (activeWsId === null || !layoutReady) return;
    const existing = paneForTab(layout.root, { surface: "knowledge" });
    const source = findPane(layout.root, paneId);
    const hiddenBehind =
      existing !== null && existing.paneId === paneId && source !== null && source.active !== existing.index;
    if (existing !== null && (hiddenBehind || newSplit)) {
      // Knowledge sits behind the source in its own pane, or the click asked
      // for a split: move the one Knowledge tab beside the source, so what
      // the id was clicked in (a chat) stays in view.
      layout = dropTab(layout, { surface: "knowledge" }, paneId, "right");
    } else if (existing !== null) {
      layout = activateTab(layout, existing.paneId, existing.index);
    } else {
      const neighbor = newSplit ? null : adjacentPane(layout, paneId);
      if (neighbor !== null) {
        layout = openKnowledge(focusPane(layout, neighbor));
      } else {
        layout = splitPane(layout, paneId, "row");
        layout = openKnowledge(layout);
      }
    }
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** Knowledge's "Ask an agent": the drafted request lands in the agent the
   *  user works with, surfaced beside Knowledge when it isn't open. Never
   *  sends — the user reads the draft first. */
  function askAgentFrom(paneId: string, text: string): string | null {
    const target = refTargetSession;
    if (target === null) return null;
    if (sessionPaneId(layout, target.id) === null) {
      layout = splitPane(layout, paneId, "row");
      layout = openSession(layout, target.id);
    }
    if (target.ui === "chat") {
      const loc = paneForTab(layout.root, { surface: "terminal", sessionId: target.id });
      if (loc !== null) layout = activateTab(layout, loc.paneId, loc.index);
      insertIntoComposer(target.id, text, "block");
    } else {
      // A terminal input submits on a newline: the draft goes in as one line.
      typeIntoSession(target.id, text.replace(/\s*\n\s*/g, " "));
    }
    return displayNames.get(target.id) ?? displayName(target);
  }

  /** Open (or focus) one of a chat's subagents as a read-only view of its
   *  own — a tab next to the chat it belongs to (it is that chat's work, not
   *  a neighbour's), or a split beside it on a modified click. */
  function openSubagentFromPane(
    paneId: string,
    sessionId: string,
    agentId: string,
    title: string,
    newSplit: boolean,
  ): void {
    const existing = paneForTab(layout.root, { surface: "subagent", sessionId, agentId, title });
    if (existing !== null) {
      layout = activateTab(layout, existing.paneId, existing.index);
      return;
    }
    if (newSplit) layout = splitPane(layout, paneId, "row");
    else layout = focusPane(layout, paneId);
    layout = openSubagent(layout, sessionId, agentId, title);
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

  /** Restore this (window, workspace)'s layout from the daemon; anything
   *  missing/invalid (including a not-yet-upgraded daemon) falls back to the
   *  default. Blobs written before per-workspace keys are read from the bare
   *  window key when they match the active workspace (windowPersistence.ts). */
  function bootViewState(): Promise<void> {
    return viewPersistence.boot();
  }

  /** Drop restored file tabs whose files are definitively gone (400/404);
   *  an unreachable or not-yet-upgraded daemon never wipes tabs. */
  function pruneDeadFiles(): void {
    void pruneDeadFileTabs(
      () => layout,
      (next) => {
        layout = next;
      },
      fsProbe,
    );
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
    // Reload Window's F5 (native Windows/Linux). The menu accelerator carries
    // it, but WebView2 reloads on F5 itself (wry leaves its browser
    // accelerator keys on) and can see the key first: claiming it here lands
    // both paths in the safety gate — ahead of the modal stand-down, which
    // would otherwise hand F5 to WebView2's ungated reload. A browser tab
    // keeps its own F5.
    if (isNativeShell() && isReloadWindowKey(e)) {
      e.preventDefault();
      e.stopPropagation();
      requestWindowReload();
      return;
    }
    // A shown modal owns the keyboard: workbench chords stand down behind it
    // (the picker and Quick Open handle their own chords below).
    if (modalOpen() && !pickerOpen && !quickOpenOpen) return;
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
    if (homeSettingsOpen && e.key === "Escape" && !escapeBelongsElsewhere(e.target)) {
      intercept();
      homeSettingsOpen = false;
      return;
    }
    if (hit?.id === "settings" && activeWsId === null) {
      intercept();
      openSettingsSurface();
      return;
    }
    if (activeWsId === null || !layoutReady) return;

    if (hit?.id === "quickOpen") {
      intercept();
      if (quickOpenOpen) closeQuickOpen();
      else openQuickOpen();
      return;
    }
    if (quickOpenOpen) return;

    if (hit?.id === "find") {
      if (!e.isComposing && findInFocusedPane("open")) intercept();
      return;
    }
    if (hit === null) {
      const terminal = findLayer()?.querySelector(":scope > .term-view") != null;
      const command = findNavigation(e, terminal);
      if (command !== null && findInFocusedPane(command)) {
        intercept();
        return;
      }
    }

    if (hit === null) {
      const destination = chordDigit(e, modifierSetting(), true);
      if (destination !== null && destination >= 1 && destination <= 9) {
        intercept();
        const target = panesOf(layout.root).find((p) => p.number === destination);
        const source = findPane(layout.root, layout.focusedPaneId);
        if (target !== undefined && source !== null) ctrl.moveTabToPane(source.id, source.active, target.id);
        else showFlash(`Open pane ${destination} first${keyHint("newPane") ? ` (${keyHint("newPane")})` : ""}.`);
        return;
      }
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
      // Pinned Mod+1–9 focuses the numbered pane; Mod+0 is the dashboard.
      const n = chordDigit(e, modifierSetting());
      if (n === 0 && detachedWindow) {
        // A solo window does not pull in the workspace dashboard.
        intercept();
        return;
      }
      if (n === 0) {
        intercept();
        openDashboardSurface();
      } else if (n !== null && n >= 1) {
        intercept();
        const pane = panesOf(layout.root).find((p) => p.number === n);
        if (pane !== undefined) {
          layout = focusPane(layout, pane.id);
          focusFocusedPane();
        }
      }
      return;
    }

    // Arrow chords in an editable surface belong to the text caret (rename
    // fields, search boxes, the file editor) — xterm's hidden textarea is
    // exempt, terminals don't use modifier-arrows for editing. The chat
    // composer is the one editable that also gives pane focus away, but only
    // when a pane lies that way: with none, the caret keeps ⌘←/→ (line
    // start/end), so a single-pane window loses nothing.
    if (hit.dir !== null && isEditableTarget(e.target)) {
      const leaves =
        hit.id === "focusArrows" &&
        e.target instanceof HTMLElement &&
        e.target.hasAttribute("data-pane-arrows") &&
        moveFocus(layout, hit.dir as FocusDir).focusedPaneId !== layout.focusedPaneId;
      if (!leaves) return;
    }

    switch (hit.id) {
      case "newPane":
        intercept();
        newPaneAt(layout.focusedPaneId);
        return;
      case "fileBack":
      case "fileForward": {
        if (isEditableTarget(e.target)) return;
        const pane = findPane(layout.root, layout.focusedPaneId);
        if (pane?.tabs[pane.active]?.surface === "file") {
          intercept();
          ctrl.navigateFileHistory(layout.focusedPaneId, hit.id === "fileBack" ? -1 : 1);
        }
        return;
      }
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
        launcher.newAgentPrimary();
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
        // A detached solo window has no rail to reveal — it is just its pane
        // (the button is gone too; re-attach is the way back to the full
        // workbench). Without this gate the chord would strand the window
        // rail-open with no strip.
        if (detachedWindow) {
          showFlash("this is a detached window — re-attach to get the full workbench");
          return;
        }
        layout = { ...layout, focusMode: !layout.focusMode };
        return;
      case "mastermind":
        intercept();
        toggleMastermindPanel();
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
      case "cycleAgents":
      case "cycleTerminals":
        intercept();
        cycleRail(hit.id === "cycleAgents");
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

  /**
   * Escape on the Home settings page closes it only when nothing closer owns
   * the key: a field holding text, a select, an editor, or an open dialog. An
   * empty field — Settings focuses its search box on open — still closes.
   * This handler runs in the capture phase, before any of those see the key.
   */
  function escapeBelongsElsewhere(t: EventTarget | null): boolean {
    const choice = ["checkbox", "radio", "button", "submit", "reset", "range", "color", "file"];
    if (t instanceof HTMLTextAreaElement || (t instanceof HTMLInputElement && !choice.includes(t.type))) return t.value !== "";
    if (t instanceof HTMLSelectElement) return true;
    if (!(t instanceof HTMLInputElement) && isEditableTarget(t)) return true;
    const modal = "dialog[open], [role='dialog'], [role='alertdialog'], [aria-modal='true']";
    if (t instanceof Element && t.closest(modal) !== null) return true;
    return document.querySelector("dialog[open], [aria-modal='true']") !== null;
  }

  $effect(() => {
    // The workspace leads; a remote window wears its host so a wall of similar
    // windows is legible:
    //   "my_project •cluster | chimaera"  (remote, in a workspace)
    //   "my_project | chimaera"           (local — the host is implicit)
    // Home (no workspace) drops the workspace but keeps the host when remote.
    // A compute-node daemon (the snapshot's `self` — daemon truth, not the
    // URL hash) appends its node: "my_project •cluster › n042 | …",
    // so a job window never poses as its login node.
    const node = $computeStatus?.self?.node;
    const hostWithNode = node ? `${hostAlias} › ${node}` : hostAlias;
    const host = isRemoteWindow ? hostWithNode : null;
    let scope = workspace
      ? host
        ? `${workspace.name} •${host}`
        : workspace.name
      : (host ?? "");
    // A detached solo window is named for what it shows: the tab leads, the
    // workspace scope trails — "claude (2) — my_project | chimaera".
    if (detachedWindow) {
      const only = tabCount(layout) === 1 ? panesOf(layout.root)[0]?.tabs[0] : undefined;
      const p = findPane(layout.root, layout.focusedPaneId);
      const shown = only ?? p?.tabs[p.active];
      if (shown !== undefined) scope = scope ? `${tabLabel(shown)} — ${scope}` : tabLabel(shown);
    }
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
    // A folder opened here would belong to this job's chimaera alone, not
    // the cluster: a cluster workspace's window picks from the cluster page.
    if (clusterWs !== null) {
      openClusterHome();
      return;
    }
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
      // Sibling effects flush in declaration order: the asset navigation
      // above can issue its reload in the very flush that cleared the last
      // dirty file, before this listener is torn down. Decide on the live set
      // so a clean document never prompts.
      if (get(dirtyFiles).size === 0) return;
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  });

  // The native app's close/quit guard: a webview gets no beforeunload when
  // its window closes or the app quits, so the shell decides from the
  // unsaved count this window pushes (no round trip, and no prompt when
  // nothing is unsaved) and asks here when it holds one.
  const unsavedCount = $derived($dirtyFiles.size);
  $effect(() => {
    if (!isNativeShell()) return;
    void reportUnsaved(unsavedCount).catch(() => {});
  });
  $effect(() => {
    if (!isNativeShell()) return;
    return asyncDisposer(onUnsavedPrompt((p) => windowClose.receive(p)));
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
    closePicker();
    homeSettingsOpen = false;
    createError = null;
    // Stamp recency for the home screen (fire-and-forget; old daemons 404).
    void touchWorkspace(w.id).catch(() => {});
    const switched = activeWsId !== w.id;
    if (!switched) return;
    // Home is the new-window launcher, not a permanent identity attached to
    // this native window. Once it enters a workspace, a later New Window must
    // be able to create a fresh Home without replacing this workbench (the
    // shell may reclaim it if the workspace later disappears). Report that
    // promotion at the selection boundary: leaving it to the reactive
    // fire-and-forget scope report above lets an immediate native New Window
    // still observe (and merely focus) the stale Home launcher.
    if (activeWsId === null) {
      if (isNativeShell()) {
        const promotedLabel = w.name + (isRemoteWindow ? ` •${hostAlias}` : "");
        try {
          await reportWindowScope(scopeAlias, w.id, promotedLabel);
        } catch (error) {
          console.error("could not promote Home to a workspace window", error);
          return;
        }
      }
      leaveHomeHub();
    }
    // A detached window deliberately switching workspaces is CONVERTING to a
    // full workbench — the user asked this window to show something else.
    // Clearing dt here (client, hint, and shell/record via the identity
    // report) is what stops the switch from either self-closing the window
    // (dt over an empty restore) or restoring the workspace mirror as a
    // silent clone while the shell still routes around it as detached; the
    // outgoing blob is rewritten without dt (or switching BACK would
    // resurrect solo mode). Then the outgoing workspace's pending layout
    // write is flushed under its own key — all before activeWsId changes.
    viewPersistence.leaveWorkspace();
    activeWsId = w.id;
    setActiveWorkspaceId(w.id);
    // layoutReady drops BEFORE the default layout lands, or the persist
    // effect would write the default under the incoming workspace's key.
    viewPersistence.resetForBoot();
    autoOpened = false;
    void bootViewState();
  }

  /** Home screen: unregister a workspace (the folder itself is untouched). */
  function removeWorkspace(w: Workspace): void {
    workspaces = workspaces.filter((x) => x.id !== w.id);
    // Its running sessions end with it: the daemon's unregister leaves them
    // alive but orphaned (no workspace row to find them from).
    void stopWorkspace(w)
      .then(() => deleteWorkspace(w.id))
      .catch(() => {
        // already gone or unreachable; the refresh below reconciles
      })
      .finally(refreshWorkspaces);
  }

  /** Tombstones, first-known created_at and the previous agent set
   *  (workspace/sessionRoster.ts); `applySessions` keeps the writes. */
  const roster = new SessionRoster();

  /**
   * Swap in a sessions list. `snapshot` marks a list that came FROM THE
   * DAEMON (events frame, poll, refetch) as opposed to a local optimistic
   * drop (`tombstone`, `onExited`): only a daemon snapshot can confirm a
   * tombstoned session is gone — a locally filtered list lacks the id by
   * construction and must not lift its own tombstone.
   */
  function applySessions(incoming: Session[], snapshot = true): void {
    // Tombstoned (optimistically killed) rows stay out until a daemon
    // snapshot confirms the retire; rail order is first-known created_at.
    const { list, agentsChanged } = roster.reconcile(incoming, snapshot);
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
      else if (s.ui === "term") {
        chatPool.disposeChat(s.id);
        // Its subagent views read through that chat: without it they could
        // neither follow nor tell working from finished. They go with it.
        layout = pruneTabs(layout, (t) => t.surface !== "subagent" || t.sessionId !== s.id);
      }
    }
    // A vanished agent just retired into the workspace's recents; an appeared
    // one may be a resumed conversation leaving them.
    if (agentsChanged) recentsRail.refresh();
    pruneAndAutoOpen();
  }

  /**
   * Sessions created by this window in the last few seconds. A sessions
   * snapshot fetched BEFORE the create but arriving AFTER it would otherwise
   * prune the fresh tab right out of the layout (stale-poll race).
   */
  const recentlyCreated = new RecentSessions(pruneAndAutoOpen);
  $effect(() => () => recentlyCreated.dispose());
  /** Sessions being optimistically killed: dropped locally at once, tombstoned
   *  so a snapshot can't re-add the dying row before the daemon's stop
   *  completes. A tombstone lifts only when a snapshot arrives WITHOUT the id
   *  (the daemon has reaped and retired it) — NOT when the DELETE resolves:
   *  DELETE only signals the process (SIGHUP) and returns while the session
   *  is still in the roster until its wait thread reaps it, so clearing on
   *  the response let the row pop back for a beat and vanish again — a
   *  visible flicker over a remote link, where the round trip and the exit
   *  both take longer. `KILL_TOMBSTONE_MS` after the request settles a
   *  lingering tombstone is dropped and the roster refetched, so a kill that
   *  genuinely failed shows the truth instead of hiding the row forever.
   *  The tombstone set is `roster.killing`. */
  const KILL_TOMBSTONE_MS = 20_000;

  /** Tombstone `ids` and drop them from the local roster in ONE re-apply
   *  (a workspace stop kills many at once). */
  function tombstone(ids: readonly string[]): void {
    roster.bury(ids);
    for (const id of ids) recentlyCreated.delete(id);
    applySessions(
      sessions.filter((s) => !ids.includes(s.id)),
      false,
    );
  }

  /** The kill requests settled: keep the tombstones until a snapshot
   *  confirms each session is gone, with one bounded fallback for the batch
   *  (see `killing`). */
  function armTombstoneExpiry(ids: readonly string[]): void {
    window.setTimeout(() => {
      if (!roster.expire(ids)) return;
      listSessions().then(applySessions, () => {
        // unreachable daemon: the next snapshot/poll reconciles
      });
    }, KILL_TOMBSTONE_MS);
  }

  /**
   * Once both the persisted layout and the first session snapshot are in:
   * drop tabs whose sessions vanished (also on every later snapshot), and —
   * exactly once — populate a pristine layout with the first session.
   */
  function pruneAndAutoOpen(): void {
    if (!layoutReady || !gotSessions) return;
    const live = new Set(sessions.map((s) => s.id));
    recentlyCreated.protect(live);
    layout = pruneSessions(layout, live);
    // The one-shot waits for the settings to actually load: getSetting
    // returns the schema default ("auto") until GET /settings resolves, and
    // the REST fallback poll can deliver sessions first — latching here on
    // the default would override an explicit "never". loadSettings flips
    // loaded even on failure (defaults then genuinely apply) and re-calls
    // this, so the gate can never wedge the landing.
    if (!autoOpened && settingsLoaded() && !detachedWindow) {
      // A detached window never auto-opens the dashboard: emptied (its
      // session died, or its tabs moved back) it closes itself instead.
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

  // A detached window with nothing left to show closes itself: its one job
  // was the torn-off tabs, and "close the view, never the session" makes the
  // close safe. gotSessions gates the boot window (an empty default layout
  // before restore must not kill the window). The browser can refuse
  // window.close on a tab it didn't script-open — then the empty state
  // renders and offers a manual close.
  $effect(() => {
    if (!detachedWindow || !layoutReady || !gotSessions || tabCount(layout) !== 0) return;
    void flushViewState().then(() => {
      if (isNativeShell()) closeThisWindow();
      else window.close();
      setTimeout(() => (detachedEmpty = true), 800);
    });
  });

  function onTitle(id: string, title: string): void {
    const s = sessions.find((x) => x.id === id);
    if (s) s.title = title;
  }

  function onExited(id: string, _status: number | null): void {
    recentlyCreated.delete(id);
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
    applySessions(
      sessions.filter((s) => s.id !== id),
      false,
    );
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

  /**
   * A notification was clicked for `sessionId`: show it here. Works before
   * this window knows the session (a window a click just opened) — the id
   * waits for the roster, then for the layout, like a worktree reveal.
   */
  function focusFromNotification(sessionId: string): void {
    const s = sessionsById.get(sessionId);
    if (s === undefined) {
      if (!gotSessions) pendingNoticeFocus = sessionId;
      return;
    }
    if (s.workspace_id !== activeWsId) {
      void revealWorktreeSession(sessionId, s.workspace_id);
      return;
    }
    if (!layoutReady) {
      pendingReveal = sessionId;
      return;
    }
    openSess(sessionId);
  }
  let pendingNoticeFocus = $state<string | null>(null);
  $effect(() => {
    if (!gotSessions || pendingNoticeFocus === null) return;
    const id = pendingNoticeFocus;
    pendingNoticeFocus = null;
    untrack(() => focusFromNotification(id));
  });

  // A file the shell asked this window to show (`open=` in the hash — a
  // cluster file peek copied to this machine): once per page load, after the
  // workspace view has loaded, as a preview tab in the focused pane — and
  // only a path inside this workspace that exists.
  let openRequest: string | null = takeOpenRequest();
  $effect(() => {
    if (openRequest === null || !layoutReady || workspace === null) return;
    const path = openRequest;
    openRequest = null;
    // Inside this workspace only (the shell copies peeks into the folder it
    // opened this window on); a `..` segment (either separator, for a Windows
    // daemon) can't climb back out.
    const rel = relativeTo(path, workspace.root);
    if (rel === null || rel === "." || rel.split(/[\\/]/).includes("..")) return;
    void fsProbe(path).then((found) => {
      if (found === "ok") untrack(() => openFilePath(path));
    });
  });

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

  /** Settings opened from Home: a full page over Home (no workspace, so no
   *  layout tab to hold it). Its view loads on demand like a pane's. Home
   *  stays mounted underneath, parked (hidden + inert): the cluster page, an
   *  open Add-machine form and a connect's progress line are still there on
   *  return, and Home's mount-time fetches don't run again. */
  let homeSettingsOpen = $state(false);
  let homeSettingsLoad = $state<ReturnType<typeof loadPaneView> | null>(null);
  // Workspace windows defer Home until it is visible. The shared view cache
  // retains successes; the same asset recovery used by panes owns retries.
  let homeLoad = $state<ReturnType<typeof loadPaneView> | null>(null);
  $effect(() => {
    if (activeWsId !== null) return;
    // One-shot: homeLoad is read and written untracked so the effect depends
    // on activeWsId alone and can never re-run on its own write.
    untrack(() => {
      if (homeLoad === null) homeLoad = loadPaneView("home");
    });
  });

  /** Open/focus the settings surface (gear button, ⌘,). In a workspace it is
   *  a layout tab; on Home it opens as Home's Settings page. */
  function openSettingsSurface(): void {
    if (activeWsId === null) {
      homeSettingsLoad = loadPaneView("settings");
      homeSettingsOpen = true;
      return;
    }
    if (!layoutReady) return;
    layout = openSettings(layout);
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  // A surface that can't reach these callbacks (a chat's usage popover)
  // asks for Settings at a section through the store.
  $effect(() => {
    const section = $settingsOpen;
    if (section === null) return;
    untrack(() => {
      settingsOpen.set(null);
      openSettingsAt(section);
    });
  });

  /** Open/focus the workspace dashboard (rail row, ⌘0, the landing default). */
  function openDashboardSurface(): void {
    if (activeWsId === null || !layoutReady) return;
    layout = openDashboard(layout);
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** Open/focus All sessions (the Recents header, the dashboard's usage
   *  line, quick-open). */
  function openSessionsSurface(): void {
    if (activeWsId === null || !layoutReady) return;
    layout = openSessionsList(layout);
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** Open Settings at Activity (the dashboard's activity line, quick-open). */
  function openActivitySurface(): void {
    openSettingsSurface();
    requestSettingsSection("Activity");
  }

  /** Open Settings scrolled to one setting's row (the Mastermind panel's
   *  "agent communication is off" state). */
  function openSettingsAt(settingId: string): void {
    openSettingsSurface();
    requestSettingsSection(settingId);
  }

  /** Open/focus the workspace Timeline (dashboard link, quick-open). */
  function openTimelineSurface(): void {
    if (activeWsId === null || !layoutReady) return;
    layout = openTimeline(layout);
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** Open/focus Knowledge (rail row when a provider is active, dashboard
   *  link, quick-open). */
  function openKnowledgeSurface(): void {
    if (activeWsId === null || !layoutReady) return;
    layout = openKnowledge(layout);
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** Open/focus the Extensions tab (the `plugins` surface: quick-open, the
   *  dock row, the attach affordances). */
  function openPluginsSurface(): void {
    if (activeWsId === null || !layoutReady) return;
    layout = openPlugins(layout);
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  }

  /** Open/focus a plugin's tab view (its card, a screen's `open-view`, a
   *  file action's answer). */
  function openPluginViewSurface(plugin: string, view: string): void {
    if (activeWsId === null || !layoutReady) return;
    layout = openPluginViewTab(layout, plugin, view);
  }

  /** Quick-open commands: the workspace surfaces that have no file or session
   *  to match on ("Timeline", "Knowledge", "Extensions" — which "plugins" and
   *  "skills" still find). */
  // Knowledge is offered only while a knowledge plugin is on here (it is
  // that plugin's view; without one there is nothing to open).
  const quickOpenCommands = $derived([
    { id: "new-pane", label: "New empty pane", aliases: ["split"], hint: keyHint("newPane"), run: () => newPaneAt(layout.focusedPaneId) },
    { id: "find", label: "Find in current pane", aliases: ["search"], hint: keyHint("find"), run: () => {
      const pane = quickOpenRestoreEl?.closest(".pane");
      const find = targetIn(pane?.querySelector<HTMLElement>(".layer.active") ?? findLayer());
      quickOpenRestoreEl = null;
      void tick().then(() => {
        if (!find?.("open")) showFlash("Find is not available in this view.");
      });
    } },
    { id: "timeline", label: "Timeline", hint: "what happened", run: openTimelineSurface },
    {
      id: "sessions",
      label: "All sessions",
      aliases: ["history"],
      hint: "every past session",
      run: openSessionsSurface,
    },
    {
      id: "activity",
      label: "Activity",
      aliases: ["usage", "tokens"],
      hint: "sessions and tokens, every workspace",
      run: openActivitySurface,
    },
    ...($knowledgeProviderActive
      ? [{ id: "knowledge", label: "Knowledge", hint: "what your agents recorded", run: openKnowledgeSurface }]
      : []),
    {
      id: "plugins",
      label: "Extensions",
      aliases: ["plugins", "skills"],
      hint: "plugins and skills",
      glyph: "extensions" as const,
      run: openPluginsSurface,
    },
    {
      id: "mastermind",
      label: "Mastermind",
      hint: "the workspace's Mastermind panel",
      run: () => setMastermindPanelOpen(true),
    },
    ...gitQuickOpenCommands(),
    {
      id: "reload-window",
      label: "Reload Window",
      aliases: ["refresh"],
      hint: "a fresh page, same layout",
      run: requestWindowReload,
    },
  ]);

  /** Git's commands: only where a repository is (git is ambient). */
  function gitQuickOpenCommands() {
    if ($gitRepos.length === 0) return [];
    const file = focusedFilePath;
    return [
      { id: "git", label: "Source Control", aliases: ["git", "changes"], hint: "changes and branches", run: openGitPanel },
      { id: "git-history", label: "History", aliases: ["log", "commits"], hint: "recent commits", run: () => openRepoHistory() },
      ...(file !== null && repoForPath($gitRepos, file) !== null
        ? [
            {
              id: "git-file-history",
              label: "File history",
              hint: basename(file),
              run: () => openFileHistory(file),
            },
          ]
        : []),
    ];
  }

  /** Put DOM focus where the focused pane takes keys — its terminal, else the
   *  pane root — so typing after a chord lands in the pane the chord chose
   *  rather than in whatever held focus before (a composer, an xterm). */
  function focusFocusedPane(): void {
    const sid = focusedSessionOf(layout);
    const paneId = layout.focusedPaneId;
    void tick().then(() => {
      if (sid !== null) pool.focusTerminal(sid);
      else paneRootEl(paneId)?.focus();
    });
  }

  function focusDirection(dir: FocusDir): void {
    layout = moveFocus(layout, dir);
    focusFocusedPane();
  }

  function cycle(delta: number): void {
    layout = cycleTab(layout, delta);
    focusFocusedPane();
  }

  /** ⌃⌘A / ⌃⌘T: open the next agent (or terminal) in the rail's own order. */
  function cycleRail(agents: boolean): void {
    // A detached window has no roster to step through (its chips are gone).
    if (detachedWindow) return;
    const group = agents ? agentSessions : shellSessions;
    const next = nextInGroup(
      group.map((s) => s.id),
      focusedSessionId,
    );
    if (next !== null) {
      openSess(next);
      return;
    }
    const make = keyHint(agents ? "newAgent" : "newTerminal");
    showFlash(`No ${agents ? "agents" : "terminals"} in this workspace yet${make ? ` — ${make} starts one` : ""}.`);
  }

  function split(dir: SplitDir): void {
    splitAt(layout.focusedPaneId, dir);
  }

  function newPaneAt(paneId: string): void {
    splitAt(paneId, newPaneSplitDirection(layout));
  }

  /** Split `paneId`; focus lands INSIDE the new pane on a real focusable
   *  target (the pane root), so chords and Escape keep working. */
  function splitAt(paneId: string, dir: SplitDir): void {
    if (panesOf(layout.root).length >= MAX_PANES) {
      showFlash(`All ${MAX_PANES} panes are open — move a tab into one of them.`);
      return;
    }
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
    const active = p.tabs[p.active];
    if (active !== undefined) {
      // A dirty file asks first (and keeps its tab until answered).
      if (!closeTabs([active])) return;
    } else {
      layout = closePane(layout, paneId);
    }
    const sid = focusedSessionOf(layout);
    const focusedId = layout.focusedPaneId;
    // After the flush: closing a pane restructures the tree, which can
    // re-parent (and blur) the surviving terminal — focus once it settles.
    void tick().then(() => {
      if (sid !== null) pool.focusTerminal(sid);
      else paneRootEl(focusedId)?.focus();
    });
  }

  /** Detach these tabs wherever they are now (views only; sessions live on). */
  function detachTabs(tabs: readonly Tab[]): void {
    let next = layout;
    for (const t of tabs) {
      const loc = paneForTab(next.root, t);
      if (loc !== null) next = detachTab(next, loc.paneId, loc.index);
    }
    layout = next;
  }

  /**
   * Close tabs by identity. Clean ones go at once; a file with unsaved edits
   * joins the "save changes?" dialog and keeps its tab until answered — its
   * buffer would survive a plain close (the buffer store keeps dirty text),
   * but a close gesture must never leave edits stranded without asking.
   * True when everything closed right away.
   */
  function closeTabs(tabs: readonly Tab[]): boolean {
    const dirty = get(dirtyFiles);
    const held = tabs.filter((t) => t.surface === "file" && dirty.has(t.path));
    detachTabs(tabs.filter((t) => !held.includes(t)));
    if (held.length === 0) return true;
    const known = new Set(pendingClose.map(tabKey));
    pendingClose = [...pendingClose, ...held.filter((t) => !known.has(tabKey(t)))];
    closeError = null;
    return false;
  }

  const pendingClosePaths = $derived(
    pendingClose.flatMap((t) => (t.surface === "file" ? [t.path] : [])),
  );

  /**
   * "Save": save each file; close those that saved, keep the rest asking.
   * Bounded: past the deadline (a dead link) it reports "not saved" and
   * hands control back while the save carries on in the background.
   */
  async function saveAndClose(): Promise<void> {
    if (closeSaving) return;
    closeSaving = true;
    closeError = null;
    const attempt = ++closeAttempt;
    const batch = pendingClose;
    const r = await saveDirtyFiles(
      batch.flatMap((t) => (t.surface === "file" ? [t.path] : [])),
      { deadlineMs: CLOSE_SAVE_DEADLINE_MS, cancelled: () => attempt !== closeAttempt },
    );
    // Cancelled while waiting: the tabs stay open, the dialog is already gone.
    if (r === null) return;
    closeSaving = false;
    const failed = batch.filter((t) => t.surface === "file" && r.unsaved.includes(t.path));
    detachTabs(batch.filter((t) => !failed.includes(t)));
    pendingClose = [...failed, ...pendingClose.filter((t) => !batch.includes(t))];
    if (failed.length > 0) {
      closeError = unsavedCloseError(
        failed.flatMap((t) => (t.surface === "file" ? [t.path] : [])),
        r.timedOut,
      );
    }
  }

  /** "Don't save": drop the edits (and their drafts), then close. */
  function discardAndClose(): void {
    for (const t of pendingClose) if (t.surface === "file") discardDirtyFile(t.path);
    detachTabs(pendingClose);
    pendingClose = [];
    closeError = null;
  }

  /** Also while saving: the tabs stay open and dirty, and a save already
   *  sent may still land in the background. */
  function cancelClose(): void {
    closeAttempt++;
    closeSaving = false;
    pendingClose = [];
    closeError = null;
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
          ? launcher.agents?.find((a) => a.id === (spawn.agent ?? "claude"))?.chatCapable
          : undefined;
      // A new agent follows the workspace's worktree choice; reopens,
      // resumes and explicit folders never do.
      const req = wantsWorktree(kind, spawn, workspace, workspaceIsRepo)
        ? { ...spawn, worktree: true }
        : spawn;
      const s = await createSession(activeWsId, kind, null, spawnSize(), req, chatCapable);
      recentlyCreated.add(s.id);
      // A racing events snapshot may already have delivered the session.
      if (needsAppend(sessions, s)) sessions.push(s);
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

  /** Every agent start from the launcher, a quick start or a Recents row. */
  function startAgent(spawn: AgentSpawn): void {
    void spawnSession("agent", spawn);
  }

  /** Flip the workspace's "new agents start in a new worktree" choice:
   *  optimistic, reverted when the daemon refuses. */
  async function toggleNewInWorktree(on: boolean): Promise<void> {
    const id = activeWsId;
    if (id === null) return;
    const setLocal = (v: boolean) => {
      workspaces = workspaces.map((w) => (w.id === id ? { ...w, new_in_worktree: v } : w));
    };
    const prev = workspaces.find((w) => w.id === id)?.new_in_worktree === true;
    setLocal(on);
    try {
      const w = await setNewInWorktree(id, on);
      workspaces = workspaces.map((x) => (x.id === id ? { ...x, ...w } : x));
    } catch (e) {
      setLocal(prev);
      createError = e instanceof ApiError ? e.message : "could not save the worktree choice";
    }
  }

  // --- a managed worktree left behind (session ended / conversation archived) ---

  /** The "kept" notice under the agents (workspace/worktreeNotice.svelte.ts).
   *  Its effects — a workspace switch drops another workspace's notice, and
   *  the timer teardown — are declared here, before the Recents refetch. */
  const keptWorktree = new KeptWorktreeNotice({
    activeWsId: () => activeWsId,
    refreshRecents: () => recentsRail.refresh(),
    onError: (message) => (createError = message),
  });
  keptWorktree.track();

  // --- the rail's Recents section ---
  // A workspace switch collapses the section and refetches it.
  recentsRail.trackWorkspace();

  /** Kill the session's process on the daemon and drop it locally — OPTIMISTIC:
   *  the row/tab vanishes at once and the ChatView tears down immediately,
   *  while the daemon's stop + retire-to-recents runs in the background. The
   *  tombstone keeps every snapshot until the retire from re-adding the dying
   *  row, so there's no lingering half-dead state, and the recents entry
   *  appears once (via the recents epoch) instead of the row "popping up
   *  weirdly" between the kill and the retire. */
  async function killSession(id: string): Promise<void> {
    confirmKillId = null;
    const dying = sessions.find((x) => x.id === id);
    const kept = dying?.kind === "agent" ? (dying.managed_worktree ?? null) : null;
    if (dying !== undefined && kept !== null && !worktreeInUse(sessions, kept, id)) {
      keptWorktree.offer(dying.workspace_id, kept, dying.git?.branch ?? null);
    }
    chatPool.disposeChat(id); // stop its socket reconnecting right away
    tombstone([id]);
    try {
      await deleteSession(id);
    } catch {
      // already gone or unreachable — it's already dropped locally.
    } finally {
      armTombstoneExpiry([id]);
    }
  }

  /** End every live session in a workspace — the home-screen "stop". The
   *  workspace registration itself is untouched; only its running work ends.
   *  Same optimistic tombstones as a single kill, so the rollup doesn't
   *  flicker per session as each one is reaped. */
  async function stopWorkspace(w: Workspace): Promise<void> {
    const live = sessions.filter((s) => s.workspace_id === w.id && s.alive);
    if (live.length === 0) return;
    const ids = live.map((s) => s.id);
    for (const id of ids) chatPool.disposeChat(id);
    tombstone(ids);
    await Promise.allSettled(ids.map((id) => deleteSession(id)));
    armTombstoneExpiry(ids);
  }

  function startRename(s: Session): void {
    confirmKillId = null;
    railRename.begin(s.id, displayNames.get(s.id) ?? displayName(s));
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
    paneTargets() {
      return panesOf(layout.root).map((p) => ({ id: p.id, number: p.number! })).sort((a, b) => a.number - b.number);
    },
    newPaneAt(paneId) {
      newPaneAt(paneId);
    },
    moveTabToPane(paneId, index, targetPaneId) {
      layout = moveTabToPane(layout, paneId, index, targetPaneId);
      const id = layout.focusedPaneId;
      const sid = focusedSessionOf(layout);
      void tick().then(() => {
        if (sid !== null) pool.focusTerminal(sid);
        else paneRootEl(id)?.focus();
      });
    },
    navigateFileHistory(paneId, delta) {
      // Dirty previews are pinned synchronously before any replacement, even
      // when the reactive pin effect has not flushed yet.
      layout = navigateFileHistory(pinPaths(layout, get(dirtyFiles)), paneId, delta);
      const id = layout.focusedPaneId;
      void tick().then(() => paneRootEl(id)?.focus());
    },
    quickOpen(paneId) {
      layout = focusPane(layout, paneId);
      openQuickOpen();
    },
    focusPane(paneId) {
      layout = focusPane(layout, paneId);
    },
    activateTab(paneId, index) {
      layout = activateTab(layout, paneId, index);
      const sid = focusedSessionOf(layout);
      if (sid !== null && !paneTabHasKeyboardFocus()) pool.focusTerminal(sid);
    },
    closeTab(paneId, index) {
      // Detaches the view only — the session stays alive in the rail. A file
      // with unsaved edits asks first.
      const tab = findPane(layout.root, paneId)?.tabs[index];
      if (tab !== undefined) closeTabs([tab]);
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
    dragFileEntry: onTreeEntryDown,
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
    openFileFrom(paneId, path, newSplit, pinned = false) {
      openFileFromPane(paneId, path, newSplit, pinned);
    },
    openPathFrom(paneId, path, kind, newSplit) {
      openPath(path, kind, { fromPane: paneId, split: newSplit });
    },
    openChangesFrom(paneId, sessionId, newSplit) {
      openChangesFromPane(paneId, sessionId, newSplit);
    },
    openSubagentFrom(paneId, sessionId, agentId, title, newSplit) {
      openSubagentFromPane(paneId, sessionId, agentId, title, newSplit);
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
    retargetBrowser(id, host, port, path, found) {
      layout = setBrowserTarget(layout, id, host, port, path, found);
    },
    openDiffFrom(paneId, path, mode, newSplit, keep) {
      openDiffFromPane(paneId, path, mode, newSplit, keep === true);
    },
    openGitFrom(paneId, tab, newSplit) {
      openGitFromPane(paneId, tab, newSplit);
    },
    beginGitDrag(e, tab, onClick) {
      beginDrag(e, tab, onClick);
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
    detachTabToWindow(paneId, index) {
      xw.detachTabToWindow(paneId, index);
    },
    moveTargets() {
      return xw.moveTargets();
    },
    refreshMoveTargets() {
      xw.refreshMoveTargets();
    },
    moveTabToWindow(paneId, index, winId) {
      xw.moveTabToWindow(paneId, index, winId);
    },
  };

  /**
   * The open question of a forced view switch (a mid-task agent), answered
   * by the ConfirmDialog below — `window.confirm` never shows in the native
   * app. One question at a time: a newer ask answers the older one "no", so
   * its switch settles and releases `switchingViews`.
   */
  let switchAsk = $state.raw<{
    sessionId: string;
    target: "chat" | "term";
    answer(go: boolean): void;
  } | null>(null);

  // A question that no longer applies closes itself: the agent is gone, or
  // another window already switched it. (Answering clears `switchAsk`, so
  // this settles on the re-run.)
  $effect(() => {
    const ask = switchAsk;
    if (ask === null) return;
    const s = sessionsById.get(ask.sessionId);
    if (s === undefined || !s.alive || s.ui === ask.target) ask.answer(false);
  });

  function askForcedSwitch(sessionId: string, target: "chat" | "term"): Promise<boolean> {
    switchAsk?.answer(false);
    return new Promise((resolve) => {
      switchAsk = {
        sessionId,
        target,
        answer(go) {
          switchAsk = null;
          resolve(go);
        },
      };
    });
  }

  /**
   * The chat⇄terminal toggle: the daemon stops the current process and
   * resumes the same conversation in the other mode; the session row's `ui`
   * flips on the events bus and every pane follows. A mid-task agent 409s
   * (busy) until the user confirms the interruption.
   *
   * Guarded against double-fire: the toggle button and its ⌘-chord both call
   * here, so a switch already in flight for this id is ignored, and the button
   * reads as pending via the `switchingViews` store meanwhile. The server's own
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
        if (!(await askForcedSwitch(sessionId, target))) return;
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

  /** A tab's display label (drags, the detached-window title). */
  function tabLabel(tab: Tab): string {
    return tab.surface === "terminal"
      ? sessionLabel(displayNames, sessionsById, tab.sessionId)
      : tab.surface === "file"
        ? (fileTitles.get(tab.path) ?? basename(tab.path))
        : tab.surface === "finder"
          ? (basename(tab.path) || "Finder")
          : tab.surface === "diff"
            ? `${basename(tab.path)} (diff)`
            : tab.surface === "gitx"
              ? (tab.title ??
                (tab.view === "commit" ? "Commit" : tab.view === "branch" ? "Changes on this branch" : "History"))
            : tab.surface === "git"
              ? "Source Control"
              : tab.surface === "changes"
                ? "Changes"
                : tab.surface === "subagent"
                  ? tab.title
                : tab.surface === "dashboard"
                  ? "Dashboard"
                  : tab.surface === "timeline"
                    ? "Timeline"
                    : tab.surface === "knowledge"
                      ? "Knowledge"
                      : tab.surface === "plugins"
                        ? "Extensions"
                        : tab.surface === "plugin"
                          ? pluginViewTitle($workspacePlugins?.plugins, tab.plugin, tab.view)
                          : tab.surface === "browser"
                          ? (tab.host || "Browser")
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
    } catch (err) {
      showFlash(`Could not link terminal: ${err instanceof Error ? err.message : "request failed"}`);
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
    // The context bridge's typing path: pooled socket when warm, one-shot
    // socket otherwise — the reference lands even in a cold agent pane.
    typeIntoSession(agentId, `${termReference(terminalId)} `);
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
    typeIntoSession(agentId, `${termReference(terminalId)} `);
    pool.focusTerminal(agentId);
  }

  /** Link mutations + reveal for the pane top bars (chips and the menu). */
  const linkCtrl: LinkCtrl = {
    reveal(sessionId, besidePaneId) {
      // The Mastermind lives in its panel, never in a pane tab (a browser
      // pane it opened names it as the opener).
      const s = sessionsById.get(sessionId);
      if (s !== undefined && isMastermind(s)) {
        setMastermindPanelOpen(true);
        return;
      }
      revealSession(sessionId, besidePaneId);
      pool.focusTerminal(sessionId);
    },
    link(terminalId, agentId) {
      void doLink(terminalId, agentId);
    },
    unlink(terminalId) {
      links = links.filter((l) => l.terminal_id !== terminalId);
      deleteLink(terminalId).catch(async (err) => {
        showFlash(`Could not unlink terminal: ${err instanceof Error ? err.message : "request failed"}`);
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
    beginDrag(e, tab, onEntryClick, false, { path, kind });
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
</script>

{#if $agentSetup}
  {#key $agentSetup}
    <AgentSetupDialog request={$agentSetup} onclose={() => agentSetup.set(null)} onlaunch={launcher.pick} />
  {/key}
{/if}

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
    <!-- Home stays mounted under its Settings page (parked: hidden + inert)
         so the page is a sheet over Home, not a replacement. Both surfaces
         sit in a boundary, like every pane view: a render error in one
         settings panel must not freeze the whole Home window. -->
    {#if homeLoad !== null}
      {#await homeLoad}
        {#if !homeSettingsOpen}<SlowLoad>Loading Home…</SlowLoad>{/if}
      {:then HomeScreen}
        <div class="home-host" class:parked={homeSettingsOpen} inert={homeSettingsOpen}>
          <svelte:boundary onerror={(e) => console.error("home failed", e)}>
            <HomeScreen
              {workspaces}
              sessions={sessions.filter((s) => !isMastermind(s))}
              hostLabel={getHostLabel()}
              health={conn.health}
              daemonReachable={eventsUp || conn.healthUp}
              onOpen={activateWorkspace}
              onRemove={removeWorkspace}
              onStop={stopWorkspace}
              onOpenFolder={openPicker}
              onSettings={openSettingsSurface}
            />
            {#snippet failed(_error, reset)}
              <div class="home-settings-shell">
                <HomeNavigation active="workspaces" onHome={reset} onSettings={openSettingsSurface} />
                <div class="home-settings-content home-surface-failed">
                  <p role="alert">Home hit an error and stopped.</p>
                  <button onclick={reset}>Try again</button>
                  <button onclick={openPicker}>Open a folder</button>
                </div>
              </div>
            {/snippet}
          </svelte:boundary>
        </div>
      {:catch error}
        {#if !homeSettingsOpen}
          <div class="home-settings-shell">
            <HomeNavigation active="workspaces"
              onHome={() => (homeLoad = retryPaneView("home", error))} onSettings={openSettingsSurface} />
            <div class="home-settings-content home-surface-failed">
              <p role="alert">Couldn't open Home.</p>
              <button onclick={() => (homeLoad = retryPaneView("home", error))}>Retry</button>
              <button onclick={openPicker}>Open a folder</button>
            </div>
          </div>
        {/if}
      {/await}
    {/if}
    {#if homeSettingsOpen}
      <div class="home-settings-shell">
        <HomeNavigation active="settings"
          onHome={() => (homeSettingsOpen = false)} onSettings={openSettingsSurface} />
      <div class="home-settings-surface">
        <nav class="home-surface-nav" aria-label="Home navigation">
          <button class="home-settings-back" onclick={() => (homeSettingsOpen = false)}>
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true"><path d="m9.5 4-4 4 4 4" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" /></svg>
            Home
          </button>
          <span class="home-surface-divider" aria-hidden="true">/</span>
          <span aria-current="page">Settings</span>
          <kbd class="home-back-hint">esc</kbd>
        </nav>
        <div class="home-settings-content">
          {#await homeSettingsLoad}
            <SlowLoad>Loading settings…</SlowLoad>
          {:then SettingsView}
            {#if SettingsView}
              <svelte:boundary onerror={(e) => console.error("settings failed", e)}>
                <SettingsView />
                {#snippet failed(_error, reset)}
                  <div class="home-surface-failed">
                    <p role="alert">Settings hit an error and stopped.</p>
                    <button onclick={reset}>Try again</button>
                  </div>
                {/snippet}
              </svelte:boundary>
            {/if}
          {:catch error}
            <div class="home-surface-failed">
              <p role="alert">Couldn't open settings.</p>
              <!-- The same asset recovery as a pane's retry: a failed CSS
                   preload is memoized by Vite, so a plain re-import would
                   fail the same way every time. -->
              <button onclick={() => (homeSettingsLoad = retryPaneView("settings", error))}>Retry</button>
            </div>
          {/await}
        </div>
      </div>
      </div>
    {/if}
  {:else}
  <div class="body" bind:clientWidth={bodyWidth}>
    <aside
      class="rail"
      class:collapsed={layout.focusMode}
      class:resizing={railDividerActive}
      style:width={layout.focusMode ? undefined : `${railWidth}px`}
      style:--rail-w={layout.focusMode ? undefined : `${railWidth}px`}
      bind:this={railEl}
    >
      <div class="workspace">
        <button
          class="ws-btn"
          class:placeholder={workspace === null && activeWsId === null}
          title={clusterWs !== null ? `${workspace?.root ?? ""} — every workspace on ${hostAlias}` : workspace?.root}
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
          <span
            class="needs"
            title="{needsYou} waiting for your approval — a permission or a question"
          >
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

      <nav class="sessions sidebar-scrollbar">
        {#snippet sessionRow(s: Session)}
          {#if confirmKillId === s.id}
            <div
              class="row confirm"
              class:agent-row={s.kind === "agent"}
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
              class:agent-row={s.kind === "agent"}
              class:active={s.id === focusedSessionId}
              class:unread={railRename.target !== s.id && isUnread(s.id)}
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
                if (railRename.target === s.id) return; // the input owns keys
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  openSess(s.id);
                } else if (e.key === "F2") {
                  e.preventDefault();
                  startRename(s);
                }
              }}
              oncontextmenu={(e) => {
                const wt = s.kind === "agent" ? (s.managed_worktree ?? null) : null;
                contextMenu.openAt(e, [
                  { label: "Rename…", onSelect: () => startRename(s) },
                  ...(wt !== null
                    ? [{ label: "Open worktree in new window", onSelect: () => void keptWorktree.openWindow(wt) }]
                    : []),
                ]);
              }}
            >
              <!-- Session-type glyph carrying the state color — the same
                   mark as the pane tab (surface parity, rail included). It
                   breathes while alive (pulse), and muted while background
                   work runs past the turn: the rail's activity cue, since the
                   row shows only the glyph, no separate dot. -->
              <SessionGlyph
                kind={s.kind}
                agentKind={s.agent_kind}
                state={glyphState(s)}
                size={11}
                title={glyphTitle(s)}
                pulse
                backgrounded={backgrounded(s)}
              />
              {#if railRename.target === s.id}
                <!-- svelte-ignore a11y_autofocus -->
                <input
                  class="rename-input"
                  type="text"
                  autofocus
                  bind:value={railRename.draft}
                  onkeydown={(e) => railRename.keydown(e)}
                  onblur={() => railRename.blur()}
                />
              {:else}
                <!-- svelte-ignore a11y_no_static_element_interactions --
                     dblclick is the mouse path; keyboard rename lives on
                     the row itself (F2), which carries the button role. -->
                <span
                  class="labels"
                  title={[displayNames.get(s.id) ?? displayName(s), s.status_detail || s.title, "Double-click to rename (F2)"].filter(Boolean).join("\n")}
                  ondblclick={() => startRename(s)}
                >
                  <span class="name">
                    {displayNames.get(s.id) ?? displayName(s)}
                    {#if s.kind === "agent" && s.remote_control_url}
                      <!-- Remote Control is on: this session is reachable from
                           the Claude app / claude.ai/code. Quiet accent pill;
                           the chat header owns the controls. -->
                      <span class="remote-badge" title="Remote Control on — pick this session up in the Claude app or at claude.ai/code">remote</span>
                    {/if}
                  </span>
                  {#if s.kind === "agent" && s.git && s.git.worktree !== s.git.repo}
                    <span class="wt-mark"><BranchChip git={s.git} /></span>
                  {/if}
                  {#if scheduledWake(s) !== null}
                    {@const wake = scheduledWake(s)}
                    <!-- Between loop ticks: it will run again on its own. -->
                    <WakeRing at={wake?.at_ms ?? null} from={wake?.from_ms ?? null} label={wake?.label ?? ""} />
                  {/if}
                </span>
              {/if}
              {#if railRename.target !== s.id && isUnread(s.id)}
                <!-- Unread: the scannable half of the cue (the bold name is
                     the readable half). Yields to the close button on hover. -->
                <span class="unread-dot" aria-hidden="true"></span>
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

        {#if $knowledgeProviderActive}
          <!-- Knowledge earns a rail row only while a structured provider
               (mycelium) is active here — with nothing recorded there is
               nothing to open, and the row would be chrome. -->
          <button
            class="row dash-row"
            class:dash-active={knowledgeOpen}
            title="knowledge — what your agents have recorded in this project"
            onclick={openKnowledgeSurface}
          >
            <svg class="dash-glyph" viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
              <path
                d="M2.5 3.2c1.8-.7 3.6-.6 5.5.5v9c-1.9-1.1-3.7-1.2-5.5-.5zM13.5 3.2c-1.8-.7-3.6-.6-5.5.5v9c1.9-1.1 3.7-1.2 5.5-.5z"
                fill="none"
                stroke="currentColor"
                stroke-width="1.3"
                stroke-linejoin="round"
              />
            </svg>
            <span class="dash-label">knowledge</span>
          </button>
        {/if}

        <!-- Extensions (Plugins · Skills): always a row — it is where a
             plugin gets installed in the first place. -->
        <button
          class="row dash-row"
          class:dash-active={pluginsOpen}
          title="extensions — install plugins, and see every skill your agents can use"
          onclick={openPluginsSurface}
        >
          <!-- The Extensions tab's own glyph (plugins/glyph.ts): one glyph per surface. -->
          <ExtensionsGlyph class="dash-glyph" />
          <span class="dash-label">extensions</span>
        </button>

        <!-- Terminals first (there are few), agents below (there are many);
             the focus-mode strip follows the same order. -->
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
        <!-- One split button: the main surface starts the saved default
             agent instantly (the shortcut's target too); its CHEVRON — hover
             ~150ms or a click — opens the launcher menu: the installed
             agents up front, the rest folded behind one row, and the way
             into Settings → Agents. ⌘/ctrl-click starts in the terminal.
             A missing default says what its click will set up. -->
        <div class="new-split" role="group" aria-label="new agent" bind:this={launcher.splitEl}>
          {#if launcher.railStart}
            {@const q = launcher.railStart}
              <button
                class="row new quick-start"
                class:want-setup={q.action !== "start"}
                data-agent={q.id}
                title={q.action === "check"
                  ? `couldn't check ${q.name}: ${q.info?.unchecked ?? "your login shell didn't answer."} Click to check again.`
                  : q.action === "install"
                  ? `install ${q.name}`
                  : q.action === "setup-chat"
                    ? `set up chat for ${q.name}`
                    : q.action === "setup"
                      ? `set up ${q.name} in Settings → Agents`
                      : `start ${q.name}${newInWorktree ? " in a new worktree" : ""} (${keyHint("newAgent")}) · ${isMac ? "⌘" : "ctrl"}-click for the terminal`}
                onclick={(e) => launcher.quickStart(q, e.metaKey || e.ctrlKey)}
              >
                <span class="new-plus" aria-hidden="true">+</span>
                <span class="new-name">new agent</span>
                <span class="new-default">{q.name}</span>
                {#if newInWorktree && q.action === "start"}
                  <svg class="new-wt" viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
                    <path
                      d="M5 3v7.5M5 12.5v.5M11 3v3a2.5 2.5 0 0 1-2.5 2.5H5"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.4"
                      stroke-linecap="round"
                    />
                    <circle cx="5" cy="12.6" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4" />
                    <circle cx="5" cy="2.4" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4" />
                    <circle cx="11" cy="2.4" r="1.6" fill="none" stroke="currentColor" stroke-width="1.4" />
                  </svg>
                {/if}
                {#if q.action !== "start"}
                  <span class="new-need"
                    >{q.action === "check"
                      ? $agentRechecks.has(q.id)
                        ? "checking…"
                        : "retry"
                      : q.action === "install"
                        ? "install"
                        : q.action === "setup-chat"
                          ? "set up chat"
                          : "set up"}</span
                  >
                {/if}
              </button>
                <button
                  class="new-chev"
                  aria-haspopup="menu"
                  aria-expanded={launcher.open}
                  aria-label="more agents"
                  title="more agents"
                  onpointerenter={launcher.armHover}
                  onpointerleave={launcher.disarmHover}
                  onkeydown={(e) => {
                    if (e.key === "ArrowDown" && !launcher.open) {
                      e.preventDefault();
                      launcher.openMenu();
                    }
                  }}
                  onclick={() => {
                    if (launcher.open && Date.now() - launcher.openedAt > 400) launcher.closeMenu();
                    else launcher.openMenu();
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
          {/if}
        </div>
        {#if createError}
          <div class="create-error">{createError}</div>
        {/if}
        {#if keptWorktree.notice !== null && keptWorktree.notice.ws === activeWsId}
          {@const wtNotice = keptWorktree.notice}
          <div class="wt-kept" role="status" title={wtNotice.path}>
            {#if wtNotice.error !== null}
              <span class="wt-kept-msg err">Kept: {wtNotice.error}</span>
            {:else}
              <span class="wt-kept-msg"><span class="wt-kept-name">{wtNotice.label}</span> kept</span>
              <button
                class="wt-kept-btn strong"
                title="delete this worktree's folder — the branch stays"
                disabled={wtNotice.busy}
                onclick={() => void keptWorktree.remove()}>Remove</button
              >
              <button class="wt-kept-btn" disabled={wtNotice.busy} onclick={keptWorktree.dismiss}>Keep</button>
            {/if}
          </div>
        {/if}

        <!-- Recents: ended agent conversations, any agent type, newest
             first — the daemon remembers them across restarts. Click resumes
             when a native handle exists, otherwise starts fresh honestly. -->
        {#if workspace !== null}
          <!-- The section fills whatever the sessions column has left and
               shows as many rows as FIT that space (measured, see
               recentsRail.fit) — never a fixed three. -->
          <div class="recents" class:expanded={recentsRail.expanded}>
            <!-- svelte-ignore a11y_no_static_element_interactions -- the
                 context menu is a shortcut; nothing here needs a keyboard
                 path beyond the rows' own. -->
            <div
              class="recents-head"
              oncontextmenu={(e) => {
                if (recentsRail.visible.length > 0) {
                  contextMenu.openAt(e, [{ label: "Archive all", onSelect: () => void recentsRail.archiveAll() }]);
                }
              }}
            >
              <span>recent</span>
              <span class="recents-actions">
              {#if recentsRail.expanded || recentsRail.visible.length > recentsRail.fit}
                <!-- In the header, not below the rows: on a short column the
                     rows may sit under the fold, the header never does. -->
                <button
                  class="recents-more"
                  aria-expanded={recentsRail.expanded}
                  title={recentsRail.expanded
                    ? "show only what fits"
                    : `show all ${recentsRail.visible.length} recent conversations`}
                  onclick={recentsRail.toggle}
                >
                  {recentsRail.expanded ? "less" : "more"}
                </button>
              {/if}
              <!-- Every past session, beyond these: in ⌘P too. Both header
                   actions reveal while the pointer is over Recents (always on
                   touch). -->
              <button
                class="recents-history"
                title="All sessions — every past session in this workspace"
                aria-label="All sessions"
                onclick={openSessionsSurface}
              >
                <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
                  <path
                    d="M2.6 8a5.4 5.4 0 1 0 1.6-3.85"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.4"
                    stroke-linecap="round"
                  />
                  <path
                    d="M2.2 2.6v2.6h2.6"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.4"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  />
                  <path
                    d="M8 5.2v3l2 1.3"
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
            <div class="recents-list sidebar-scrollbar" class:expanded={recentsRail.expanded} bind:this={recentsRail.listEl}>
              {#each recentsRail.expanded ? recentsRail.visible : recentsRail.visible.slice(0, recentsRail.fit) as r (r.key)}
                <button
                  class="recent-row"
                  title={recentsRail.tooltip(r)}
                  onclick={() => recentsRail.open(r)}
                  oncontextmenu={(e) =>
                    contextMenu.openAt(e, [{ label: "Archive", onSelect: () => void recentsRail.archive(r) }])}
                  animate:flip={{ duration: recentsRail.flipMs }}
                  out:fade={{ duration: recentsRail.flipMs === 0 ? 0 : 120 }}
                >
                  <SessionGlyph kind="agent" agentKind={r.kind} size={11} />
                  <span class="recent-title">{r.title}</span>
                  <span class="recent-age">{relativeAge(r.lastActive)}</span>
                </button>
              {/each}
              {#if recentsRail.archivedUndo !== null && recentsRail.archivedUndo.ws === activeWsId}
                <div class="recents-note">
                  Archived {recentsRail.archivedUndo.keys.length} ·
                  <button class="recents-undo" onclick={() => void recentsRail.undoArchiveAll()}>Undo</button>
                </div>
              {:else if recentsRail.visible.length === 0}
                <div class="recents-note">No recent conversations</div>
              {/if}
            </div>
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
                dropAction={dropSpot?.kind === "fileOp" ? dropSpot.operation : "upload"}
                activePath={focusedFilePath}
                reveal={treeReveal}
                createRequest={treeCreate}
                dropDir={(dropSpot?.kind === "uploadDir" || (dropSpot?.kind === "fileOp" && dropSpot.blocked === null)) && dropSpot.paneId === null ? dropSpot.dir : null}
                dropKey={(dropSpot?.kind === "uploadDir" || (dropSpot?.kind === "fileOp" && dropSpot.blocked === null)) && dropSpot.paneId === null
                  ? (dropSpot.rowKey ?? null)
                  : null}
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
        <!-- Two groups so the bar can WRAP: identity (dot, host, link RTT)
             and tools (git, slurm, settings). When both don't
             fit one row the tools drop to a second, instead of the host name
             being squeezed to nothing behind a branch and a queue count. -->
        <span class="daemon-id">
        <span
          class="daemon-dot"
          class:ok={eventsUp}
          class:pulse={$reconnectingSockets > 0}
          role="status"
          title={eventsUp ? "connected" : "disconnected"}
          aria-label={eventsUp ? "connected" : "disconnected"}
        ></span>
        <!-- Inside an allocation the label carries the node ("cluster ›
             n042") so a compute-node window never poses as its login
             node — derived from the daemon's self block, hash-independent. -->
        <span class="daemon-host" class:remote={isRemoteWindow} title={conn.health?.hostname}
          >{$computeStatus?.self
            ? `${getHostLabel()} › ${$computeStatus.self.node}`
            : getHostLabel()}</span
        >
        {#if isRemoteWindow && $linkRtt !== null && $linkRtt >= LINK_RTT_BADGE_MS}
          <!-- Honest latency signal: on a distant host every keystroke echo
               and UI fetch pays at least this — better named than mysterious
               (the remote perf audit's R4). -->
          <span
            class="daemon-rtt"
            title={`link round trip ~${$linkRtt} ms — every keystroke echo and fetch pays at least this`}
            >{$linkRtt}ms</span
          >
        {/if}
        </span>
        <span class="daemon-tools">
        {#if stripGit !== null}
          {@const gs = stripGit.status}
          <!-- Always-on orientation: what branch you're on and how dirty the
               tree is; one click opens the source-control panel. With several
               repositories it names the one holding what's focused. -->
          <button
            class="daemon-git"
            onclick={() => {
              openGitPanel();
              if (stripGit.path !== null) revealRepo(stripGit.path);
            }}
            title={`${stripGit.label ? `${stripGit.label}: ` : ""}${gs.detached ? `detached at ${gs.head ?? "?"}` : (gs.branch ?? "unborn branch")}${gs.upstream ? ` · ${gs.upstream}` : ""} — open source control`}
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
            {#if stripGit.label}<span class="dg-repo">{stripGit.label}</span>{/if}
            <span class="dg-branch"
              >{branchLabel(gs)}</span
            >
            {#if gs.ahead > 0}<span class="dg-ab" title={aheadBehindWords(gs.ahead, 0, gs.upstream)}>↑{gs.ahead}</span>{/if}
            {#if gs.behind > 0}<span class="dg-ab" title={aheadBehindWords(0, gs.behind, gs.upstream)}>↓{gs.behind}</span>{/if}
            {#if gs.counts.total > 0}
              <span class="dg-dirty" title="{gs.counts.total} changed">
                ●{gs.counts.total}
              </span>
            {/if}
          </button>
        {:else if stripRepoCount > 1}
          <!-- A folder of repositories with nothing focused inside one. -->
          <button class="daemon-git" onclick={openGitPanel} title="{stripRepoCount} repositories — open source control">
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
            <span class="dg-branch">{stripRepoCount} repos</span>
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
        {#if activeWsId !== null}
          <button
            class="daemon-settings"
            title="settings ({keyHint("settings")})"
            aria-label="settings"
            onclick={openSettingsSurface}
          >
            <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
              <!-- A cog: settings. -->
              <path
                d="M6.77 3.05L6.98 1.18A6.9 6.9 0 0 1 9.02 1.18L9.23 3.05A5.1 5.1 0 0 1 10.63 3.63L12.10 2.45A6.9 6.9 0 0 1 13.55 3.90L12.37 5.37A5.1 5.1 0 0 1 12.95 6.77L14.82 6.98A6.9 6.9 0 0 1 14.82 9.02L12.95 9.23A5.1 5.1 0 0 1 12.37 10.63L13.55 12.10A6.9 6.9 0 0 1 12.10 13.55L10.63 12.37A5.1 5.1 0 0 1 9.23 12.95L9.02 14.82A6.9 6.9 0 0 1 6.98 14.82L6.77 12.95A5.1 5.1 0 0 1 5.37 12.37L3.90 13.55A6.9 6.9 0 0 1 2.45 12.10L3.63 10.63A5.1 5.1 0 0 1 3.05 9.23L1.18 9.02A6.9 6.9 0 0 1 1.18 6.98L3.05 6.77A5.1 5.1 0 0 1 3.63 5.37L2.45 3.90A6.9 6.9 0 0 1 3.90 2.45L5.37 3.63A5.1 5.1 0 0 1 6.77 3.05Z"
                fill="none"
                stroke="currentColor"
                stroke-width="1.3"
                stroke-linejoin="round"
              />
              <circle cx="8" cy="8" r="2.2" fill="none" stroke="currentColor" stroke-width="1.3" />
            </svg>
          </button>
        {/if}
        </span>
      </div>
    </aside>

    <!-- Sidebar width handle: a quiet vertical splitter on the rail's edge.
         Hidden, not unmounted, in focus mode: inserting a sibling ahead of
         the stage makes WebKit re-resolve every parked document in it. -->
    <div
      class="rail-resize"
      class:active={railDividerActive}
      hidden={layout.focusMode}
      role="separator"
      aria-orientation="vertical"
      aria-label="resize sidebar"
      title="drag to resize · double-click to reset"
      onpointerdown={onRailResizeDown}
      ondblclick={resetRailWidth}
    ></div>

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
            dash={dashCtx}
            {ctrl}
          />
        {/if}
        {#if dropSpot?.kind === "edge"}
          <!-- Window-edge preview: the root split's new pane, full height/width,
               named like the pane zones so it never reads as a bare rectangle. -->
          <div class="edge-drop {dropSpot.edge}">
            <span class="drop-chip">split window {sideWord(dropSpot.edge)}</span>
          </div>
        {/if}
        {#if detachedEmpty && tabCount(layout) === 0}
          <!-- A detached window that emptied but the browser refused
               window.close (a tab we didn't script-open): say so instead of
               leaving a mute blank stage. -->
          <div class="detached-empty">
            <p>This window has nothing left to show.</p>
            <button onclick={() => window.close()}>close window</button>
          </div>
        {/if}
      {/if}
    </main>
    {#if mastermindPanel.open && MastermindPanelView !== null && layoutReady && activeWsId !== null}
      <MastermindPanelView
        cfg={mmCfg}
        session={mmSession}
        wsId={activeWsId}
        paneId={layout.focusedPaneId}
        {ctrl}
        refresh={refreshWorkspaces}
        visible
        context={mmContext}
        hostWidth={bodyWidth}
        onOpenSettings={openSettingsAt}
      />
    {/if}
  </div>

  {#if layout.focusMode}
    <!-- Focus-mode session strip: the rail is gone, but the window always
         says where you are. Hidden whenever the rail is visible. -->
    <!-- Deep native drag regions include the strip's padding and flex gaps;
         Tauri excludes interactive descendants, so its buttons still work. -->
    <footer class="strip" data-tauri-drag-region={nativeTitlebarOverlay ? "deep" : undefined}>
      {#if !detachedWindow}
        <!-- Sidebar reveal is the MAIN window's affordance. A detached solo
             window offers no rail at all — it is just its pane; the way back
             to the full workbench is re-attach (maintainer call). -->
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
      {:else}
        <!-- Same place, same look — but a plain label: nothing in a solo
             window opens the rail. The spacer replaces the hidden chips row
             as the strip's flex grower — without it every non-macOS-native
             window (no .strip-drag) bunches the controls at the left edge.
             Under the macOS titlebar overlay the spacer grows ALONGSIDE
             .strip-drag, so it (and the inert label) must be drag regions
             too or half the titlebar goes dead for moving the window. -->
        <span class="strip-ws inert" data-tauri-drag-region>{workspace?.name ?? "chimaera"}</span>
        <div class="strip-spacer" data-tauri-drag-region aria-hidden="true"></div>
      {/if}
      {#if !detachedWindow}
        <!-- The workspace's session chips are the MAIN window's wayfinding; a
             detached solo window is just its own tabs — the roster following
             it in reads as the whole workbench tagging along (found live). -->
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
                state={glyphState(s)}
                size={10}
                title={glyphTitle(s)}
                pulse
                backgrounded={backgrounded(s)}
              />
              <span class="chip-name">{displayNames.get(s.id) ?? displayName(s)}</span>
              {#if isUnread(s.id)}
                <span class="chip-unread" aria-hidden="true"></span>
              {/if}
            </button>
          {/each}
        </div>
      {/if}
      {#if nativeTitlebarOverlay}
        <!-- Native focus mode promotes this strip to the titlebar. Keep a
             generous, control-height empty target so dragging never competes
             with the workspace/session buttons. -->
        <div class="strip-drag" data-tauri-drag-region aria-hidden="true"></div>
      {/if}
      {#if needsYou > 0}
        <!-- Scoped upstream: in a detached window this counts only the
             sessions THIS window shows, so it never false-alarms for the
             rest of the workspace. -->
        <span class="strip-needs" title="a permission or a question is waiting on you"
          >{needsYou} awaiting approval</span
        >
      {/if}
      {#if detachedWindow && detachOrigin !== null}
        <!-- Send everything in this solo window back where it came from; the
             ok-ack empties the layout and the window closes itself. -->
        <button class="strip-reattach" title="move this window's tabs back to their original window" onclick={() => void xw.reattachToOrigin()}>
          re-attach
        </button>
      {/if}
      <span class="strip-host" class:remote={isRemoteWindow} title={conn.health?.hostname}
        >{getHostLabel()}</span
      >
      {#if isRemoteWindow && $linkRtt !== null && $linkRtt >= LINK_RTT_BADGE_MS}
        <!-- Detached windows ghost keystrokes like any remote window — they
             get the same honest latency signal. -->
        <span
          class="daemon-rtt"
          title={`link round trip ~${$linkRtt} ms — every keystroke echo and fetch pays at least this`}
          >{$linkRtt}ms</span
        >
      {/if}
    </footer>
  {/if}
  {/if}
</div>

{#if launcher.open && activeWsId !== null && launcher.anchor !== null}
  <Launcher
    anchor={launcher.anchor}
    initial={launcher.agents}
    onPick={launcher.pick}
    onInstall={launcher.install}
    onUpdate={launcher.update}
    onClose={launcher.closeMenu}
    onSettings={launcher.openAgentSettings}
    onAgents={launcher.setAgents}
    inWorktree={workspaceIsRepo ? (workspace?.new_in_worktree === true) : null}
    onToggleWorktree={(on) => void toggleNewInWorktree(on)}
  />
{/if}


{#if pickerOpen}
  <!-- Home row: raises (or opens) this host's Home launcher without leaving
       this window's workspace — a promoted window never turns back into Home. -->
  <FolderPicker
    recents={workspaces}
    onOpened={activateWorkspace}
    onClose={closePicker}
    onHome={activeWsId !== null
      ? () => {
          closePicker();
          void openWindow(scopeAlias, null);
        }
      : undefined}
  />
{/if}

{#if quickOpenOpen && activeWsId !== null}
  <QuickOpen
    workspaceId={activeWsId}
    sessions={wsSessions}
    sessionNames={displayNames}
    commands={quickOpenCommands}
    onOpenFile={quickOpenFile}
    onOpenSession={quickOpenSession}
    onClose={closeQuickOpen}
  />
{/if}

{#if $attachRequest !== null && activeWsId !== null}
  <!-- The "Use mycelium for Knowledge" sheet: one modal instance for every
       surface that opens it (Knowledge's empty card, the dashboard line,
       the plugin card, the Mastermind dock). Lazy — it rides the plugins
       chunk, not the always-loaded shell. -->
  {#await import("./lib/plugins/AttachSheet.svelte") then { default: AttachSheet }}
    <!-- Keyed: the sheet checks its workspace once when it opens, so a
         workspace switch underneath must start it over, never post one
         workspace's hook hashes to another. Bounded like the pane views: a
         throw here closes into a line, not a frozen window. -->
    {#key `${activeWsId}:${$attachRequest.pluginId}`}
      <svelte:boundary onerror={(e) => console.error("attach sheet failed", e)}>
        <AttachSheet
          wsId={activeWsId}
          pluginId={$attachRequest.pluginId}
          onOpenSession={(id) => {
            closeAttachSheet();
            openSess(id);
          }}
          onClose={closeAttachSheet}
        />
        {#snippet failed()}
          <div class="sheet-crash" role="alert">
            <span>The setup sheet hit an error.</span>
            <button type="button" onclick={closeAttachSheet}>Close</button>
          </div>
        {/snippet}
      </svelte:boundary>
    {/key}
  {/await}
{/if}

<!-- Blocking re-auth overlay: the daemon rejected this window's token
     (restart or expiry). Self-gating on the `unauthorized` store. -->
<ReauthOverlay enabled={!conn.canReconnect} />

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
    }? This cannot be undone.${unsavedDeleteNote($pendingDelete.path, $dirtyFiles)}`}
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

<!-- A mid-task agent's view switch interrupts its turn: asked, not assumed. -->
{#if switchAsk !== null}
  <ConfirmDialog
    title={switchAsk.target === "chat" ? "Open as chat?" : "Open as terminal?"}
    body={`${sessionLabel(displayNames, sessionsById, switchAsk.sessionId)} is mid-task. Switching restarts it via resume, which interrupts the current turn.`}
    confirmLabel="switch"
    enterConfirms
    onConfirm={() => switchAsk?.answer(true)}
    onCancel={() => switchAsk?.answer(false)}
  />
{/if}

<!-- The native window close / app quit over every unsaved file here; it takes
     precedence over a tab close already asking (whose files it includes). -->
{#if windowClose.prompt !== null}
  <CloseDirtyDialog
    paths={windowClose.prompt.paths}
    saving={windowClose.saving}
    error={windowClose.error}
    action={windowClose.prompt.reason}
    onSave={() => void windowClose.save()}
    onDiscard={() => windowClose.discard()}
    onCancel={() => windowClose.cancel()}
  />
<!-- Closing tabs whose files hold unsaved edits: save / don't save / cancel. -->
{:else if pendingClosePaths.length > 0}
  <CloseDirtyDialog
    paths={pendingClosePaths}
    saving={closeSaving}
    error={closeError}
    onSave={() => void saveAndClose()}
    onDiscard={discardAndClose}
    onCancel={cancelClose}
  />
{/if}

<!-- Ambient update offer (small, snoozable): a newer release, or a daemon
     older than this app. One per window; dismissals are origin-wide. An
     explicit check (menu, version stamp) answers here too, even "up to date". -->
{#if $assetTransition !== null}
  <AssetTransitionNotice
    transition={$assetTransition}
    blockedFiles={$dirtyFiles.size}
    blockedDrafts={$volatileChatDrafts.size}
    onReload={requestAssetReload}
    onDismiss={clearChunkFailure}
    onCancel={cancelWindowReload}
  />
{/if}

{#if updateNotice !== null && $assetTransition === null}
  <UpdateToast notice={updateNotice} />
{/if}

<!-- Transient outcome chip: an action that would otherwise fail in silence
     (a refused detach, a blocked popup, a cross-window move that timed out). -->
{#if flash.text !== null}
  <div class="flash-chip" role="status" aria-live="polite">{flash.text}</div>
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

{#if clusterWs !== null && clusterHomeOpen && ClusterHomeView !== null}
  <ClusterHomeView
    alias={hostAlias}
    here={clusterWs}
    hereName={workspace?.name ?? "workspace"}
    onClose={() => (clusterHomeOpen = false)}
  />
{/if}

{#if jobCtx !== null && JobNoticesView !== null}
  <!-- A job window: the "continue on a new node" offer near the job's time
       limit, and the calm overlay once the shell says the job ended. -->
  <JobNoticesView
    alias={hostAlias}
    cws={jobCtx.cws}
    self={$computeStatus?.self ?? null}
    receivedAt={$computeStatus?.received_at_ms ?? 0}
    ended={conn.jobEnded}
    stacked={conn.reconnectSurface !== "hidden" && !$askpassActive}
  />
{/if}

{#if conn.reconnectSurface !== "hidden" && !$askpassActive}
  <!-- An automatic reconnect is status, not a blocking decision: keep the
       rendered workbench readable while the tunnel heals. Only a failed
       attempt becomes a modal with Retry. A scoped askpass prompt temporarily
       owns this space when this host actually needs authentication. Dismissing
       a failure leaves a compact Retry surface: a native 401 must never become
       an unrecoverable blank state. An asset-transition notice must not hide
       it either: a build/connection transition blocked on unsaved work has no
       dismiss and can sit there indefinitely, so the strip stacks below it. -->
  {#if conn.reconnectSurface === "status"}
    <div
      class="reconnect-status"
      class:stacked={$assetTransition !== null}
      role="status"
      aria-live="polite"
    >
      <span class="reconnect-spinner" class:spin={conn.reconnecting} aria-hidden="true"></span>
      <span class="reconnect-status-copy">
        <strong>{conn.reconnecting ? `reconnecting to ${hostAlias}…` : `waiting for ${hostAlias}…`}</strong>
        <span>{conn.reconnectReason ?? "the workbench will resume in place"}</span>
      </span>
      <button class="reconnect-status-dismiss" aria-label="dismiss reconnect status" onclick={() => conn.dismissReconnect()}>×</button>
    </div>
  {:else if conn.reconnectSurface === "failure"}
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
        <p class="reconnect-body">{conn.reconnectError}</p>
        <div class="reconnect-actions">
          <button class="reconnect-dismiss" onclick={() => conn.dismissReconnect()}>dismiss</button>
          <button
            class="reconnect-retry"
            use:focusOnMount
            disabled={conn.reconnecting}
            onclick={() => conn.retryReconnect()}
          >
            {conn.reconnecting ? "reconnecting…" : "retry"}
          </button>
        </div>
      </div>
    </div>
  {:else}
    <div
      class="reconnect-status"
      class:stacked={$assetTransition !== null}
      role="status"
      aria-live="polite"
    >
      <span class="reconnect-spinner" aria-hidden="true"></span>
      <span class="reconnect-status-copy">
        <strong>reconnect to {hostAlias}</strong>
        <span>
          {conn.reconnectError ?? "this window still needs fresh remote credentials"}
        </span>
      </span>
      <button
        class="reconnect-status-retry"
        disabled={conn.reconnecting}
        onclick={() => conn.retryReconnect()}
      >
        {conn.reconnecting ? "reconnecting…" : "retry"}
      </button>
    </div>
  {/if}
{/if}

<style>
  /* Parked Home under its Settings page: opacity + inert (never visibility,
     which inherits and makes WebKit re-resolve the subtree). The opacity
     stacking context also contains Home's fixed-position version stamp. */
  .home-host.parked {
    opacity: 0;
  }
  .home-settings-shell {
    position: absolute;
    inset: 0;
    display: flex;
    background: var(--bg);
  }
  .home-surface-failed {
    padding: 24px;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px;
  }
  .home-surface-failed p {
    margin: 0;
    flex-basis: 100%;
  }
  .home-settings-surface {
    flex: 1;
    min-width: 0;
    min-height: 0;
    padding: 44px 24px 24px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .home-surface-nav {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 34px;
    font-size: var(--text-sm);
    color: var(--fg);
  }
  .home-settings-back {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--muted);
    background: transparent;
    border: 0;
    border-radius: 6px;
    padding: 8px;
    font: inherit;
    cursor: pointer;
  }
  .home-settings-back:hover { color: var(--fg); background: var(--row-hover); }
  .home-settings-back:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  .home-surface-divider, .home-back-hint { color: var(--muted); }
  .home-back-hint { margin-left: auto; font-size: var(--text-xs); }
  .home-settings-content {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow: hidden;
    border: 1px solid var(--edge);
    border-radius: 8px;
  }
  /* On a phone the navigation bar above already says where you are: drop
     the breadcrumb and the card frame so the page gets the whole width. */
  @media (max-width: 700px) {
    .home-settings-shell { flex-direction: column; }
    .home-settings-surface { padding: 0; gap: 0; }
    .home-surface-nav { display: none; }
    .home-settings-content { border: 0; border-radius: 0; }
  }

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
    position: relative;
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

  /* Transient activity lives in the tooltip. Keep a compact, centered row
     through status changes, rename and kill confirmation. */
  .row.agent-row {
    box-sizing: border-box;
    flex-shrink: 0;
    height: calc(var(--text-sm) * 1.3 + 12px);
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
    align-items: center;
    gap: 6px;
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

  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--text-sm);
  }
  /* A branch shares the name's line without taking its full width. */
  .wt-mark {
    display: flex;
    min-width: 0;
    max-width: 45%;
    opacity: 0.85;
    font-size: var(--text-xs);
  }

  /* Remote Control badge on a rail row: an accent pill inline after the
     name, the same pill language as the chat header's chips. */
  .remote-badge {
    display: inline-block;
    vertical-align: 1px;
    margin-left: 5px;
    padding: 0 5px;
    font-family: var(--mono);
    font-size: 9px;
    line-height: 13px;
    letter-spacing: 0.02em;
    color: var(--accent);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--edge));
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent) 9%, transparent);
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

  /* Unread output: an agent finished (or handed back, waiting for input)
     and you haven't looked yet. Two halves, the unread-mail convention: a
     bolder full-ink name you READ, and a small accent dot you SCAN for down
     the rail. Still no bar or wash in the dense rail — those read as hover /
     active. The same pair marks pane tabs, strip chips, and dashboard cards.
     A focused row is never unread, so this never fights the active state.
     Counts are reserved for approvals (see needsApproval): unread never adds
     a number anywhere. */
  .row.unread .name,
  .chip.unread .chip-name {
    color: var(--fg);
    font-weight: 600;
  }

  /* Sits in the close button's slot (right edge, like the ⌘-digit badge),
     so marking never reflows the label; hover hands the slot back to ×. */
  .unread-dot {
    position: absolute;
    right: 11px;
    top: 50%;
    width: 6px;
    height: 6px;
    margin-top: -3px;
    border-radius: 50%;
    background: var(--accent);
    pointer-events: none;
    transition: opacity 0.12s ease;
    animation: unread-in 0.28s ease-out;
  }

  .row:hover .unread-dot,
  .row:focus-within .unread-dot {
    opacity: 0;
  }

  .chip-unread {
    flex: none;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--accent);
    animation: unread-in 0.28s ease-out;
  }

  /* One-shot arrival (not an infinite presence animation — nothing to pause
     while hidden). */
  @keyframes unread-in {
    from {
      opacity: 0;
      transform: scale(0.3);
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

  /* --- the rail's new-agent row (the launcher's split button) --- */

  .new-split {
    display: flex;
    align-items: stretch;
    gap: 1px;
    margin-top: 0.15rem;
  }

  .new-split .row.new.quick-start {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    margin-top: 0;
    gap: 6px;
    color: var(--fg);
    font-weight: 500;
  }

  .new-plus {
    flex: none;
    width: 0.7em;
    text-align: center;
    opacity: 0.75;
  }

  /* The agent's name gives way first when the rail is narrow (one ellipsis
     before a second); the verb shrinks only once the name is gone, so the
     narrowest rail still clips inside the button, never under the chevron. */
  .new-name {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* What the main surface will spawn — quiet transparency, not chrome. */
  .new-default {
    flex: 0 24 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
  }

  /* A missing agent: the click sets it up rather than spawning a doomed
     pane, so the row says which step — quiet, in the accent. */
  .new-need {
    margin-left: auto;
    flex: none;
    font-size: var(--text-xs);
    color: var(--accent);
  }

  .new-split .row.new.quick-start.want-setup .new-name {
    opacity: 0.8;
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

  .new-chev:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: -2px;
  }

  /* A managed worktree left behind: one quiet line, GitRepoSection's
     Remove/Keep language. */
  .wt-kept {
    display: flex;
    align-items: baseline;
    gap: 0.3rem;
    min-width: 0;
    padding: 2px 8px 4px;
    font-size: var(--text-xs);
    line-height: 1.35;
    color: var(--muted);
  }

  .wt-kept-msg {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .wt-kept-msg.err {
    white-space: normal;
    color: var(--err);
  }

  .wt-kept-name {
    font-family: var(--mono);
    color: var(--fg);
  }

  /* Two text buttons in the rail's own hover language (a tinted box, no
     underline); the keyboard ring is the app-wide one. */
  .wt-kept-btn {
    flex: none;
    appearance: none;
    border: none;
    background: none;
    padding: 1px 5px;
    font: inherit;
    color: var(--muted);
    cursor: pointer;
    border-radius: 4px;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }

  /* The last box overhangs its padding so Keep ends on the rows' text edge. */
  .wt-kept-btn:last-child {
    margin-right: -5px;
  }

  .wt-kept-btn.strong {
    color: var(--fg);
  }

  .wt-kept-btn:hover:not(:disabled) {
    color: var(--fg);
    background: var(--row-hover);
  }

  .wt-kept-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  /* The worktree choice is on: a tiny branch glyph after the agent name,
     inline so the split button never shifts. */
  .new-wt {
    flex: none;
    color: var(--muted);
    opacity: 0.8;
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

  /* Fills whatever the sessions column has left. Basis 0 AND min-height 0:
     with the automatic minimum the rendered rows would feed back into the
     box the row count is measured from (the section could grow to its
     high-water mark and never shrink). The list's min-height (3 rows) is
     the floor, below which the column scrolls. One fixed row height keeps
     the fit arithmetic exact. */
  .recents {
    --recent-row-h: 26px;
    margin-top: 0.55rem;
    flex: 1 1 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  /* Expanded: at least the old 240px window of scrollable history, even
     when the live roster leaves the column no slack. */
  .recents.expanded {
    min-height: 240px;
  }

  .recents-head {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 4px 8px;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--muted);
    user-select: none;
  }

  .recents-list {
    flex: 1;
    /* The floor: three rows + their gaps. Clip the rest — the rendered
       count (recentsFit) is what fits, so nothing is ever half-shown. */
    min-height: calc(var(--recent-row-h) * 3 + 2px);
    display: flex;
    flex-direction: column;
    gap: 1px;
    overflow: hidden;
  }

  /* Expanded: a scrollable window, soft edge fade when it overflows. */
  .recents-list.expanded {
    overflow-y: auto;
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
    flex: none;
    align-items: center;
    gap: 8px;
    height: var(--recent-row-h);
    padding: 0 8px;
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

  /* The "more" / "less" toggle lives in the section header: quiet lowercase
     text (the header's uppercase tracking is for the label only). */
  .recents-more {
    padding: 1px 4px;
    font: inherit;
    font-size: var(--text-xs);
    font-weight: 500;
    letter-spacing: 0;
    text-transform: none;
  }

  .recents-note {
    flex: none;
    display: flex;
    align-items: center;
    gap: 4px;
    height: var(--recent-row-h);
    padding: 0 8px;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .recents-undo {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--fg);
    cursor: pointer;
  }
  .recents-undo:hover {
    text-decoration: underline;
    text-underline-offset: 2px;
  }

  .recents-actions {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  /* All sessions: a quiet history mark. */
  .recents-history {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 2px 4px;
  }
  /* Both header actions show while the pointer is over Recents or a key
     focuses them, and always where there is no hover. */
  :is(.recents-more, .recents-history) {
    appearance: none;
    border: none;
    background: none;
    margin: -2px -4px -2px 0;
    border-radius: 4px;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease,
      background-color 0.12s ease;
  }
  .recents:hover :is(.recents-more, .recents-history),
  :is(.recents-more, .recents-history):focus-visible {
    opacity: 0.9;
  }
  /* Scoped under .recents so it outranks the reveal rule above (the
     pointer is over both), letting the hovered action reach full opacity. */
  .recents :is(.recents-more, .recents-history):hover {
    opacity: 1;
    color: var(--fg);
    background: var(--row-hover);
  }
  :is(.recents-more, .recents-history):focus-visible {
    outline: 1px solid var(--focus-ring);
  }
  @media (hover: none) {
    .recents-more,
    .recents-history {
      opacity: 0.9;
    }
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

  /* Keep file actions visible, with full hit targets even in a narrow rail. */
  .files-head {
    flex: none;
    display: flex;
    align-items: center;
    padding-right: 8px;
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
    padding: 6px;
    min-width: 28px;
    min-height: 28px;
    margin-right: 2px;
    border-radius: 5px;
    color: var(--muted);
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

  .files-finder:focus-visible,
  .files-header:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
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
    /* The tree is its own scroller (sticky ancestor rows need to live inside
       the element that scrolls); this box only sizes it. */
    overflow: hidden;
    /* Column so the tree can grow to fill — a right-click in the empty area
       below the last row then still lands on the tree's context menu. */
    display: flex;
    flex-direction: column;
  }

  /* Wraps: the identity group and the tools group each keep their content
     width, so when the two don't share a row the tools take a second one
     (flex lines break on content size, before any shrinking). */
  .daemon {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
    padding: 12px 16px 0;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  /* Identity: never grows (the tools sit right after it on a shared row);
     only an absurd host name ellipsizes, and only when it alone overflows. */
  .daemon-id {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 0 1 auto;
    min-width: 0;
    max-width: 100%;
  }

  /* Tools: take the slack (the gear's auto margin keeps it at the right
     edge) and, on a row of their own, the whole width. */
  .daemon-tools {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 1 1 auto;
    min-width: 0;
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

  /* Hidden document: nobody sees the reconnect pulse — stop burning frames
     (the html.app-hidden contract; see app.css). */
  :global(html.app-hidden) .daemon-dot.pulse {
    animation-play-state: paused;
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

  /* The measured link RTT, quiet next to the host: only rendered on a remote
     window once the estimate crosses 30 ms — a fast tunnel needs no badge. */
  .daemon-rtt {
    color: var(--muted);
    font-size: 10px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  /* Branch + dirty count: always-on git orientation, quiet until it matters.
     Shrinkable (the branch ellipsizes) so on a shared row it yields to the
     host name; on its own row it gets the width to spell the branch out. */
  .daemon-git {
    flex: 0 1 auto;
    min-width: 0;
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    gap: 0.22rem;
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

  /* A git diagnostic ("git too old", "can’t read repo") is the whole message:
     never ellipsize it — the bar wraps instead, now that it can. */
  .daemon-git.bad .dg-branch {
    max-width: none;
  }

  /* With several repositories the chip names the focused one first. */
  .dg-repo {
    color: var(--muted);
    max-width: 10em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* The branch's width budget scales with the rail (roughly what the old 50%
     chip cap left for the text after the icon, count and padding) with a
     10ch floor. The budget is what bounds the bar's wrap decision: flex
     lines break on content width, so an uncapped branch would put the tools
     on a second row for every long branch name. It does not prevent the
     wrap — a long, dirty branch still takes the second row at the default
     rail width; that row is the point, the host stays readable. */
  .dg-branch {
    font-family: var(--mono);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    max-width: max(10ch, calc(var(--rail-w, 230px) * 0.5 - 60px));
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

  /* Settings gear: the always-there mouse path to ⌘, — quiet until hover.
     flex: none so a crowded tools row shrinks the branch label, never the
     buttons' hit targets. */
  .daemon-settings {
    flex: none;
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
    display: flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 42%, transparent);
    border-radius: 7px;
    pointer-events: none;
  }

  /* The same label chip as Pane's band-label (one grammar for every preview). */
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

  /* The detached window's plain workspace label: same slot, no affordance.
     It doubles as titlebar drag surface, so its text must never select. */
  .strip-ws.inert {
    user-select: none;
  }
  .strip-ws.inert:hover {
    background: none;
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

  /* The asset-transition notice docks in the same top-centre slot and a
     blocked transition can sit there indefinitely, so the strip yields the
     slot and drops below it rather than the two overlapping. */
  .reconnect-status.stacked {
    top: calc(14px + var(--notice-stack-offset));
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

  /* Outcome chip: the upload-chip recipe, one line, auto-fading (showFlash). */
  .flash-chip {
    position: fixed;
    left: 50%;
    bottom: 14px;
    transform: translateX(-50%);
    z-index: 171; /* one over the upload stack; still under the update toast */
    max-width: 480px;
    padding: 5px 14px;
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 999px;
    box-shadow: 0 8px 28px color-mix(in srgb, var(--fg) 12%, transparent);
    font-size: var(--text-xs);
    color: var(--muted);
    animation: upload-in 0.16s ease-out;
  }

  /* The solo strip's flex grower (the chips row plays this role in normal
     windows; .strip-drag exists only under the macOS titlebar overlay). */
  .strip-spacer {
    align-self: stretch;
    flex: 1;
    min-width: 0;
  }

  /* The solo strip's one-click re-attach (dt windows with a known origin). */
  .strip-reattach {
    flex: none;
    padding: 2px 10px;
    background: transparent;
    border: 1px solid var(--edge);
    border-radius: 999px;
    color: var(--muted);
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .strip-reattach:hover {
    color: var(--fg);
    border-color: var(--accent);
  }

  /* The emptied detached window whose self-close the browser refused. */
  .detached-empty {
    position: absolute;
    inset: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .detached-empty button {
    padding: 5px 14px;
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    color: var(--fg);
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .detached-empty button:hover {
    border-color: var(--accent);
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

  .sheet-crash {
    position: fixed;
    left: 50%;
    top: 30%;
    transform: translateX(-50%);
    z-index: 110;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 14px 18px;
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.22);
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .sheet-crash button {
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 4px 12px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }
</style>

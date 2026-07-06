<script lang="ts">
  import { onMount, tick } from "svelte";
  import {
    ApiError,
    getActiveWorkspaceId,
    getHostLabel,
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
    displayName,
    dotState,
    dotTitle,
    listWorkspaces,
    needsAttention,
    pollSessions,
    type Session,
    type SessionKind,
    type Workspace,
  } from "./lib/sessions";
  import { EventsSocket } from "./lib/events";
  import { reconnectingSockets } from "./lib/ws";
  import {
    activateTab,
    allFilePaths,
    closePane,
    cycleTab,
    defaultLayout,
    deserializeLayout,
    detachTab,
    dropTab,
    findPane,
    focusPane,
    focusedFile as focusedFileOf,
    focusedSession as focusedSessionOf,
    moveFocus,
    moveTabToIndex,
    openFile,
    openSession,
    panes,
    pruneFiles,
    pruneSessions,
    serializeLayout,
    setRatio,
    splitPane,
    tabCount,
    toggleZoom,
    type FocusDir,
    type Layout,
    type SplitDir,
    type Tab,
  } from "./lib/layout";
  import { basename, fileTabTitles, fsProbe } from "./lib/files";
  import {
    paneContentEl,
    paneRootEl,
    startDrag,
    type DropSpot,
    type LayoutCtrl,
  } from "./lib/dnd";
  import { chordDigit, isAppChord, isLayer2, isMac, KEYS } from "./lib/keys";
  import * as pool from "./lib/termPool";
  import { flushViewState, loadViewState, saveViewState, windowKey } from "./lib/viewState";
  import FolderPicker from "./lib/FolderPicker.svelte";
  import FileTree from "./lib/FileTree.svelte";
  import SplitTree from "./lib/SplitNode.svelte";
  import Pane from "./lib/Pane.svelte";

  let health = $state<Health | null>(null);
  let workspaces = $state<Workspace[]>([]);
  let sessions = $state<Session[]>([]);
  let activeWsId = $state<string | null>(getActiveWorkspaceId());
  let pickerOpen = $state(false);
  let eventsUp = $state(false);
  let createError = $state<string | null>(null);
  /** Rail row currently showing the inline kill confirmation. */
  let confirmKillId = $state<string | null>(null);
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

  // Rail FILES section: collapsible, resizable share of the rail height.
  let filesOpen = $state(true);
  let filesFrac = $state(0.4);
  let filesDividerActive = $state(false);
  let railEl = $state<HTMLElement | null>(null);
  let daemonEl = $state<HTMLElement | null>(null);

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
  const sessionsById = $derived(new Map(sessions.map((s) => [s.id, s])));
  const focusedSessionId = $derived(focusedSessionOf(layout));
  const focusedFilePath = $derived(focusedFileOf(layout));
  /** Open file tabs' display titles (basename, disambiguated by parent dir). */
  const fileTitles = $derived(fileTabTitles(allFilePaths(layout)));
  const zoomedPane = $derived(
    layout.zoomedPaneId !== null ? findPane(layout.root, layout.zoomedPaneId) : null,
  );
  /** With more than one pane, every pane shows its tab bar (orientation). */
  const multiPane = $derived(panes(layout.root).length > 1);

  /** Sessions in the active workspace waiting on the user. */
  const needsYou = $derived(wsSessions.filter(needsAttention).length);

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
  $effect(() => {
    if (eventsUp) return;
    return pollSessions(applySessions, () => {
      // transient poll failure; the daemon dot already reflects reachability
    });
  });

  // Persist the layout (debounced in viewState) whenever it changes, keyed
  // by (window, workspace) so each workspace keeps its own tree.
  $effect(() => {
    const blob = { v: 1, ws: activeWsId, layout: serializeLayout(layout) };
    if (!layoutReady) return;
    saveViewState(stateKey(activeWsId), blob);
  });

  // Dispose pooled terminals for sessions that no longer exist.
  $effect(() => {
    const ids = sessions.map((s) => s.id);
    if (!gotSessions) return;
    pool.syncSessions(ids);
  });

  onMount(() => {
    pool.initPool({ onTitle, onExited, onSocketError });
    const events = new EventsSocket({
      onSessions: applySessions,
      onStatus: (up) => (eventsUp = up),
      onFatal: (message) => {
        if (message === "unauthorized") notifyUnauthorized();
      },
    });
    refreshWorkspaces();
    void bootViewState();

    const onPagehide = () => void flushViewState();
    window.addEventListener("keydown", onKeydown, true);
    window.addEventListener("pagehide", onPagehide);
    return () => {
      window.removeEventListener("keydown", onKeydown, true);
      window.removeEventListener("pagehide", onPagehide);
      events.close();
      pool.disposePool();
    };
  });

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

    if (key === "d" && plain) {
      intercept();
      split(l2 ? "col" : "row");
      return;
    }
    if (key === "e" && plain) {
      intercept();
      void newSession(l2 ? "agent" : "shell");
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
    const n = chordDigit(e);
    if (n !== null && !l2 && plain && n <= wsSessions.length) {
      intercept();
      openSess(wsSessions[n - 1].id);
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

  /** Scope this window to `w` (open in THIS window). */
  function activateWorkspace(w: Workspace): void {
    workspaces = workspaces.some((x) => x.id === w.id)
      ? workspaces.map((x) => (x.id === w.id ? w : x))
      : [w, ...workspaces];
    const switched = activeWsId !== w.id;
    closePicker();
    createError = null;
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

  function applySessions(list: Session[]): void {
    list.sort((a, b) => a.created_at - b.created_at || a.id.localeCompare(b.id));
    sessions = list;
    gotSessions = true;
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
      if (tabCount(layout) === 0 && wsSessions.length > 0) {
        layout = openSession(layout, wsSessions[0].id);
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

  async function newSession(kind: SessionKind): Promise<void> {
    if (activeWsId === null) {
      openPicker();
      return;
    }
    createError = null;
    try {
      const s = await createSession(activeWsId, kind, null, spawnSize());
      recentlyCreated.set(s.id, Date.now());
      // A racing events snapshot may already have delivered the session.
      if (!sessions.some((x) => x.id === s.id)) sessions.push(s);
      // The new session opens as the active tab in the focused pane,
      // focused, immediately — never an invisible rail-only row.
      openSess(s.id);
    } catch (e) {
      // Both kinds surface an inline error (409 when claude is missing,
      // "unauthorized"/network noise otherwise).
      const what = kind === "agent" ? "agent" : "terminal";
      createError = e instanceof ApiError ? e.message : `failed to start ${what}`;
    }
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
  };

  /** Shared drag start for rail rows and pane tabs (any surface). */
  function beginDrag(e: PointerEvent, tab: Tab, onClick: () => void): void {
    const label =
      tab.surface === "terminal"
        ? (displayNames.get(tab.sessionId) ??
          sessionsById.get(tab.sessionId)?.name ??
          tab.sessionId.slice(0, 8))
        : (fileTitles.get(tab.path) ?? basename(tab.path));
    startDrag(
      e,
      { tab, label },
      {
        onSpot: (s) => (dropSpot = s),
        onDrop: (spot) => {
          layout =
            spot.kind === "tab"
              ? moveTabToIndex(layout, tab, spot.paneId, spot.index)
              : dropTab(layout, tab, spot.paneId, spot.zone);
          if (tab.surface === "terminal") pool.focusTerminal(tab.sessionId);
        },
        onClick,
        onEnd: () => (dropSpot = null),
      },
    );
  }

  function onRailRowDown(e: PointerEvent, sessionId: string): void {
    beginDrag(e, { surface: "terminal", sessionId }, () => openSess(sessionId));
  }

  /** Svelte action: focus the node as soon as it mounts (confirm buttons). */
  function focusOnMount(node: HTMLElement): void {
    node.focus();
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
      filesFrac = Math.min(Math.max(h / railH, 0.12), 0.8);
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
</script>

<div class="shell">
  <div class="body">
    <aside class="rail" class:collapsed={layout.focusMode} bind:this={railEl}>
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
      </div>

      <nav class="sessions">
        {#each wsSessions as s (s.id)}
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
              <span class="dot {dotState(s)}" title={dotTitle(s)}></span>
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
              role="button"
              tabindex="0"
              onpointerdowncapture={(e) => {
                // Capture-phase (directly attached); the close button stays a
                // plain click.
                if (e.target instanceof Element && e.target.closest(".close")) return;
                onRailRowDown(e, s.id);
              }}
              onkeydown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  openSess(s.id);
                }
              }}
            >
              <span class="dot {dotState(s)}" title={dotTitle(s)}></span>
              <span class="labels">
                <span class="name">{displayNames.get(s.id) ?? displayName(s)}</span>
                {#if s.title && s.title !== s.name && s.title !== s.agent_title}
                  <span class="title">{s.title}</span>
                {/if}
              </span>
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
        {/each}
        <button
          class="row new primary"
          title="start a Claude agent ({KEYS.newAgent})"
          onclick={() => void newSession("agent")}>+ new agent</button
        >
        <button
          class="row new"
          title="open a terminal ({KEYS.newTerminal})"
          onclick={() => void newSession("shell")}>+ terminal</button
        >
        {#if createError}
          <div class="create-error">{createError}</div>
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
          {#if filesOpen}
            <div class="files-body">
              <FileTree root={workspace.root} onOpen={openFilePath} activePath={focusedFilePath} />
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
        <span class="daemon-host" title={health?.hostname}>{getHostLabel()}</span>
      </div>
    </aside>

    <main class="stage">
      {#if activeWsId === null}
        <div class="empty">
          <button class="open-cta" onclick={openPicker}>Open a folder</button>
        </div>
      {:else if layoutReady}
        {#if zoomedPane !== null}
          <Pane
            node={zoomedPane}
            focusedPaneId={layout.focusedPaneId}
            zoomed
            forceTabs={multiPane}
            {dropSpot}
            sessions={sessionsById}
            names={displayNames}
            fileNames={fileTitles}
            {ctrl}
          />
        {:else}
          <SplitTree
            node={layout.root}
            focusedPaneId={layout.focusedPaneId}
            forceTabs={multiPane}
            {dropSpot}
            sessions={sessionsById}
            names={displayNames}
            fileNames={fileTitles}
            {ctrl}
          />
        {/if}
      {/if}
    </main>
  </div>

  {#if layout.focusMode}
    <!-- Focus-mode session strip: the rail is gone, but the window always
         says where you are. Hidden whenever the rail is visible. -->
    <footer class="strip">
      <button
        class="strip-ws"
        title="show sidebar ({KEYS.focusMode})"
        onclick={() => (layout = { ...layout, focusMode: false })}
        >{workspace?.name ?? "chimaera"}</button
      >
      <div class="chips">
        {#each wsSessions as s (s.id)}
          <button
            class="chip"
            class:focused={s.id === focusedSessionId}
            title={s.title ?? undefined}
            onclick={() => openSess(s.id)}
          >
            <span class="dot {dotState(s)}" title={dotTitle(s)}></span>
            <span class="chip-name"
              >{s.kind === "shell" ? "$ " : ""}{displayNames.get(s.id) ?? displayName(s)}</span
            >
          </button>
        {/each}
      </div>
      {#if needsYou > 0}
        <span class="strip-needs">{needsYou} need{needsYou === 1 ? "s" : ""} you</span>
      {/if}
      <span class="strip-host" title={health?.hostname}>{getHostLabel()}</span>
    </footer>
  {/if}
</div>

{#if pickerOpen}
  <FolderPicker recents={workspaces} onOpened={activateWorkspace} onClose={closePicker} />
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
    width: 230px;
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

  /* Focus mode: the rail collapses to nothing; the strip carries context. */
  .rail.collapsed {
    width: 0;
    padding-left: 0;
    padding-right: 0;
    opacity: 0;
    visibility: hidden;
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

  .labels {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    line-height: 1.3;
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

  .create-error {
    padding: 2px 8px 4px;
    font-size: var(--text-xs);
    line-height: 1.35;
    color: var(--err);
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

  .files-header {
    flex: none;
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

  .stage {
    flex: 1;
    display: flex;
    min-width: 0;
    min-height: 0;
    position: relative;
    background: var(--bg);
    padding: 8px;
  }

  .empty {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    font-size: var(--text-md);
  }

  .open-cta {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-md);
    color: var(--muted);
    cursor: pointer;
    transition: color 0.12s ease;
  }

  .open-cta:hover {
    color: var(--fg);
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
</style>

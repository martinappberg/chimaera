<script lang="ts">
  /**
   * The pane's always-present top bar (~26px): type glyph + tab name per
   * tab (active emphasized by WEIGHT, not color), pane controls at the
   * right edge (fade in on bar hover; the zoom badge stays persistent
   * while zoomed). The bar's empty area is a drag handle for the active
   * tab, so every pane can always be re-tiled by its bar.
   *
   * Surface parity: terminal and file tabs share one anatomy — glyph +
   * name + close, same drag, same middle-click close, same dblclick zoom.
   * A terminal's glyph carries its session-state color.
   */
  import { tabKey, type PaneNode, type Tab } from "./layout";
  import type { Session } from "../workspace/sessions";
  import { dotState, dotTitle, renameSession, switchingViews } from "../workspace/sessions";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import type { DropSpot, LayoutCtrl } from "./dnd";
  import { agentHue, type LinkCtrl } from "../workspace/agentLinks";
  import { basename, fsDownload, midTruncate, viewKindFor } from "../previews/files";
  import { isRemoteHost } from "../net/api";
  import { fsRenameOp } from "../workspace/fsEvents";
  import { stemLength, validateEntryName } from "../shared/fsNames";
  import { contextMenu, type ContextMenuEntry } from "../shared/contextMenu.svelte";
  import { writeClipboard } from "../net/native";
  import { dirtyFiles } from "../shared/editing";
  import { gitIndex } from "../workspace/git";
  import { decoFor } from "../workspace/gitDeco";
  import { PINNED } from "../shared/keys";
  import { keyHint } from "../shared/keybindings";
  import { activeSelection, referenceTarget, requestReference } from "../shared/reference";
  import { dismiss } from "../shared/dismiss";
  import FileIcon from "../shared/FileIcon.svelte";
  import FolderIcon from "../shared/FolderIcon.svelte";

  interface Props {
    node: PaneNode;
    /** True while this pane is rendered zoomed (fullscreen in the window). */
    zoomed?: boolean;
    /** True when this is the only pane — the grip (which moves a pane between
     *  splits) has nowhere to go, so it hides. */
    soloPane?: boolean;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    /** Open-file tab titles (basename, disambiguated), keyed by path. */
    fileNames: Map<string, string>;
    /** terminal session id -> agent session id (the linked-terminal edges). */
    links: Map<string, string>;
    linkCtrl: LinkCtrl;
    dropSpot: DropSpot | null;
    ctrl: LayoutCtrl;
    /** Bound by Pane so the dnd hit-tester can target this bar. */
    el?: HTMLElement | null;
  }

  let {
    node,
    zoomed = false,
    soloPane = false,
    sessions,
    names,
    fileNames,
    links,
    linkCtrl,
    dropSpot,
    ctrl,
    el = $bindable(null),
  }: Props = $props();

  const activeSession = $derived.by(() => {
    const tab = node.tabs[node.active];
    return tab !== undefined && tab.surface === "terminal"
      ? (sessions.get(tab.sessionId) ?? null)
      : null;
  });

  /** Chips on an AGENT pane: the terminals this agent holds a leash to. */
  const linkedTerminals = $derived.by(() => {
    if (activeSession === null || activeSession.kind !== "agent") return [];
    const out: string[] = [];
    for (const [terminal, agent] of links) {
      if (agent === activeSession.id) out.push(terminal);
    }
    return out;
  });

  /** Back-reference on a TERMINAL pane: the agent holding its leash. */
  const linkedAgentId = $derived(
    activeSession !== null && activeSession.kind === "shell"
      ? (links.get(activeSession.id) ?? null)
      : null,
  );

  function sessionLabel(id: string): string {
    return names.get(id) ?? sessions.get(id)?.name ?? id.slice(0, 8);
  }

  /** Chip dot modifier for a linked terminal: what is that shell doing? */
  function chipState(id: string): string {
    const s = sessions.get(id);
    if (s === undefined || !s.alive) return "quiet";
    if (s.exec_stage === "executing") return "exec";
    if (s.exec_stage === "queued") return "queued";
    if (s.phase === "running") return "busy";
    return "ready";
  }

  function chipTitle(id: string): string {
    const s = sessions.get(id);
    const doing =
      s === undefined || !s.alive
        ? "exited"
        : s.exec_stage === "executing"
          ? "agent is running a command here"
          : s.exec_stage === "queued"
            ? "agent exec queued for the prompt"
            : s.phase === "running"
              ? "a command is running"
              : "at the prompt";
    return `linked terminal · ${doing} — click to focus`;
  }

  /** The chat⇄terminal toggle target for the active agent session, when the
   *  agent supports the structured chat surface; null hides the control. */
  const viewToggle = $derived.by(() => {
    if (activeSession === null || activeSession.kind !== "agent") return null;
    if (activeSession.chat_capable !== true) return null;
    return activeSession.ui === "chat" ? ("term" as const) : ("chat" as const);
  });

  /** A view switch for the active session is in flight (its POST hasn't
   *  resolved): the toggle disables itself so a second click can't fire a
   *  concurrent switch. */
  const switchPending = $derived(
    activeSession !== null && $switchingViews.has(activeSession.id),
  );

  // --- "link to agent…" menu (parity path for the drag gesture) ------------

  let linkMenuOpen = $state(false);

  /** Live agents in this terminal's workspace, offered by the link menu. */
  const agentChoices = $derived.by(() => {
    if (activeSession === null || activeSession.kind !== "shell") return [];
    return [...sessions.values()].filter(
      (s) =>
        s.kind === "agent" && s.alive && s.workspace_id === activeSession.workspace_id,
    );
  });

  function chooseAgent(agentId: string): void {
    if (activeSession === null) return;
    if (linkedAgentId === agentId) {
      linkCtrl.unlink(activeSession.id);
    } else {
      linkCtrl.link(activeSession.id, agentId);
    }
    linkMenuOpen = false;
  }

  /** Insertion index while a drag hovers this tab bar, else null. */
  const insertIndex = $derived(
    dropSpot?.kind === "tab" && dropSpot.paneId === node.id ? dropSpot.index : null,
  );

  /**
   * Context bridge: true when this pane's ACTIVE tab is the terminal that
   * owns the current selection — the bar grows a quiet "reference" action
   * (the terminal itself has no floating overlay; the bar is its affordance).
   */
  const hasTermSelection = $derived.by(() => {
    const sel = $activeSelection;
    const active = node.tabs[node.active];
    return (
      sel !== null &&
      sel.kind === "terminal" &&
      active !== undefined &&
      active.surface === "terminal" &&
      active.sessionId === sel.sessionId
    );
  });

  function label(tab: Tab): string {
    if (tab.surface === "terminal") {
      return names.get(tab.sessionId) ?? sessions.get(tab.sessionId)?.name ?? tab.sessionId.slice(0, 8);
    }
    if (tab.surface === "settings") return "Settings";
    if (tab.surface === "finder") return basename(tab.path) || "Finder";
    if (tab.surface === "git") return "Source Control";
    if (tab.surface === "diff") return `${basename(tab.path)} (diff)`;
    if (tab.surface === "changes") {
      const n = names.get(tab.sessionId) ?? sessions.get(tab.sessionId)?.name;
      return n !== undefined ? `Changes · ${n}` : "Changes";
    }
    return fileNames.get(tab.path) ?? basename(tab.path);
  }

  /** The pane's active tab, when it is a terminal (font controls target). */
  const activeTerminal = $derived.by(() => {
    const t = node.tabs[node.active];
    return t !== undefined && t.surface === "terminal" ? t : null;
  });

  /** Surfaces that carry a per-pane text size: terminals and rendered
   *  markdown documents both grow the A−/A+ controls (same handler). */
  const fontTarget = $derived.by(() => {
    const t = node.tabs[node.active];
    if (t === undefined) return false;
    // Chat panes have no font-sizable surface: ChatView never reads
    // node.fontSize, and a chord writing it would surprise on toggle-to-
    // terminal. Only real terminal surfaces and rendered markdown carry A−/A+.
    if (t.surface === "terminal") return sessions.get(t.sessionId)?.ui !== "chat";
    return t.surface === "file" && viewKindFor(t.path) === "markdown";
  });

  /**
   * Touched-files chip: agent panes get a quiet "N files" chip once the
   * session's hook-derived files_touched list is non-empty; newest last on
   * the wire, newest FIRST in the popover.
   */
  const touched = $derived.by(() => {
    if (activeTerminal === null) return null;
    const s = sessions.get(activeTerminal.sessionId);
    if (s === undefined || s.kind !== "agent") return null;
    const files = s.files_touched;
    return files != null && files.length > 0 ? files : null;
  });

  /** The chip opens this session's changes review (its touched files, on the
   *  git status/diff APIs) in a pane — Cmd/Ctrl forces a fresh split. */
  function openChanges(e: MouseEvent): void {
    if (activeTerminal === null) return;
    ctrl.openChangesFrom(node.id, activeTerminal.sessionId, e.metaKey || e.ctrlKey);
  }

  /** Empty bar area drags the pane's ACTIVE tab (capture runs before the
   *  tabs' own handlers; anything inside a tab or a button is theirs). */
  function onBarPointerDown(e: PointerEvent): void {
    if (!(e.target instanceof Element)) return;
    if (e.target.closest("[data-tab-index], button") !== null) return;
    const active = node.tabs[node.active];
    if (active !== undefined) ctrl.dragTab(e, node.id, node.active, active);
  }

  // --- tab context menu + inline rename --------------------------------------
  //
  // The "master name" pattern: renaming a terminal/chat tab pins the SESSION
  // name (the same PATCH the rail rows and /rename use); renaming a file tab
  // renames the file ON DISK (open tabs follow via App's mutation
  // subscription). The tab label is never an independent alias.

  /** tabKey of the tab being renamed inline, if any. */
  let renamingTab = $state<string | null>(null);
  let renameDraft = $state("");
  let renameError = $state<string | null>(null);

  function beginTabRename(tab: Tab): void {
    renamingTab = tabKey(tab);
    renameDraft = tab.surface === "file" ? basename(tab.path) : label(tab);
    renameError = null;
  }

  function cancelTabRename(): void {
    renamingTab = null;
    renameDraft = "";
    renameError = null;
  }

  /** Focus the rename input; file names preselect the stem. */
  function renameFocus(node: HTMLInputElement, selectStem: boolean): void {
    node.focus();
    if (selectStem) node.setSelectionRange(0, stemLength(node.value));
  }

  async function commitTabRename(tab: Tab, viaBlur = false): Promise<void> {
    if (renamingTab !== tabKey(tab)) return;
    const name = renameDraft.trim();
    if (name === "") {
      cancelTabRename();
      return;
    }
    if (tab.surface === "terminal") {
      cancelTabRename();
      if (name !== label(tab)) {
        renameSession(tab.sessionId, name).catch(() => {
          // next sessions snapshot restores the truth (rail-rename semantics)
        });
      }
      return;
    }
    if (tab.surface !== "file") {
      cancelTabRename();
      return;
    }
    if (name === basename(tab.path)) {
      cancelTabRename();
      return;
    }
    const invalid = validateEntryName(name, { allowSlashes: false });
    if (invalid !== null) {
      if (viaBlur) cancelTabRename();
      else renameError = invalid;
      return;
    }
    try {
      const i = tab.path.lastIndexOf("/");
      const parent = i > 0 ? tab.path.slice(0, i) : "";
      await fsRenameOp(tab.path, `${parent}/${name}`);
      cancelTabRename(); // App's mutation subscription rewrites this tab
    } catch (e) {
      renameError = e instanceof Error ? e.message : "rename failed";
    }
  }

  function onRenameKeydown(e: KeyboardEvent, tab: Tab): void {
    e.stopPropagation(); // chords and terminal keys stay out
    if (e.key === "Enter") {
      e.preventDefault();
      void commitTabRename(tab);
    } else if (e.key === "Escape") {
      e.preventDefault();
      cancelTabRename(); // nulled before blur fires, so blur no-ops
    }
  }

  async function copyPath(p: string): Promise<void> {
    if (await writeClipboard(p)) return;
    try {
      await navigator.clipboard.writeText(p);
    } catch {
      // clipboard unavailable — quiet
    }
  }

  function tabMenu(tab: Tab, i: number): ContextMenuEntry[] {
    const close: ContextMenuEntry = {
      label: "Close",
      onSelect: () => ctrl.closeTab(node.id, i),
    };
    if (tab.surface === "terminal") {
      return [{ label: "Rename…", onSelect: () => beginTabRename(tab) }, "separator", close];
    }
    if (tab.surface === "file") {
      const dirty = $dirtyFiles.has(tab.path);
      return [
        ...(tab.preview === true
          ? [{ label: "Keep Open", onSelect: () => ctrl.pinTab(node.id, i) } as ContextMenuEntry, "separator" as const]
          : []),
        {
          label: "Rename…",
          disabled: dirty,
          hint: dirty ? "save the file first — renaming would drop unsaved edits" : undefined,
          onSelect: () => beginTabRename(tab),
        },
        { label: "Reveal in File Tree", onSelect: () => ctrl.revealPathInTree(tab.path) },
        "separator",
        ...(isRemoteHost()
          ? [{ label: "Download", onSelect: () => void fsDownload(tab.path) } as ContextMenuEntry]
          : []),
        { label: "Copy Path", onSelect: () => void copyPath(tab.path) },
        "separator",
        close,
      ];
    }
    return [close];
  }
</script>

<div class="bar" bind:this={el} onpointerdowncapture={onBarPointerDown}>
  {#if !zoomed && !soloPane}
    <!-- Pane grip: fades in on bar hover; drag it to move the WHOLE pane (all
         tabs) to another split. A plain click focuses the pane. Being a
         <button>, the bar's active-tab drag ignores it (closest("button")). -->
    <button
      class="pane-grip"
      title="drag to move this pane"
      aria-label="move pane"
      onpointerdown={(e) => ctrl.dragPane(e, node.id)}
    >
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
        <rect x="2.5" y="3" width="11" height="10" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.3" />
        <line x1="2.5" y1="6" x2="13.5" y2="6" stroke="currentColor" stroke-width="1.3" />
      </svg>
    </button>
  {/if}
  <div class="tabs" role="tablist">
    {#each node.tabs as tab, i (tabKey(tab))}
      {@const sid = tab.surface === "terminal" ? tab.sessionId : null}
      {@const ts = sid !== null ? (sessions.get(sid) ?? null) : null}
      {@const fEntry = tab.surface === "file" ? $gitIndex.files.get(tab.path) : undefined}
      {@const fDeco = fEntry ? decoFor(fEntry) : null}
      <div
        class="tab"
        class:active={i === node.active}
        class:insert={insertIndex === i}
        class:link-target={dropSpot?.kind === "linktab" &&
          dropSpot.paneId === node.id &&
          dropSpot.index === i}
        style:--hue={sid !== null && ts?.kind === "agent" ? agentHue(sid) : null}
        data-link-agent={sid !== null && ts?.kind === "agent" && ts.alive ? sid : undefined}
        role="tab"
        aria-selected={i === node.active}
        tabindex="-1"
        data-tab-index={i}
        title={tab.surface === "file" || tab.surface === "finder" || tab.surface === "diff"
          ? tab.path
          : label(tab)}
        onpointerdowncapture={(e) => {
          // Capture-phase (directly attached, not delegated); ignore presses
          // on the close button and the rename input so they stay plain
          // interactive targets.
          if (e.target instanceof Element && e.target.closest(".tab-close, .tab-rename-input"))
            return;
          ctrl.dragTab(e, node.id, i, tab);
        }}
        onauxclick={(e) => {
          // Middle-click closes the tab (detaches the view, never the session).
          if (renamingTab === tabKey(tab)) return;
          if (e.button === 1) {
            e.preventDefault();
            ctrl.closeTab(node.id, i);
          }
        }}
        ondblclick={() => {
          if (renamingTab === tabKey(tab)) return;
          // VS Code: double-clicking a PREVIEW (italic) file tab pins it;
          // otherwise the pane zooms (the long-standing gesture).
          if (tab.surface === "file" && tab.preview === true) {
            ctrl.pinTab(node.id, i);
          } else {
            ctrl.zoomPane(node.id);
          }
        }}
        oncontextmenu={(e) => contextMenu.openAt(e, tabMenu(tab, i))}
      >
        {#if tab.surface === "terminal"}
          {@const s = sessions.get(tab.sessionId)}
          <!-- Session-type glyph (agent_kind-driven) carrying the state color. -->
          <SessionGlyph
            kind={s?.kind ?? "shell"}
            agentKind={s?.agent_kind}
            state={s ? dotState(s) : ""}
            size={10}
            title={s ? dotTitle(s) : "terminal"}
          />
        {:else if tab.surface === "settings"}
          <svg class="glyph" viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
            <title>settings</title>
            <circle cx="8" cy="8" r="2.2" fill="none" stroke="currentColor" stroke-width="1.4" />
            <path
              d="M8 1.8v2M8 12.2v2M1.8 8h2M12.2 8h2M3.6 3.6l1.4 1.4M11 11l1.4 1.4M12.4 3.6L11 5M5 11l-1.4 1.4"
              fill="none"
              stroke="currentColor"
              stroke-width="1.4"
              stroke-linecap="round"
            />
          </svg>
        {:else if tab.surface === "finder"}
          <span class="tab-glyph" class:on={i === node.active}>
            <FolderIcon size={13} plain={i === node.active} />
          </span>
        {:else if tab.surface === "git"}
          <svg class="glyph" viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
            <title>source control</title>
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
        {:else if tab.surface === "diff"}
          <svg class="glyph" viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
            <title>diff</title>
            <path
              d="M2.5 2.5h4v11h-4zM9.5 2.5h4v11h-4z"
              fill="none"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linejoin="round"
            />
            <path d="M3.6 6.2h1.8M10.6 6.2h1.8M11.5 5.3v1.8" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
          </svg>
        {:else if tab.surface === "changes"}
          <svg class="glyph" viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
            <title>changes</title>
            <path
              d="M4 2v5m0 0a2 2 0 1 0 0 4m0-4a2 2 0 1 1 0 4m0 0v3M12 14V9m0 0a2 2 0 1 0 0-4m0 4a2 2 0 1 1 0-4m0 0V2"
              fill="none"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linecap="round"
            />
          </svg>
        {:else if $dirtyFiles.has(tab.path)}
          <!-- Dirty dot replaces the type glyph in its slot (unsaved edits). -->
          <span class="dirty-dot" title="unsaved changes"></span>
        {:else}
          <span class="tab-glyph" class:on={i === node.active}>
            <FileIcon path={tab.path} size={13} plain={i === node.active} />
          </span>
        {/if}
        {#if renamingTab === tabKey(tab)}
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="tab-rename-input"
            class:invalid={renameError !== null}
            type="text"
            spellcheck="false"
            autocomplete="off"
            aria-label="rename"
            title={renameError ?? undefined}
            bind:value={renameDraft}
            use:renameFocus={tab.surface === "file"}
            onkeydown={(e) => onRenameKeydown(e, tab)}
            onblur={() => void commitTabRename(tab, true)}
          />
        {:else}
          <span
            class="tab-name"
            class:preview={tab.surface === "file" && tab.preview === true}
            style:color={fDeco ? fDeco.color : undefined}>{label(tab)}</span
          >
        {/if}
        <button
          class="tab-close"
          aria-label="close tab"
          title="close tab"
          onclick={(e) => {
            e.stopPropagation();
            ctrl.closeTab(node.id, i);
          }}>&times;</button
        >
      </div>
    {/each}
    <div class="tab-tail" class:insert={insertIndex === node.tabs.length}></div>
  </div>

  <!-- Linked-terminal chips: on an agent pane, the complete map of the
       terminals it holds; on a linked terminal, the way back to its agent.
       Always visible (the bond is state, not chrome), hue = the agent's. -->
  {#if linkedTerminals.length > 0 && activeSession !== null}
    {@const hue = agentHue(activeSession.id)}
    <div class="links" role="group" aria-label="linked terminals">
      {#each linkedTerminals as tid (tid)}
        <span class="chip" style:--hue={hue}>
          <button class="chip-main" title={chipTitle(tid)} onclick={() => linkCtrl.reveal(tid, node.id)}>
            <span class="chip-dot {chipState(tid)}"></span>
            <span class="chip-name">{sessionLabel(tid)}</span>
          </button>
          <button class="chip-x" title="unlink" aria-label="unlink {sessionLabel(tid)}" onclick={() => linkCtrl.unlink(tid)}>&times;</button>
        </span>
      {/each}
    </div>
  {:else if linkedAgentId !== null && activeSession !== null}
    <div class="links" role="group" aria-label="linked agent">
      <span class="chip" style:--hue={agentHue(linkedAgentId)}>
        <button
          class="chip-main"
          title="linked to this agent — click to jump"
          onclick={() => linkCtrl.reveal(linkedAgentId, node.id)}
        >
          <svg class="chip-spark" viewBox="0 0 16 16" width="9" height="9" aria-hidden="true">
            <path d="M8 1.5v13M1.5 8h13M3.9 3.9l8.2 8.2M12.1 3.9l-8.2 8.2" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
          </svg>
          <span class="chip-name">{sessionLabel(linkedAgentId)}</span>
        </button>
        <button class="chip-x" title="unlink" aria-label="unlink from agent" onclick={() => linkCtrl.unlink(activeSession.id)}>&times;</button>
      </span>
    </div>
  {/if}

  <!-- Pane controls at the bar's right edge: the mouse path to every pane
       chord (tooltips teach the chords). Faded in on bar hover; the zoom
       badge stays persistent while zoomed. -->
  <div
    class="bar-right"
    use:dismiss={{ enabled: linkMenuOpen, onDismiss: () => (linkMenuOpen = false) }}
  >
    {#if hasTermSelection}
      <!-- Selection-driven, not hover-driven: visible exactly while the
           terminal holds a selection. Same handler as the chord. -->
      <button
        class="ref-btn"
        disabled={$referenceTarget === null}
        title={$referenceTarget === null
          ? "no agent session in this workspace — start one to reference"
          : `reference selection in ${$referenceTarget.name} (${PINNED.reference})`}
        onclick={(e) => {
          e.stopPropagation();
          requestReference();
        }}
      >
        <span class="ref-at" aria-hidden="true">@</span>
        reference
        {#if $referenceTarget !== null}
          <!-- The destination is always named — never a mystery landing. -->
          <span class="ref-target">→ {midTruncate($referenceTarget.name, 16)}</span>
        {/if}
      </button>
    {/if}
    {#if touched !== null}
      <!-- Hook-derived "files changed": a quiet chip that opens the session's
           git-diff review in a pane. Not hover-gated — it is information. -->
      <button
        class="touched-chip"
        title="review the {touched.length} file{touched.length === 1
          ? ''
          : 's'} this agent changed — opens a diff (⌘-click for a new split)"
        onclick={(e) => {
          e.stopPropagation();
          openChanges(e);
        }}
      >
        <svg class="touched-edit" viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M4 20h4l10.5 -10.5a2.828 2.828 0 1 0 -4 -4l-10.5 10.5v4" />
          <path d="M13.5 6.5l4 4" />
        </svg>
        {touched.length} file{touched.length === 1 ? "" : "s"} changed
      </button>
    {/if}
    {#if zoomed}
      <!-- Persistent while zoomed — the always-visible exit. Reads as an
           action ("restore"), not a state ("zoom"): the pane is already
           zoomed, so a collapse glyph + label says what the click DOES. -->
      <button
        class="zoom-badge"
        title="restore — exit zoom ({keyHint("zoom")})"
        aria-label="restore pane"
        onclick={() => ctrl.zoomPane(node.id)}
      >
        <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
          <path
            d="M2.5 9.5h4v4M13.5 6.5h-4v-4M9.5 6.5L14 2M2 14L6.5 9.5"
            fill="none"
            stroke="currentColor"
            stroke-width="1.3"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
        <span>restore</span>
      </button>
    {/if}
    <div class="controls">
      {#if viewToggle !== null && activeSession !== null}
        <!-- The same conversation, the other surface: stops the process and
             resumes it in chat / as the real TUI (same session id). -->
        <button
          class="ctl"
          class:pending={switchPending}
          disabled={switchPending}
          title={switchPending
            ? "switching…"
            : viewToggle === "chat"
              ? "open as chat"
              : "open as terminal"}
          aria-label={viewToggle === "chat" ? "open as chat" : "open as terminal"}
          onclick={() => ctrl.switchView(activeSession.id, viewToggle)}
        >
          {#if viewToggle === "chat"}
            <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
              <path
                d="M3 3.5h10a1 1 0 011 1v6a1 1 0 01-1 1H8l-3 2.5v-2.5H3a1 1 0 01-1-1v-6a1 1 0 011-1z"
                fill="none"
                stroke="currentColor"
                stroke-width="1.3"
                stroke-linejoin="round"
              />
            </svg>
          {:else}
            <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
              <rect x="2" y="3" width="12" height="10" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.3" />
              <path d="M4.5 6.5l2 1.7-2 1.7M8 10.5h3.5" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" />
            </svg>
          {/if}
        </button>
      {/if}
      {#if agentChoices.length > 0}
        <!-- The link icon is a drag handle: drag it onto an agent pane to
             link this terminal there (drop on the "link to this agent" band).
             A plain click (or Enter/Space) opens the picker menu as the
             parity path. -->
        <button
          class="ctl"
          class:on={linkMenuOpen}
          title={linkedAgentId !== null
            ? "linked — drag onto an agent to move · click to choose"
            : "drag onto an agent to link · click to choose"}
          aria-label="link to agent"
          aria-haspopup="menu"
          aria-expanded={linkMenuOpen}
          onpointerdown={(e) => {
            if (activeTerminal !== null) {
              ctrl.dragSurface(e, activeTerminal, () => (linkMenuOpen = !linkMenuOpen));
            }
          }}
          onkeydown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              linkMenuOpen = !linkMenuOpen;
            }
          }}
        >
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <path
              d="M6.5 9.5l3-3M5 7l-1.8 1.8a2.3 2.3 0 003.2 3.2L8.2 10.2M11 9l1.8-1.8a2.3 2.3 0 00-3.2-3.2L7.8 5.8"
              fill="none"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linecap="round"
            />
          </svg>
        </button>
      {/if}
      {#if fontTarget}
        <!-- Parity with the Cmd/Ctrl +/−/0 chords: per-pane text size
             (terminals and rendered markdown alike). -->
        <button
          class="ctl ctl-font"
          title="smaller text ({PINNED.fontMinus})"
          aria-label="smaller text"
          onclick={() => ctrl.adjustFont(node.id, -1)}>A−</button
        >
        <button
          class="ctl ctl-font"
          title="larger text ({PINNED.fontPlus}) · reset {PINNED.fontReset}"
          aria-label="larger text"
          onclick={() => ctrl.adjustFont(node.id, 1)}>A+</button
        >
      {/if}
      <button
        class="ctl"
        title="split right ({keyHint("splitRight")})"
        aria-label="split right"
        onclick={() => ctrl.splitPaneAt(node.id, "row")}
      >
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
          <rect x="2" y="3" width="12" height="10" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.3" />
          <line x1="8" y1="3" x2="8" y2="13" stroke="currentColor" stroke-width="1.3" />
        </svg>
      </button>
      <button
        class="ctl"
        title="split down ({keyHint("splitDown")})"
        aria-label="split down"
        onclick={() => ctrl.splitPaneAt(node.id, "col")}
      >
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
          <rect x="2" y="3" width="12" height="10" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.3" />
          <line x1="2" y1="8" x2="14" y2="8" stroke="currentColor" stroke-width="1.3" />
        </svg>
      </button>
      {#if !zoomed}
        <button class="ctl" title="zoom ({keyHint("zoom")})" aria-label="zoom" onclick={() => ctrl.zoomPane(node.id)}>
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <path d="M9.5 2.5h4v4M6.5 13.5h-4v-4M13.5 2.5L9 7M2.5 13.5L7 9" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
          </svg>
        </button>
      {/if}
      <button
        class="ctl"
        title="close view ({keyHint("closeView")})"
        aria-label="close view"
        onclick={() => ctrl.closeView(node.id)}
      >
        <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
          <path d="M4 4l8 8M12 4l-8 8" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" />
        </svg>
      </button>
    </div>

    {#if linkMenuOpen}
      <div class="overlay-surface link-menu" role="menu" aria-label="link to agent">
        <div class="link-menu-title">link to agent</div>
        {#each agentChoices as a (a.id)}
          <button class="overlay-row link-menu-item" role="menuitem" onclick={() => chooseAgent(a.id)}>
            <span class="chip-dot menu-dot" style:--hue={agentHue(a.id)}></span>
            <span class="link-menu-name">{sessionLabel(a.id)}</span>
            {#if linkedAgentId === a.id}
              <span class="link-menu-state">linked · click to unlink</span>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .bar {
    flex: none;
    display: flex;
    align-items: stretch;
    height: 26px;
    overflow: hidden;
    border-bottom: 1px solid var(--edge);
    padding: 0 4px;
    user-select: none;
  }

  .tabs {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: stretch;
    overflow: hidden;
  }

  /* Pane grip: same hover-fade recipe as the right-edge .controls, so the bar
     height never shifts (opacity, not display). cursor: grab reads as a
     draggable handle. */
  .pane-grip {
    flex: none;
    align-self: center;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 18px;
    margin-right: 2px;
    padding: 0;
    border: none;
    background: none;
    border-radius: 4px;
    color: var(--muted);
    cursor: grab;
    opacity: 0;
    pointer-events: none;
    transition:
      opacity 0.12s ease,
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .bar:hover .pane-grip,
  .pane-grip:focus-visible {
    opacity: 1;
    pointer-events: auto;
  }

  .pane-grip:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  .pane-grip:active {
    cursor: grabbing;
  }

  .tab {
    position: relative;
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 0 0.4rem 0 0.55rem;
    max-width: 200px;
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: default;
    user-select: none;
    transition: color 0.12s ease;
  }

  .tab:hover {
    color: var(--fg);
  }

  /* Active-tab emphasis via weight, not color — the bar stays quiet. */
  .tab.active {
    color: var(--fg);
    font-weight: 600;
  }

  /* A link-intent drag hovers this agent's tab: light it in the agent's hue
     (the tab is a link target even when it isn't the active view). */
  .tab.link-target {
    color: var(--fg);
    background: hsl(var(--hue) 55% 55% / 0.18);
    box-shadow: inset 0 0 0 1px hsl(var(--hue) 55% 55% / 0.6);
    border-radius: 5px;
  }

  /* Drag insertion caret. */
  .tab.insert::before,
  .tab-tail.insert::before {
    content: "";
    position: absolute;
    top: 5px;
    bottom: 5px;
    left: -1px;
    width: 2px;
    border-radius: 1px;
    background: var(--accent);
  }

  .tab-tail {
    position: relative;
    flex: 1;
    min-width: 8px;
  }

  /* The terminal type glyph moved into SessionGlyph (shared with quick-open
     and the launcher); its state palette lives there too. */

  /* File type glyph slot; quiet by default, lifts to full strength on the
     active tab (parity with the weight-based active emphasis). */
  .tab-glyph {
    flex: none;
    display: flex;
    align-items: center;
    opacity: 0.85;
  }

  .tab-glyph.on {
    opacity: 1;
  }

  /* Unsaved-edits marker: sits in the glyph slot, same footprint as a glyph. */
  .dirty-dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
  }

  .tab-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* A VS Code preview tab: italic until it is pinned (dbl-click / edit). */
  .tab-name.preview {
    font-style: italic;
  }

  /* Inline tab rename, sized like the name it replaces. */
  .tab-rename-input {
    min-width: 0;
    width: 12ch;
    font: inherit;
    color: var(--fg);
    background: var(--bg);
    border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--edge));
    border-radius: 3px;
    padding: 0 4px;
    outline: none;
  }

  .tab-rename-input.invalid {
    border-color: var(--err);
  }

  .tab-close {
    appearance: none;
    border: none;
    background: none;
    padding: 0 0.15rem;
    font: inherit;
    font-size: var(--text-md);
    font-weight: 400;
    line-height: 1;
    color: var(--muted);
    cursor: pointer;
    opacity: 0;
    flex: none;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }

  .tab:hover .tab-close,
  .tab.active .tab-close {
    opacity: 0.7;
  }

  .tab-close:hover {
    opacity: 1;
    color: var(--fg);
  }

  /* --- linked-terminal chips ------------------------------------------- */

  .links {
    flex: none;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 4px;
    min-width: 0;
    overflow: hidden;
  }

  /* One chip = the visible bond: agent hue, quiet until something happens. */
  .chip {
    display: flex;
    align-items: center;
    height: 18px;
    border: 1px solid hsl(var(--hue) 45% 55% / 0.45);
    border-radius: 9px;
    background: hsl(var(--hue) 50% 55% / 0.08);
    color: var(--fg);
    font-family: var(--mono);
    font-size: var(--text-xs);
    max-width: 150px;
    min-width: 0;
  }

  .chip-main {
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    gap: 5px;
    height: 100%;
    padding: 0 2px 0 7px;
    font: inherit;
    color: inherit;
    cursor: pointer;
    min-width: 0;
  }

  .chip-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .chip-spark {
    flex: none;
    color: hsl(var(--hue) 55% 55%);
  }

  .chip-dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }

  /* Chip dot states: what the linked shell is doing right now. */
  .chip-dot.ready {
    background: hsl(var(--hue) 40% 55% / 0.55);
  }

  .chip-dot.busy {
    background: var(--accent);
  }

  .chip-dot.quiet {
    background: none;
    border: 1px solid var(--muted);
    opacity: 0.6;
  }

  /* Agent exec queued: hollow agent-hue ring, waiting its turn. */
  .chip-dot.queued {
    background: none;
    border: 1.5px solid hsl(var(--hue) 60% 55%);
  }

  /* Agent exec running: the leash is being pulled — a gentle hue pulse. */
  .chip-dot.exec {
    background: hsl(var(--hue) 65% 55%);
    animation: chip-pulse 1.4s ease-in-out infinite;
  }

  @keyframes chip-pulse {
    0%,
    100% {
      box-shadow: 0 0 0 0 hsl(var(--hue) 65% 55% / 0.55);
    }
    50% {
      box-shadow: 0 0 0 3.5px hsl(var(--hue) 65% 55% / 0);
    }
  }

  .chip-x {
    appearance: none;
    border: none;
    background: none;
    padding: 0 6px 0 2px;
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

  .chip:hover .chip-x {
    opacity: 0.7;
  }

  .chip-x:hover {
    opacity: 1;
    color: var(--fg);
  }

  /* --- link-to-agent menu ------------------------------------------------
     .overlay-surface / .overlay-row live in app.css; these add only the bar
     anchor (positioned in .bar-right) and the dot-row layout. */

  .link-menu {
    top: 25px;
    right: 4px;
    z-index: 20;
    min-width: 180px;
  }

  .link-menu-title {
    padding: 3px 8px 5px;
    font-size: var(--text-xs);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
  }

  .link-menu-item {
    display: flex;
    align-items: center;
    gap: 7px;
  }

  .menu-dot {
    background: hsl(var(--hue) 55% 55%);
  }

  .link-menu-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .link-menu-state {
    margin-left: auto;
    padding-left: 10px;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  @media (prefers-reduced-motion: reduce) {
    .chip-dot.exec {
      animation: none;
    }
  }

  /* --- controls at the bar's right edge --- */

  .bar-right {
    position: relative;
    flex: none;
    display: flex;
    align-items: center;
    gap: 4px;
    padding-left: 4px;
  }

  .controls {
    display: flex;
    align-items: center;
    gap: 1px;
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.12s ease;
  }

  .bar:hover .controls,
  .controls:focus-within {
    opacity: 1;
    pointer-events: auto;
  }

  .ctl {
    appearance: none;
    border: none;
    background: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 18px;
    padding: 0;
    border-radius: 4px;
    color: var(--muted);
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .ctl:hover {
    background: var(--row-hover);
    color: var(--fg);
  }

  /* The link control stays lit while its menu is open (its mouse home). */
  .ctl.on {
    background: var(--row-hover);
    color: var(--fg);
  }

  /* A view switch is in flight: the toggle stands down quietly (no hover
     affordance) until the POST resolves, so it can't fire a second switch. */
  .ctl:disabled {
    cursor: default;
  }

  .ctl.pending {
    opacity: 0.5;
  }

  .ctl.pending:hover {
    background: none;
    color: var(--muted);
  }

  /* Context bridge: quiet selection-driven action in the bar (no hover
     gating — it appears exactly while the terminal holds a selection). */
  .ref-btn {
    appearance: none;
    border: none;
    display: flex;
    align-items: center;
    gap: 4px;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 7%, transparent);
    padding: 0 7px;
    height: 18px;
    border-radius: 4px;
    cursor: pointer;
    white-space: nowrap;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .ref-target {
    color: var(--muted);
    max-width: 130px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .ref-btn:hover:enabled .ref-target {
    color: inherit;
  }

  .ref-btn:hover:enabled {
    background: color-mix(in srgb, var(--fg) 12%, transparent);
    color: var(--fg);
  }

  .ref-btn:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }

  .ref-btn:disabled {
    opacity: 0.55;
    cursor: default;
  }

  .ref-at {
    color: var(--accent);
    font-weight: 600;
  }

  .ref-btn:disabled .ref-at {
    color: var(--muted);
  }

  /* --- "N files changed" chip: opens the session's diff review --- */

  .touched-chip {
    appearance: none;
    border: none;
    display: flex;
    align-items: center;
    gap: 4px;
    font: inherit;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 7%, transparent);
    padding: 0 7px;
    height: 18px;
    border-radius: 4px;
    cursor: pointer;
    white-space: nowrap;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .touched-edit {
    flex: none;
    opacity: 0.8;
  }

  .touched-chip:hover {
    background: color-mix(in srgb, var(--fg) 12%, transparent);
    color: var(--fg);
  }

  /* Per-pane text size (parity with the chords); text glyphs, same cluster. */
  .ctl-font {
    font-size: 10px;
    font-family: var(--mono);
    letter-spacing: -0.02em;
    width: 24px;
  }

  /* Persistent while zoomed — the always-visible mouse exit from zoom.
     Collapse glyph + "restore" label: an action, not the "ZOOM" state. */
  .zoom-badge {
    appearance: none;
    border: none;
    display: flex;
    align-items: center;
    gap: 4px;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 7%, transparent);
    padding: 0 7px;
    height: 18px;
    border-radius: 4px;
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .zoom-badge:hover {
    background: color-mix(in srgb, var(--fg) 12%, transparent);
    color: var(--fg);
  }
</style>

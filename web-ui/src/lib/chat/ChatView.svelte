<script lang="ts">
  import { customModelSelection, modelChoice } from "./modelPicker";
  import { onDestroy, tick, untrack } from "svelte";
  import { displayName, forkSession, rewindSession, renameSession, type Session } from "../workspace/sessions";
  import { fsValidate } from "../previews/files";
  import { openPath, type OpenPathOptions, type PathKind } from "../shared/openPath";
  import type { LinkContext } from "../shared/fileRef";
  import {
    chatLinkContext,
    groupByBases,
    PathResolver,
    resolveAndOpen,
    resolveScope,
    type ValidateAnswer,
  } from "./paths";
  import { listAgents } from "../workspace/launcher";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import BranchChip from "../shared/BranchChip.svelte";
  import { openBranchChanges } from "../workspace/git";
  import { insertIntoComposer, registerFollow } from "./composerBus";
  import {
    acquireChat,
    releaseChat,
    saveChatScroll,
    chatScroll,
    chatTurnStart,
    chatRenderWindow,
    saveChatRenderWindow,
    chatFollowedVersion,
    saveChatFollowedVersion,
  } from "./chatPool";
  import { dismiss } from "../shared/dismiss";
  import { formatElapsedSeconds, messageTimestampRefreshIn } from "../shared/time";
  import ChatHeader from "./ChatHeader.svelte";
  import ChatFind from "./ChatFind.svelte";
  import { legacyCapabilities } from "./capabilities";
  import Markdown from "./Markdown.svelte";
  import UserText from "./UserText.svelte";
  import ThoughtRow from "./ThoughtRow.svelte";
  import ToolGroup from "./ToolGroup.svelte";
  import type { TurnTail } from "./toolLabels";
  import FinishedRow from "./FinishedRow.svelte";
  import AgentMessageCards from "./AgentMessageCards.svelte";
  import { isAgentOrigin, parseAgentText } from "./agentMessages";
  import ActivityFold from "./ActivityFold.svelte";
  import { FINISHED_FOLD_MIN, foldSpans } from "./activityFold";
  import FinishedFold from "./FinishedFold.svelte";
  import { backgroundKind } from "./backgroundKinds";
  import AgentsTray from "./AgentsTray.svelte";
  import BackgroundTray from "./BackgroundTray.svelte";
  import WorkTray from "../shared/WorkTray.svelte";
  import Chevron from "../shared/Chevron.svelte";
  import ArtifactGallery from "./ArtifactGallery.svelte";
  import { EmbedResolver } from "./embeds";
  import { HoverTargets } from "./hoverTargets";
  import { resolveTargets } from "../shared/embed/embed";
  import { HoverPreviews } from "../previews/doc/hoverController.svelte";
  import PermissionCard from "./PermissionCard.svelte";
  import ElicitationCard from "./ElicitationCard.svelte";
  import type { PendingElicitation } from "./elicitation";
  import PlanApprovalCard from "./PlanApprovalCard.svelte";
  import QuestionCard from "./QuestionCard.svelte";
  import UsagePanel from "./UsagePanel.svelte";
  import McpPanel from "./McpPanel.svelte";
  import ConnectionDialog from "../plugins/ConnectionDialog.svelte";
  let authServer = $state<string | null>(null);
  import RewindDialog from "./RewindDialog.svelte";
  import AttachmentStrip from "./AttachmentStrip.svelte";
  import ForkDialog from "./ForkDialog.svelte";
  import AgentMessageMeta from "./AgentMessageMeta.svelte";
  import Composer from "./Composer.svelte";
  import { hookNotice } from "./hookNotice";
  import HookRow from "./HookRow.svelte";
  import SameFileNotice from "../workspace/SameFileNotice.svelte";
  import ModsWorkbench from "./ModsWorkbench.svelte";
  import ModSite from "./ModSite.svelte";
  import { modsFor } from "./mods.svelte";
  import type { NativeComposer } from "./nativeComposer";
  import { pageVisible } from "../shared/visibility";
  import { sameFile } from "../workspace/sameFile.svelte";
  import ReferenceChip from "../shared/ReferenceChip.svelte";
  import { activeSelection, clearSelection, setSelection } from "../shared/reference";
  import { quotableRange, quoteChipPosition } from "./quoteSelection";
  import { get } from "svelte/store";
  import { skillBlocksForText, type ComposerCommand } from "./composer";
  import type { ImageAttachment } from "./images";
  import type {
    BackgroundTask,
    ChatBlock,
    PendingPermission,
    PendingQuestion,
    PendingSend,
    PlanEntry,
  } from "./store.svelte";
  import {
    advanceTailWindow,
    autoPageEarlier,
    keepInReach,
    pageAround,
    pageEarlier,
    pageLater,
    PREFETCH_VIEWPORTS,
    prefetchPage,
    restoreVirtualWindow,
    restoreWindow,
    spacerNeedsRebalance,
    spacerTarget,
    tailWindow,
    trimShift,
    type PagePlan,
  } from "./transcriptWindow";
  import { measureShift, rowsInReach, selectAnchor, type ReadingAnchor } from "./readingAnchor";
  import { HistoryWeights, tailWeights, weightAt } from "./heightModel";
  import { activeTheme, getSetting, setSetting } from "../settings/store.svelte";
  import { hostCanDictate, voiceProblem } from "./voice.svelte";
  import { keyHint } from "../shared/keybindings";

  interface Props {
    session: Session;
    focused: boolean;
    /** Whether this retained view is the pane's visible tab. Distinct from
     *  focus: an unfocused split is still visible and should keep animating. */
    visible?: boolean;
    /** Workspace terminals for @term: mention grants. */
    terminals?: { id: string; name: string }[];
    /** Open a file path in an adjacent pane (the workbench path-click flow). */
    onOpenFile?: (path: string) => void;
    /** Kind-aware open: files → viewer pane, dirs → the Finder. A fallback:
     *  path links open through the workbench opener (`shared/openPath.ts`)
     *  whenever App has registered one. */
    onOpenPath?: (path: string, kind: "file" | "dir") => void;
    /** Flip this session to its real TUI (the pane-bar view toggle). Used for
     *  interactive CLI flows the `-p` stream-json mode can't run — `/login`
     *  above all — so the native auth flow runs where it belongs. */
    onSwitchToTerminal?: () => void;
    /** Focus the newly-created session while the source remains alive. */
    onForked?: (session: Session) => void;
  }

  let {
    session,
    focused,
    visible = true,
    terminals = [],
    onOpenFile,
    onOpenPath,
    onSwitchToTerminal,
    onForked,
  }: Props = $props();

  // The component is keyed on session id by its parent: one instance per
  // retained pane layer. Ordinary tab switches keep that tree (and its bounded
  // transcript window) mounted; if the pane live set evicts it or a pane
  // move remounts it, the session-keyed pool still reuses the warm store and
  // open socket instead of re-fetching the journal. Release keeps them warm;
  // the pool disposes them when the session ends or toggles to a PTY.
  // svelte-ignore state_referenced_locally
  const { store, socket } = acquireChat(session.id);
  const mods = modsFor(socket.nativeUi);
  let composerApi = $state<NativeComposer>();
  let modDockWidth = $state(0);
  // svelte-ignore state_referenced_locally
  onDestroy(() => releaseChat(session.id));
  onDestroy(() => {
    if (followFrame !== null) cancelAnimationFrame(followFrame);
    if (prefetchFrame !== null) cancelAnimationFrame(prefetchFrame);
    if (idleTimer !== null) clearTimeout(idleTimer);
    if (idleFrame !== null) cancelAnimationFrame(idleFrame);
  });
  onDestroy(() => prosePaths.dispose());
  onDestroy(() => proseEmbeds.dispose());

  // Curated model choices for this agent's picker (daemon-cached catalog).
  let models = $state<{ id: string; label: string }[]>([]);
  // svelte-ignore state_referenced_locally
  const agentKind = session.agent_kind ?? session.name ?? "agent";
  const capabilities = $derived(store.capabilities ?? legacyCapabilities(agentKind));
  const supports = (command: string) => capabilities.commands.includes(command);
  /** Product name for the identity chip. Prefer the daemon catalog's own name;
   *  fall back to a built-in map until it resolves (a workspace can mix agents,
   *  so the surface always says WHICH one this is). */
  let agentCatalogName = $state<string | null>(null);
  let forkAgents = $state<{ id: string; name: string }[]>([]);
  const agentName = $derived(
    agentCatalogName ??
      (agentKind === "claude" ? "Claude Code" : agentKind === "codex" ? "Codex" : agentKind === "agy" ? "Antigravity" : agentKind === "grok" ? "Grok Build" : agentKind),
  );
  void listAgents().then((agents) => {
    const info = agents.find((a) => a.id === agentKind);
    models = info?.models ?? [];
    agentCatalogName = info?.name ?? null;
    forkAgents = agents
      .filter((agent) => agent.installed && !agent.outdated && agent.chatCapable && agent.forkCapable)
      .map((agent) => ({ id: agent.id, name: agent.name }));
  });
  const availableForkAgents = $derived(
    forkAgents.length > 0 ? forkAgents : [{ id: agentKind, name: agentName }],
  );

  let transcriptEl = $state<HTMLElement | null>(null);
  let columnEl = $state<HTMLElement | null>(null);
  let spacerEl = $state<HTMLElement | null>(null);
  let laterSpacerEl = $state<HTMLElement | null>(null);
  let historySentinelEl = $state<HTMLElement | null>(null);
  let laterSentinelEl = $state<HTMLElement | null>(null);
  const canAutoLoadHistory = typeof IntersectionObserver !== "undefined";
  // Seed scroll intent from the pool so a remount restores the reading
  // position instead of snapping to the bottom.
  // svelte-ignore state_referenced_locally
  let atBottom = $state(chatScroll(session.id).atBottom);
  let menu = $state<"model" | "mode" | "effort" | "mcp" | "remote" | "options" | null>(null);

  // --- bounded transcript DOM ------------------------------------------------
  // The reducer/socket always fold the complete bounded journal so background
  // work and dashboard truth remain live. The expensive DOM is a separate,
  // bottom-anchored window: historical Markdown/artifacts mount only when the
  // reader asks for older context. A hidden retained chat freezes this plain
  // snapshot, so incoming work updates the store without re-rendering a tab no
  // one can see; activation reconciles one bounded page in a single paint.
  // svelte-ignore state_referenced_locally
  const savedRenderWindow = chatRenderWindow(session.id);
  /** Raw array shell: visible rows are the reducer's reactive block proxies,
   *  so a streamed text delta updates only that Markdown row. Hidden/paged
   *  views swap this once for plain data. Deep-cloning the whole 192-row page
   *  per token is exactly the multi-chat scaling failure this boundary avoids. */
  let renderBlocks = $state.raw<ChatBlock[]>([]);
  let renderStart = $state(0);
  let renderEnd = $state(0);
  let renderReady = $state(false);
  let renderedVersion = $state(-1);
  // Plain (non-reactive) bookkeeping, like rendersLive: only ever read
  // inside the windowing effect's untracked body or handlers.
  /** store.structuralVersion at the last range write. "Did the row set
   *  change" keys on this, never on lengths: at cap append+trim nets the
   *  length out, and a retracted-then-reappended tail nets even the virtual
   *  total out, while the rows are new either way. */
  let renderedStructural = -1;
  /** store.trimmedCount at the last range write. A trim front-splices the
   *  array under this view's absolute range; the delta says how far to shift
   *  renderStart/renderEnd so they keep naming the same rows. */
  let renderedTrim = 0;
  /** store.epoch at the last range write. A journal reset restarts the trim
   *  numbering — ranges are discarded across generations, never shifted. */
  let renderedEpoch = store.epoch;
  /** False while a setRangeAnchored scroll correction is scheduled but not
   *  applied. A hide in the same frame cancels the tick (anchorRevision
   *  bump); activation must then reconcile instead of early-outing on top of
   *  an uncorrected scroll position. */
  let anchorSettled = true;
  // svelte-ignore state_referenced_locally
  let followedVersion = $state(chatFollowedVersion(session.id) ?? -1);
  /** A non-empty draft pauses bottom-following, never transcript rendering. */
  let composerEngaged = $state(false);
  /** An explicit history page is stable. Ordinary scrolling inside a tail page
   *  keeps streaming until retaining the reader would exceed the DOM cap.
   *  Reactive for the template only (the windowing effect reads it
   *  untracked): a row appended to a tail window lags renderEnd by one flush,
   *  and reading that lag as "newer rows omitted" tore the live chrome out
   *  and back in — a layout forced in between shrank the transcript under a
   *  pinned reader, and WebKit's scrollTop clamp there read as scrolling up. */
  let tracksTail = $state(false);
  const atLiveEdge = $derived(tracksTail || renderEnd >= store.blocks.length);
  let rendersLive = false;
  let wasVisible = false;
  let pagingTranscript = false;
  const hasDeferredActivity = $derived(
    followedVersion !== store.transcriptVersion || !atLiveEdge,
  );

  function markFollowed(version = store.transcriptVersion): void {
    followedVersion = version;
    saveChatFollowedVersion(session.id, version);
  }

  let anchorRevision = 0;

  /** Persist the current window into the pool in trim-stable virtual
   *  coordinates, stamped with the transcript generation — a trim or journal
   *  reset while the view is unmounted then can't leave the cursor naming
   *  the wrong rows (stale ones are discarded at restore). */
  function saveWindowVirtual(start: number, end: number, tail: boolean): void {
    const trimmed = store.trimmedCount;
    saveChatRenderWindow(session.id, start + trimmed, end + trimmed, tail, store.epoch);
  }

  function setRange(
    start: number,
    end: number,
    options: { live: boolean; tail: boolean },
  ): void {
    const total = store.blocks.length;
    const safeEnd = Math.max(0, Math.min(end, total));
    const safeStart = Math.max(0, Math.min(start, safeEnd));
    renderStart = safeStart;
    renderEnd = safeEnd;
    if (safeEnd >= total) setLater(0, true);
    const source = store.blocks.slice(safeStart, safeEnd);
    renderBlocks = options.live ? source : $state.snapshot(source);
    rendersLive = options.live;
    tracksTail = options.tail;
    renderedVersion = store.transcriptVersion;
    renderedStructural = store.structuralVersion;
    renderedTrim = store.trimmedCount;
    renderedEpoch = store.epoch;
    renderReady = true;
    anchorRevision += 1;
    // A fresh range write supersedes any still-unapplied anchor correction.
    anchorSettled = true;
    saveWindowVirtual(safeStart, safeEnd, tracksTail);
  }

  function setTail(live = true): void {
    const range = tailWindow(store.blocks.length);
    setRange(range.start, range.end, { live, tail: true });
  }

  /** Break every reactive block link exactly once when a retained tab hides.
   *  Its DOM and local controls survive, while background events do no hidden
   *  Markdown parsing, grouping, or transcript layout. */
  function freezeRenderedRange(): void {
    if (!renderReady || !rendersLive) return;
    renderBlocks = $state.snapshot(renderBlocks);
    rendersLive = false;
    // The snapshot captured every in-place mutation the live proxies had
    // delivered, but no structural change beyond the range — advance the
    // version stamp only when none occurred, so the activation early-out can
    // never skip rows this freeze did not capture. renderedTrim likewise stays
    // at its last range write (the range was not shifted here); activation
    // shifts by the missed delta.
    if (store.structuralVersion === renderedStructural) {
      renderedVersion = store.transcriptVersion;
    }
    anchorRevision += 1;
  }

  /** The anchored row's CURRENT absolute index, resolved by uid identity
   *  against the rendered slice (immune to stale DOM labels after a trim
   *  shift); the parsed label is the fallback for a row already dropped. */
  function anchorArrayIndex(anchor: ReadingAnchor): number | null {
    if (anchor.uid !== null) {
      const offset = renderBlocks.findIndex((b) => b.uid === anchor.uid);
      if (offset !== -1) return renderStart + offset;
    }
    return anchor.index;
  }

  function canDiscardBefore(start: number): boolean {
    if (start <= renderStart) return true;
    const anchor =
      transcriptEl === null || columnEl === null ? null : selectAnchor(transcriptEl, columnEl);
    if (anchor === null) return false;
    const index = anchorArrayIndex(anchor);
    return index !== null && index >= start;
  }

  // --- reading position -----------------------------------------------------
  // A reader who scrolled away from the tail keeps the text under them fixed
  // through every layout change above it — a page mounted or trimmed, a
  // preview decoding, a fold regrouping. The shift is measured on the row at
  // the viewport's top edge (readingAnchor.ts) and absorbed by the history
  // spacer ahead of the column, NOT by rewriting scrollTop: WebKit has no
  // native scroll anchoring, and its scrolling thread snaps a mid-fling
  // scrollTop write back for a frame or two (why transcriptWindow.ts owns the
  // spacer policy). The spacer is only resized — with one compensating write —
  // at scroll idle or where the follow writer already owns the position.
  /** Spacer height in px, written straight to the element: absorption runs
   *  inside scroll/resize callbacks and must land before this frame paints.
   *  Negative when an estimate fell short (rendered as a negative margin). */
  let spacerPx = 0;
  /** The row the reader is on while not following the tail. */
  let readingAnchor: ReadingAnchor | null = null;
  /** Direction of travel, so prefetch only pages the way the reader goes. */
  let lastScrollTop = 0;
  let scrollDirection: -1 | 1 = -1;
  let idleTimer: ReturnType<typeof setTimeout> | null = null;
  let idleFrame: number | null = null;
  let prefetchFrame: number | null = null;
  /** No scroll event for this long means no gesture or momentum is in flight. */
  const SCROLL_IDLE_MS = 160;

  function setSpacer(px: number): void {
    spacerPx = Math.round(px);
    if (spacerEl === null) return;
    spacerEl.style.height = `${Math.max(0, spacerPx)}px`;
    spacerEl.style.marginBottom = `${Math.min(0, spacerPx)}px`;
  }

  /** The history spacer's twin below the window: room for the unmounted
   *  LATER rows of a reader paging through history. Without it the scroll
   *  height swung by a page at every write (the scrollbar thumb resized and
   *  jumped against the reader's direction) and the track's bottom meant
   *  "the rendered end", not the conversation's. Everything it changes is
   *  below the reader, so it is resized freely — never below the viewport's
   *  bottom, where shrinking would clamp scrollTop. Zero at the live edge,
   *  unconditionally (`force`): room past the newest row would break
   *  following, and a reader looking into it belongs at the end anyway. */
  let laterPx = 0;

  function setLater(px: number, force = false): void {
    const el = transcriptEl;
    let next = Math.max(0, Math.round(px));
    if (next === laterPx) return;
    if (el !== null && next < laterPx && !force) {
      const room = el.scrollHeight - el.scrollTop - el.clientHeight;
      next = Math.max(next, laterPx - Math.max(0, room));
    }
    laterPx = next;
    if (laterSpacerEl !== null) laterSpacerEl.style.height = `${laterPx}px`;
  }

  /** After a range write, give the later spacer whatever the scroll height
   *  changed by since `before`: the reader's rows are held by the history
   *  spacer, so the change is below them, and the thumb then moves only when
   *  the reader scrolls. A window that reaches the live edge has nothing
   *  unmounted below it. */
  function settleLater(before: number): void {
    const el = transcriptEl;
    if (el === null) return;
    if (renderEnd >= store.blocks.length) setLater(0, true);
    // Never below the model of what is still unmounted: a running total
    // that undershot (figures weigh more than their estimate) ran out with
    // pages to go, the reader met the scroll range's end early, and the last
    // page then moved the end — and the thumb — thousands of px away.
    else setLater(Math.max(laterPx - (el.scrollHeight - before), laterTarget()));
  }

  /** The later spacer's modelled size: the unmounted rows after the window
   *  at the mounted window's px per unit. */
  function laterTarget(): number {
    if (renderEnd >= store.blocks.length) return 0;
    return spacerTarget(tailWeights(store.blocks, renderEnd, charsPerLine()).total, historyPxPerWeight());
  }

  /** Persist the reading position relative to the rendered rows — the spacer
   *  is re-derived on remount, so an absolute scrollTop would drift. */
  function saveReadingPosition(bottom: boolean): void {
    const el = transcriptEl;
    if (el === null) return;
    saveChatScroll(session.id, Math.max(0, el.scrollTop - spacerPx), bottom);
  }

  function pinReadingAnchor(): void {
    readingAnchor =
      transcriptEl === null || columnEl === null ? null : selectAnchor(transcriptEl, columnEl);
  }

  /** Give back any shift of the anchored row through the spacer — never a
   *  scroll write, which a gesture in flight would snap back. */
  function holdReadingAnchor(): number {
    const el = transcriptEl;
    const column = columnEl;
    if (el === null || column === null || readingAnchor === null) return 0;
    const measured = measureShift(readingAnchor, column);
    if (measured === null) {
      pinReadingAnchor();
      return 0;
    }
    readingAnchor = measured.anchor;
    if (Math.abs(measured.shift) < 1) return 0;
    setSpacer(spacerPx - measured.shift);
    return measured.shift;
  }

  /** Modelled weight of the unmounted history (heightModel.ts), and the px
   *  one unit of it renders at, calibrated on the mounted window. */
  const historyWeights = new HistoryWeights();

  function charsPerLine(): number {
    const width = columnEl?.clientWidth ?? 0;
    return width > 0 ? width / (chatFontSize * 0.55) : 80;
  }

  function historyGeneration(): string {
    return `${store.epoch}|${store.trimmedCount}`;
  }

  /** Rendered px per model unit across the mounted window, clamped so one
   *  odd window (an expanded tool card, a gallery) cannot skew it wildly. */
  function windowPxPerWeight(): number {
    const column = columnEl;
    const nominal = chatFontSize * chatLineHeight;
    if (column === null) return nominal;
    const children = column.children;
    let first: HTMLElement | null = null;
    let last: HTMLElement | null = null;
    for (let i = 0; i < children.length && first === null; i++) {
      if (children[i].hasAttribute("data-block-uid")) first = children[i] as HTMLElement;
    }
    for (let i = children.length - 1; i >= 0 && last === null; i--) {
      if (children[i].hasAttribute("data-block-uid")) last = children[i] as HTMLElement;
    }
    const cpl = charsPerLine();
    let weight = 0;
    // Bounded by the live array: a reset or tail splice can shrink it before
    // the windowing effect repairs renderEnd.
    const end = Math.min(renderEnd, store.blocks.length);
    // The window's trailing run is unfolded: the finished lines that end it,
    // else the activity after its last reply.
    let settledEnd = end;
    while (settledEnd > renderStart && store.blocks[settledEnd - 1].kind === "finished") settledEnd--;
    if (settledEnd === end) {
      while (settledEnd > renderStart) {
        const kind = store.blocks[settledEnd - 1].kind;
        if (kind === "message" || kind === "finished") break;
        settledEnd--;
      }
    }
    for (let i = renderStart; i < end; i++) {
      weight += weightAt(store.blocks, i, cpl, i < settledEnd);
    }
    if (first === null || last === null || weight <= 0) return nominal;
    const measured = (last.offsetTop + last.offsetHeight - first.offsetTop) / weight;
    return Math.min(nominal * 2.5, Math.max(nominal * 0.4, measured));
  }

  /** What the history pages mounted above the reader really measured (the
   *  anchor shift each one caused) against what the model weighed them at:
   *  a steadier scale for the rest of the history than the one window on
   *  screen, which the live tail skews (its turn is unfolded, its cards
   *  large). Keyed like the weights, so a trim, reset or reflow restarts it. */
  let paged = { key: "", px: 0, weight: 0 };

  function pagedKey(): string {
    return `${historyGeneration()}|${Math.round(charsPerLine())}`;
  }

  function notePagedHeight(weight: number, px: number): void {
    const key = pagedKey();
    if (paged.key !== key) paged = { key, px: 0, weight: 0 };
    if (weight <= 0 || px <= 0) return;
    paged.px += px;
    paged.weight += weight;
  }

  /** Px per model unit for the unmounted history: the paged measurements
   *  once there are a few screens of them, else the mounted window. */
  function historyPxPerWeight(): number {
    const nominal = chatFontSize * chatLineHeight;
    if (paged.key !== pagedKey() || paged.weight < 60) return windowPxPerWeight();
    return Math.min(nominal * 2.5, Math.max(nominal * 0.4, paged.px / paged.weight));
  }

  /** The window start + history generation the spacer was last sized for;
   *  the bottom-follow writer re-sizes only when it moved (it runs per
   *  streamed frame, and sizing walks the window's text). */
  let spacerSizedFor = "";

  /** Resize the spacer to its target with one compensating scroll write.
   *  Only called where no gesture can be in flight (scroll idle) or where a
   *  scroll write happens anyway (the bottom-follow writer, a restore). */
  function rebalanceSpacer(onlyIfMoved = false): void {
    const el = transcriptEl;
    const column = columnEl;
    if (el === null || column === null) return;
    const sizedFor = `${renderStart}|${historyGeneration()}`;
    if (onlyIfMoved && sizedFor === spacerSizedFor) return;
    spacerSizedFor = sizedFor;
    // The follow writer runs at the live edge, where the later spacer is
    // already gone; only an idle or restore pass re-models it.
    if (!onlyIfMoved) {
      const later = laterTarget();
      if (spacerNeedsRebalance(laterPx, later)) setLater(later);
    }
    // A followed tail shorter than the viewport is still filling from the
    // sentinel; blank space above it would only push the rows down. A short
    // history page keeps its room, or the next page could not be absorbed.
    const target =
      atBottom && column.offsetHeight < el.clientHeight
        ? 0
        : spacerTarget(
            historyWeights.upTo(store.blocks, renderStart, charsPerLine(), historyGeneration()),
            historyPxPerWeight(),
          );
    if (!spacerNeedsRebalance(spacerPx, target)) return;
    // Read the offset BEFORE the spacer changes: shrinking it can leave the
    // old offset past the new maximum, the engine clamps it, and a relative
    // `+=` would then apply the shift on top of the clamp (a reader deep in
    // a short window was thrown thousands of px).
    const top = el.scrollTop;
    const delta = target - spacerPx;
    setSpacer(target);
    el.scrollTop = top + delta;
    lastScrollTop = el.scrollTop;
  }

  function scheduleScrollIdle(): void {
    if (idleTimer !== null) clearTimeout(idleTimer);
    if (idleFrame !== null) cancelAnimationFrame(idleFrame);
    idleFrame = null;
    idleTimer = setTimeout(() => {
      idleTimer = null;
      // After a long task this timer can run ahead of the scroll events
      // queued behind it, mid-fling: one rendering update delivers them
      // first, and any of them re-arms the wait instead.
      idleFrame = requestAnimationFrame(() => {
        idleFrame = null;
        if (!visible || pagingTranscript || store.hydrating) return;
        rebalanceSpacer();
        if (!atBottom) pinReadingAnchor();
        saveReadingPosition(atBottom);
      });
    }, SCROLL_IDLE_MS);
  }

  $effect(() => {
    if (visible) return;
    untrack(() => {
      if (idleTimer !== null) clearTimeout(idleTimer);
      if (idleFrame !== null) cancelAnimationFrame(idleFrame);
      idleTimer = null;
      idleFrame = null;
    });
  });

  // A re-hydrating transcript (a journal reset) shows only its loading line;
  // room held for the old history would push that line out of view. The
  // re-mounted tail sizes the spacer afresh.
  $effect(() => {
    if (!store.hydrating) return;
    untrack(() => {
      setSpacer(0);
      setLater(0, true);
      spacerSizedFor = "";
      readingAnchor = null;
    });
  });

  /** Mount the next page in the reader's direction of travel once it is
   *  within reach, so a fling never stops dead at the rendered edge. */
  function maybePrefetch(): void {
    const el = transcriptEl;
    const column = columnEl;
    if (el === null || column === null || atBottom || pagingTranscript) return;
    if (!visible || store.hydrating) return;
    const view = el.getBoundingClientRect();
    const rows = column.getBoundingClientRect();
    if (farJump(el, view, rows)) return;
    const next = prefetchPage(
      { above: view.top - rows.top, below: rows.bottom - view.bottom, viewport: el.clientHeight },
      { start: renderStart, end: renderEnd },
      store.blocks.length,
      scrollDirection,
    );
    if (next === "earlier") revealEarlier();
    else if (next === "later") revealLater();
  }

  /** The mounted rows a page write must keep: those within one viewport past
   *  the prefetch reach, so a direction change can't page straight back
   *  either (transcriptWindow.ts `keepInReach`). */
  function rowsNearReader(): { start: number; end: number } | null {
    const el = transcriptEl;
    const column = columnEl;
    if (el === null || column === null) return null;
    return rowsInReach(el, column, el.clientHeight * (PREFETCH_VIEWPORTS + 1));
  }

  /** The viewport landed deep inside the spacer (a scrollbar drag): mount the
   *  page the modelled history puts there instead of paging toward it. The
   *  reader sees only blank space, so moving the spacer to put that page
   *  under them moves nothing they could be reading. */
  function farJump(el: HTMLElement, view: DOMRect, rows: DOMRect): boolean {
    const reach = el.clientHeight * PREFETCH_VIEWPORTS;
    const total = store.blocks.length;
    let index: number;
    /** A drag to the very end of the track means the conversation's end —
     *  not the last page's first rows at the viewport's top, which left a
     *  figure-heavy tail screens short of the bottom. */
    let toEnd = false;
    if (renderStart > 0 && spacerPx > 0 && rows.top - view.bottom >= reach) {
      const earlier = historyWeights.upTo(store.blocks, renderStart, charsPerLine(), historyGeneration());
      // Map by the spacer's own proportion, not the model's px scale: it has
      // absorbed real page heights since it was sized, and its two ends must
      // still mean block 0 and the window's first block.
      const spacerTop = rows.top - spacerPx;
      const fraction = Math.min(1, Math.max(0, (view.top - spacerTop) / spacerPx));
      index = Math.min(renderStart - 1, historyWeights.indexAt(fraction * earlier));
    } else if (renderEnd < total && laterPx > 0 && view.top - rows.bottom >= reach) {
      // The same below: the later spacer's ends mean the window's end and
      // the live edge.
      const later = tailWeights(store.blocks, renderEnd, charsPerLine());
      const fraction = Math.min(1, Math.max(0, (view.top - rows.bottom) / laterPx));
      index = Math.min(total - 1, later.at(fraction * later.total));
      toEnd = el.scrollTop + el.clientHeight >= el.scrollHeight - 2;
    } else {
      return false;
    }
    const height = el.scrollHeight;
    pagingTranscript = true;
    if (toEnd) {
      setTail();
    } else {
      const page = pageAround(index, total);
      setRange(page.start, page.end, { live: false, tail: false });
    }
    void tick().then(() => {
      const column = columnEl;
      const current = transcriptEl;
      if (column !== null && current !== null && toEnd) {
        // The end on the viewport's bottom, by the space above it (no scroll
        // write under a dragged thumb); the reader is following again.
        settleLater(height);
        setSpacer(spacerPx + current.scrollTop + current.clientHeight - current.scrollHeight);
        atBottom = true;
        markFollowed();
      } else if (column !== null && current !== null) {
        const row = Array.from(column.children).find((child) => {
          const start = Number(child.getAttribute("data-block-index"));
          const end = Number(child.getAttribute("data-block-end") ?? start);
          return child.hasAttribute("data-block-index") && start <= index && end >= index;
        });
        if (row !== undefined) {
          const offset = row.getBoundingClientRect().top - current.getBoundingClientRect().top;
          setSpacer(spacerPx - offset);
        }
        settleLater(height);
      }
      pinReadingAnchor();
      afterPaging();
    });
    return true;
  }

  /** The latest setRangeAnchored height hold; only it releases the column. */
  let columnHeightHold = 0;

  /** Replace a range while keeping the row under the reader where it is. */
  function setRangeAnchored(
    start: number,
    end: number,
    options: { live: boolean; tail: boolean },
    onHeld?: (shift: number) => void,
  ): void {
    // Settle any shift still pending, then pin the row at the viewport.
    holdReadingAnchor();
    pinReadingAnchor();
    // Rows discarded above the reader shorten the content until the hold
    // grows the spacer back, and the hold's own measuring is a layout: WebKit
    // clamps scrollTop at any layout where the content is momentarily too
    // short, and mid-gesture that clamp fights its scrolling thread for a few
    // frames. The column keeps its height until the hold has run.
    const column = columnEl;
    const hold = ++columnHeightHold;
    if (column !== null) column.style.minHeight = `${column.offsetHeight}px`;
    const height = transcriptEl?.scrollHeight ?? 0;
    setRange(start, end, options);
    const revision = anchorRevision;
    anchorSettled = false;
    void tick().then(() => {
      // A cancelled hold (a freeze bumped the revision before this ran)
      // leaves anchorSettled false, so the next activation reconciles instead
      // of early-outing on an unabsorbed shift.
      const current = revision === anchorRevision;
      const shift = current ? holdReadingAnchor() : 0;
      if (column !== null && hold === columnHeightHold) column.style.minHeight = "";
      if (current) {
        onHeld?.(shift);
        settleLater(height);
        anchorSettled = true;
        saveReadingPosition(false);
      }
    });
  }

  // Hidden views freeze once. Visible tail rows point at the reducer's live
  // proxies, so in-place chunks need no range replacement; only structural
  // appends swap the small raw array. Activation catches up behind the saved
  // anchor but never discards it merely because a large hidden backlog arrived.
  $effect(() => {
    if (!visible) {
      untrack(() => {
        freezeRenderedRange();
        wasVisible = false;
      });
      return;
    }
    const activating = !wasVisible;
    if (store.hydrating) return;
    // Marked only once a reconcile actually runs: a thaw during rehydration
    // must not burn `activating` before the post-hydration pass can use it.
    wasVisible = true;
    const total = store.blocks.length;
    const version = store.transcriptVersion;
    const trimmed = store.trimmedCount;
    const following = atBottom;
    const drafting = composerEngaged;
    untrack(() => {
      const structural = store.structuralVersion;
      if (renderReady && store.epoch !== renderedEpoch) {
        // The journal was reset and the transcript rebuilt: this range (and
        // any trim delta against it) is dead-coordinate data from another
        // generation. Discard to the live tail — never shift across epochs.
        setTail();
        if (!atBottom) atBottom = true;
        queueBottomScroll(true);
        return;
      }
      // Cap trims splice the array's front out from under this absolute
      // range; shift with them — and drop the same rows from the rendered
      // slice (frozen snapshots included) — so range, rows, and index labels
      // keep agreeing. Same-epoch only, where trimmedCount is monotonic.
      const trimDelta = trimmed - renderedTrim;
      if (trimDelta > 0 && renderReady) {
        const shifted = trimShift({ start: renderStart, end: renderEnd }, trimDelta);
        if (shifted === null) {
          // Every rendered row was trimmed away under this view — the mounted
          // mirror of a stale saved cursor: fall back to the live tail.
          setTail();
          if (!atBottom) atBottom = true;
          queueBottomScroll(true);
          return;
        }
        renderStart = shifted.window.start;
        renderEnd = shifted.window.end;
        if (shifted.lost > 0) renderBlocks = renderBlocks.slice(shifted.lost);
      }
      renderedTrim = trimmed;
      if (!renderReady) {
        // Saved cursors are virtual and generation-stamped. One that predates
        // a journal reset, or whose rows were all trimmed away, is discarded
        // together with its scroll position — an arbitrary reading offset
        // must not land on the tail we fall back to.
        const saved =
          savedRenderWindow === null || savedRenderWindow.epoch !== store.epoch
            ? null
            : restoreVirtualWindow(savedRenderWindow, trimmed, total);
        if (!atBottom && savedRenderWindow !== null && saved !== null) {
          setRange(saved.start, saved.end, {
            live: savedRenderWindow.tail,
            tail: savedRenderWindow.tail,
          });
        } else {
          if (!atBottom && savedRenderWindow !== null) {
            saveChatScroll(session.id, 0, true);
            atBottom = true;
          }
          setTail();
        }
        if (followedVersion < 0 || followedVersion > version) markFollowed(version);
      } else if (renderStart >= total || renderEnd > total) {
        // A rewind/compaction can invalidate an absolute historical range.
        const repaired = restoreWindow({ start: renderStart, end: renderEnd }, total);
        setRangeAnchored(repaired.start, repaired.end, {
          live: tracksTail,
          tail: tracksTail,
        });
      } else if (activating) {
        if (!tracksTail) {
          if (!anchorSettled) {
            // The pending hold died with a freeze; the anchor it pinned is
            // still the pre-change one, so absorb the shift now.
            anchorSettled = true;
            holdReadingAnchor();
            saveReadingPosition(false);
          }
          return;
        }
        if (version === renderedVersion && structural === renderedStructural && anchorSettled) {
          // Nothing arrived while hidden: the frozen page is already exact, so
          // skip the slice/rebuild entirely. Rows stay inert snapshots until
          // the next event or a bottom-reach rebinds live proxies (both call
          // setRange), so a quiet tab switch costs no transcript work. Still
          // settle the unread stamp: an in-place chunk landing in the same
          // flush as the hide is IN the snapshot but was never marked
          // followed, and nothing after this would heal the phantom chip.
          if (following && !drafting) markFollowed(version);
          return;
        }
        const next = advanceTailWindow({ start: renderStart, end: renderEnd }, total);
        if (following && !drafting) {
          setRange(next.start, next.end, { live: true, tail: true });
          markFollowed(version);
        } else if (canDiscardBefore(next.start)) {
          setRangeAnchored(next.start, next.end, { live: true, tail: true });
        } else {
          // Rebind the saved page for normal in-place streaming, but do not
          // throw its visible row away merely to fit a large hidden backlog.
          setRangeAnchored(renderStart, Math.min(renderEnd, total), {
            live: true,
            tail: false,
          });
        }
      } else if (
        tracksTail &&
        (version !== renderedVersion || structural !== renderedStructural)
      ) {
        if (drafting && version !== renderedVersion && atBottom) atBottom = false;
        const next = advanceTailWindow({ start: renderStart, end: renderEnd }, total);
        if (structural === renderedStructural) {
          // In-place chunk growth: the row set is unchanged. The live proxy
          // already delivered it to its row — unless an early-out activation
          // left the rows frozen; rebind them now.
          if (!rendersLive) {
            setRange(renderStart, renderEnd, { live: true, tail: true });
          } else {
            renderedVersion = version;
          }
          if (following && !drafting) markFollowed(version);
        } else if (following && !drafting) {
          setRange(next.start, next.end, { live: true, tail: true });
          markFollowed(version);
        } else if (canDiscardBefore(next.start)) {
          setRangeAnchored(next.start, next.end, { live: true, tail: true });
        } else {
          // The bounded tail is full and the row to discard is still visible:
          // preserve the page and make the deferred gap explicit.
          freezeRenderedRange();
          tracksTail = false;
          saveWindowVirtual(renderStart, renderEnd, false);
        }
      }
    });
  });

  /** Model picker: the agent's own catalog (claude initialize.models /
   *  codex model/list) beats the daemon's curated list. */
  const modelChoices = $derived(store.modelCatalogReceived ? store.models : models);
  const allowCustomModel = $derived(supports("set_model") && capabilities.custom_model);
  /** The catalog row for the live model. Ids come in three spellings:
   *  picker values ("opus[1m]"), catalog resolvedModel
   *  ("claude-opus-4-8[1m]"), and the BARE api id assistant messages report
   *  ("claude-opus-4-8") — match all three, preferring named entries over
   *  "Default (recommended)" (both resolve to the same model). While the real
   *  model is not yet known (store.model === null, before init/ready resolves)
   *  this is undefined so the header shows a neutral loading chip — NOT a
   *  concrete "default" that would flash the wrong name (slow on remote). */
  const currentModel = $derived(modelChoice(store.models, store.model));
  /** Only the active model's reported metadata establishes effort support. */
  const effortChoices = $derived(currentModel?.efforts ?? []);
  /** Agent read-back is the only displayed truth. Both drivers emit an
   *  effort_state after applying a selection. */
  const effortShown = $derived(store.effort);
  const hasEffort = $derived(supports("set_effort") && effortChoices.length > 0);
  /** Ultracode: session-scoped xhigh + standing workflow orchestration —
   *  offered when the live model supports xhigh (the extension's gate). */
  const hasUltracode = $derived(
    supports("set_ultracode") && (currentModel?.efforts.includes("xhigh") ?? false),
  );

  // The chat scale is deliberately local: every child uses the shared
  // --text-* tokens, so overriding them on this root covers prose, composer,
  // tool cards, trays, dialogs, and the dashboard's embedded Mastermind chat
  // without making terminal/editor content follow an interface preference.
  const chatFontSize = $derived(getSetting("chat.fontSize"));
  const chatLineHeight = $derived(getSetting("chat.lineHeight"));
  const chatContentWidth = $derived(getSetting("chat.contentWidth"));
  const chatFontFamily = $derived(
    getSetting("chat.fontFamily").trim() || "var(--ui-font)",
  );

  /** When the reader last did something that scrolls. WebKit dispatches the
   *  wheel (momentum included), pointer (the scrollbar too), touch, or key
   *  input ahead of the scroll it causes; a scroll chained to one stays the
   *  reader's, so a track-click animation or fling tail outlives the input. */
  let scrollIntentAt = -Infinity;
  const SCROLL_INTENT_MS = 300;
  function noteScrollIntent(): void {
    scrollIntentAt = performance.now();
  }
  /** A decelerating fling keeps sending momentum wheel events after its
   *  per-frame move rounds to nothing, so scroll events stop while WebKit's
   *  scrolling thread still owns the position — an idle rebalance's scroll
   *  write then was snapped back, landing the reader thousands of px away in
   *  a spacer. Wheel input keeps a pending idle waiting until it stops. */
  function onWheel(): void {
    noteScrollIntent();
    if (idleTimer !== null || idleFrame !== null) scheduleScrollIdle();
    // Pushing against the top produces wheel input but no scroll event.
    if (!atBottom) holdTopEdge();
  }
  // Passive by hand: Svelte attaches `onwheel` non-passive, which would pull
  // WebKit's wheel scrolling off its scrolling thread.
  $effect(() => {
    const el = transcriptEl;
    if (el === null) return;
    el.addEventListener("wheel", onWheel, { passive: true });
    return () => el.removeEventListener("wheel", onWheel);
  });

  function onScroll() {
    const el = transcriptEl;
    if (el === null) return;
    // A parked view has no reader. Its scroller still moves — a turn ending
    // while hidden drops the status row, and WebKit clamps the offset — and
    // against a frozen range with rows waiting that read as leaving the live
    // edge: the chat reopened short of its newest row, no longer following.
    if (!visible) {
      lastScrollTop = el.scrollTop;
      return;
    }
    const top = el.scrollTop;
    const moved = top !== lastScrollTop;
    const up = top < lastScrollTop;
    if (moved) scrollDirection = up ? -1 : 1;
    lastScrollTop = top;
    const now = performance.now();
    const byReader = now - scrollIntentAt < SCROLL_INTENT_MS;
    // Only a real move carries the reader's gesture on: the follow writer's
    // own (non-moving) echoes must not keep a stale intent alive all stream.
    if (byReader && moved) scrollIntentAt = now;
    const nearEnd = el.scrollHeight - top - el.clientHeight < 40;
    // A pinned follower leaves the live edge only by its own hand. WebKit
    // dispatches the follow writer's scroll event a frame late, after rows
    // that landed in between already grew the transcript (a follower who
    // never moved), and it clamps scrollTop wherever a streamed re-render
    // momentarily shrinks the content under the reader (an up-move nobody
    // made). Idle, geometry alone decides, so a find or focus scroll holds.
    const held =
      atBottom && !nearEnd && (!moved || (up && !byReader && store.running));
    atBottom = renderEnd >= store.blocks.length && (nearEnd || held);
    if (held && atBottom) queueBottomScroll();
    if (atBottom) {
      // Reaching the actual live edge is an explicit resume signal even after
      // paging history: keep the current range, rebind its live proxies, and
      // let future rows append normally.
      readingAnchor = null;
      if (!tracksTail || !rendersLive) {
        setRange(renderStart, renderEnd, { live: true, tail: true });
      }
      markFollowed();
    } else {
      // A shift that landed between frames (a preview decoding above the
      // reader) is absorbed before the anchor moves to the new top row.
      holdReadingAnchor();
      pinReadingAnchor();
      holdTopEdge();
      maybePrefetch();
    }
    // Persist into the pool so the next remount restores this position.
    saveReadingPosition(atBottom);
    scheduleScrollIdle();
  }

  // Every source of bottom-following funnels through one coalesced writer.
  // Stream events, Markdown reveals, image sizing, and composer layout used to
  // schedule competing tick/scroll cycles, which visibly bounced the pane.
  let followAfterTick = false;
  let followFrame: number | null = null;
  let forceFollow = false;
  function queueBottomScroll(force = false): void {
    forceFollow ||= force;
    if (followAfterTick || followFrame !== null) return;
    followAfterTick = true;
    void tick().then(() => {
      followAfterTick = false;
      const forced = forceFollow;
      forceFollow = false;
      if (!visible || (!forced && (!atBottom || composerEngaged))) return;
      followFrame = requestAnimationFrame(() => {
        followFrame = null;
        const el = transcriptEl;
        if (el === null || (!forced && (!atBottom || composerEngaged))) return;
        // This writer owns the position anyway, so it keeps the spacer sized
        // for the reader's first scroll up into history.
        rebalanceSpacer(true);
        el.scrollTop = el.scrollHeight;
        lastScrollTop = el.scrollTop;
        readingAnchor = null;
        markFollowed();
        saveReadingPosition(true);
      });
    });
  }

  function scrollToBottom() {
    if (!store.hydrating) setTail();
    atBottom = true;
    queueBottomScroll(true);
  }

  function applyEarlierPage(plan: PagePlan, preserveTail: boolean) {
    const el = transcriptEl;
    if (el === null || pagingTranscript || renderStart === 0 || store.hydrating) return;
    pagingTranscript = true;
    if (preserveTail) {
      setRange(plan.settled.start, plan.settled.end, { live: true, tail: true });
      atBottom = true;
      queueBottomScroll();
    } else {
      atBottom = false;
      const settled = keepInReach(plan, rowsNearReader());
      // Prepending to a window that still ends at the live edge is scrolling
      // within the tail, not paging away from it: keep it live until the cap
      // drops the newest page (then it is an explicit history page).
      const keepsTail = tracksTail && settled.end >= store.blocks.length;
      const cpl = charsPerLine();
      const generation = historyGeneration();
      const weight =
        historyWeights.upTo(store.blocks, renderStart, cpl, generation) -
        historyWeights.upTo(store.blocks, settled.start, cpl, generation);
      setRangeAnchored(
        settled.start,
        settled.end,
        { live: keepsTail, tail: keepsTail },
        (shift) => notePagedHeight(weight, shift),
      );
    }
    void tick().then(afterPaging);
  }

  /** Once the window holds the first row, whatever the spacer still holds
   *  is the estimate's leftover: blank above the first message. It shrinks as
   *  fast as the reader scrolls into it, so the first message stops at the
   *  viewport's top like any document's start instead of sliding down into
   *  screens of nothing (only an idle rebalance used to remove it, and a
   *  reader who never paused dragged straight into it). Only the space above
   *  them changes — never the scroll offset — so this is safe mid-gesture. A
   *  negative spacer (rows pulled past the scroll origin) is given back once
   *  the reader is pushing against the top. */
  function holdTopEdge(): void {
    const el = transcriptEl;
    const column = columnEl;
    if (el === null || column === null || renderStart !== 0 || spacerPx === 0) return;
    const top = el.scrollTop;
    if (spacerPx < 0) {
      if (top > 0) return;
      setSpacer(0);
    } else {
      const columnTop = column.getBoundingClientRect().top - el.getBoundingClientRect().top + top;
      const blankFrom = columnTop - spacerPx;
      const keep = Math.max(0, top - blankFrom);
      if (keep >= spacerPx) return;
      setSpacer(keep);
    }
    pinReadingAnchor();
  }

  /** A page landed: keep going while the reader is still within reach of an
   *  edge (a fast fling, or a scrollbar drag into the spacer), and let the
   *  spacer settle once scrolling stops. */
  function afterPaging(): void {
    pagingTranscript = false;
    holdTopEdge();
    // One page per frame: a chain of microtask pages would render the whole
    // run in one blocking task.
    if (prefetchFrame === null) {
      prefetchFrame = requestAnimationFrame(() => {
        prefetchFrame = null;
        maybePrefetch();
      });
    }
    scheduleScrollIdle();
  }

  async function revealFindMessage(uid: number): Promise<HTMLElement | null> {
    const index = store.blocks.findIndex((block) => block.uid === uid);
    if (index < 0 || !visible || transcriptEl === null) return null;
    atBottom = false;
    pagingTranscript = true;
    const page = pageAround(index, store.blocks.length);
    const tail = page.end >= store.blocks.length;
    setRange(page.start, page.end, { live: tail, tail });
    await tick();
    if (!visible) { pagingTranscript = false; return null; }
    const row = columnEl?.querySelector<HTMLElement>(`[data-block-uid="${uid}"]`) ?? null;
    if (row !== null && transcriptEl !== null) {
      transcriptEl.scrollTop += row.getBoundingClientRect().top - transcriptEl.getBoundingClientRect().top - 24;
      pinReadingAnchor();
    }
    afterPaging();
    return row;
  }

  function revealEarlier() {
    applyEarlierPage(
      pageEarlier({ start: renderStart, end: renderEnd }, store.blocks.length),
      false,
    );
  }

  function revealLater() {
    const total = store.blocks.length;
    if (
      transcriptEl === null ||
      pagingTranscript ||
      renderEnd >= total ||
      store.hydrating
    ) {
      return;
    }
    pagingTranscript = true;
    const plan = pageLater({ start: renderStart, end: renderEnd }, total);
    const settled = keepInReach(plan, rowsNearReader());
    const reachesTail = settled.end >= total;
    setRangeAnchored(settled.start, settled.end, {
      live: reachesTail,
      tail: reachesTail,
    });
    void tick().then(afterPaging);
  }

  // Earlier pages should feel like ordinary transcript scrolling, not a
  // pagination workflow. The sentinel also fills a tall/empty viewport where
  // no scroll event can fire. Keep the button below only as a compatibility
  // fallback for a browser without IntersectionObserver.
  $effect(() => {
    const root = transcriptEl;
    const sentinel = historySentinelEl;
    const hasEarlier = renderStart > 0;
    if (
      !canAutoLoadHistory ||
      !visible ||
      !hasEarlier ||
      store.hydrating ||
      root === null ||
      sentinel === null
    ) {
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (!entries.some((entry) => entry.isIntersecting)) return;
        const plan = autoPageEarlier(
          { start: renderStart, end: renderEnd },
          store.blocks.length,
          atBottom,
        );
        if (plan !== null) applyEarlierPage(plan, plan.preserveTail);
      },
      { root, rootMargin: "96px 0px 0px" },
    );
    observer.observe(sentinel);
    return () => observer.disconnect();
  });

  // The forward twin: scroll-driven prefetch (maybePrefetch) usually mounts
  // the next page long before the reader gets here, but a window that already
  // ends inside the viewport produces no scroll event to drive it. Re-created
  // per page (it reads renderEnd), so a sentinel still in view keeps paging;
  // `hasLaterRows` is derived so a live turn's appends don't rebuild it.
  const hasLaterRows = $derived(!atLiveEdge);
  $effect(() => {
    const root = transcriptEl;
    const sentinel = laterSentinelEl;
    void renderEnd;
    if (
      !canAutoLoadHistory ||
      !visible ||
      !hasLaterRows ||
      store.hydrating ||
      root === null ||
      sentinel === null
    ) {
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) revealLater();
      },
      { root, rootMargin: "0px 0px 96px" },
    );
    observer.observe(sentinel);
    return () => observer.disconnect();
  });

  // On (re)mount, restore the saved reading position ONCE: bottom-pinned
  // sessions stick to the bottom, otherwise jump back to where the user was
  // reading. Guarded so it never re-fires mid-stream and fights the autoscroll.
  let didRestore = false;
  $effect(() => {
    const el = transcriptEl;
    if (el === null || didRestore || store.hydrating || !renderReady) return;
    didRestore = true;
    const saved = chatScroll(session.id);
    void tick().then(() => {
      if (transcriptEl === null) return;
      if (saved.atBottom) {
        scrollToBottom();
        return;
      }
      // The saved offset is relative to the rendered rows (see
      // saveReadingPosition); size the spacer first, then land past it.
      rebalanceSpacer();
      transcriptEl.scrollTop = spacerPx + saved.scrollTop;
      lastScrollTop = transcriptEl.scrollTop;
      pinReadingAnchor();
    });
  });

  // --- transcript fork -------------------------------------------------------
  // An assistant action branches AFTER its row. A user action edits by
  // branching BEFORE its row and restoring the prompt as an unsent draft.
  // Same-agent choices upgrade to native only when that exact preceding
  // boundary is representable by the vendor.
  let forkIntent = $state<null | {
    throughSeq: number;
    nativeAt: string | null;
    beforeUserId: string | null;
    draft: string | null;
    applying: boolean;
  }>(null);

  type ConversationBlock = Extract<ChatBlock, { kind: "user" | "message" }>;

  function previousConversationBlock(blockIndex: number): ConversationBlock | null {
    // #105 renders immutable snapshots of a bounded window, so object identity
    // does not match the reducer's source array. The render item retains the
    // absolute source index specifically for boundary-sensitive actions.
    for (let i = blockIndex - 1; i >= 0; i--) {
      const previous = store.blocks[i];
      if (previous.kind === "user" || previous.kind === "message") return previous;
    }
    return null;
  }

  function askFork(block: ChatBlock, blockIndex: number) {
    if (block.kind === "user") {
      // Editing a sent prompt is a branch BEFORE that prompt: preserve the
      // source, copy only its preceding history, and restore the text as an
      // unsent draft in the destination composer.
      const previous = previousConversationBlock(blockIndex);
      const beforeUserId = block.checkpoint?.id ?? block.id;
      forkIntent = {
        // Old/imported journals can lack a user id entirely. They cannot use
        // the daemon's exact before-user resolver, so fall back to the latest
        // preceding conversation row rather than duplicating the prompt.
        throughSeq: beforeUserId === null ? (previous?.forkSeq ?? 0) : block.forkSeq,
        nativeAt:
          agentKind === "claude"
            ? (block.checkpoint?.preceding ?? null)
            : agentKind === "codex" && previous?.kind === "message" && previous.nativeTurnComplete
              ? previous.turnId
              : null,
        beforeUserId,
        draft: block.text,
        applying: false,
      };
    } else if (block.kind === "message") {
      forkIntent = {
        throughSeq: block.forkSeq,
        nativeAt:
          agentKind === "codex" && block.nativeTurnComplete ? block.turnId : null,
        beforeUserId: null,
        draft: null,
        applying: false,
      };
    }
  }

  function confirmFork(destination: string) {
    const intent = forkIntent;
    if (intent === null || intent.applying) return;
    forkIntent = { ...intent, applying: true };
    void forkSession(
      session.id,
      intent.throughSeq,
      destination,
      intent.nativeAt,
      intent.beforeUserId,
    )
      .then((forked) => {
        forkIntent = null;
        if (intent.draft !== null) insertIntoComposer(forked.id, intent.draft);
        onForked?.(forked);
      })
      .catch((error: unknown) => {
        forkIntent = null;
        store.notice(`fork failed: ${String(error)}`, "error");
      });
  }

  // Stick to the bottom while new content streams, unless the reader scrolled
  // up or is composing. `lastSeq` catches in-place chunk/tool updates; every
  // request is coalesced by queueBottomScroll above.
  $effect(() => {
    void store.blocks.length;
    void store.pending.length;
    void store.elicitations.length;
    void store.lastSeq;
    if (!visible || store.hydrating || !renderReady || !atBottom || composerEngaged) return;
    queueBottomScroll();
  });

  // Artifact images/tables can resolve after the transcript page mounted, and
  // a pinned work tray or the composer can change the transcript's viewport
  // height without changing its content. Observe both surfaces so the same
  // coalesced scroll owner keeps the live edge anchored. A reader who scrolled
  // up keeps the row under them instead: resize callbacks run after layout and
  // before paint, so a preview decoding above them is absorbed by the spacer
  // in the frame it lands (the column is observed, the spacer beside it is
  // not, so absorbing never re-triggers this observer).
  $effect(() => {
    const column = columnEl;
    const transcript = transcriptEl;
    if (
      !visible ||
      column === null ||
      transcript === null ||
      typeof ResizeObserver === "undefined"
    ) {
      return;
    }
    const observer = new ResizeObserver(() => {
      if (atBottom) {
        if (!composerEngaged) queueBottomScroll();
      } else if (readingAnchor === null) {
        pinReadingAnchor();
      } else {
        holdReadingAnchor();
      }
    });
    observer.observe(column);
    observer.observe(transcript);
    return () => observer.disconnect();
  });

  /** The images a message shows only as a count: all of them when none has
   *  a saved copy (old journals, Remote Control), else the ones whose save
   *  failed. Empty when every picture is shown. */
  function unsavedImages(m: { attachments: number; attachmentPaths: string[] }): string {
    const n = m.attachments - m.attachmentPaths.length;
    if (n <= 0) return "";
    const noun = `image${n > 1 ? "s" : ""}`;
    return m.attachmentPaths.length > 0 ? `+${n} ${noun}` : `${n} ${noun}`;
  }

  /** `afterTurn`: hold it until the running turn ends (`send_after_turn`)
   *  instead of having the agent read it at its next step. Idle, the daemon
   *  treats both as an ordinary send. */
  function sendMessage(text: string, images: ImageAttachment[], afterTurn: boolean): boolean {
    if (images.length > 0 && !capabilities.image_input) {
      store.notice(`${agentName} does not support images in this chat. Your draft is kept.`, "error");
      return false;
    }
    const blocks: Record<string, unknown>[] = [];
    if (text.length > 0) blocks.push({ type: "text", text });
    // Codex exposes skills through `skills/list`; an exact `/skill-name`
    // token keeps the user's original prose AND adds the app-server's native
    // skill input block. Claude catalog entries have no skill_path and keep
    // riding its own slash-command text path.
    blocks.push(...skillBlocksForText(text, composerCommands));
    for (const img of images) {
      blocks.push({ type: "image", media_type: img.media_type, data: img.data });
    }
    return socket.send({ type: afterTurn ? "send_after_turn" : "send", blocks });
  }

  // A send made outside the composer (the Mastermind panel's one-click
  // prompts) follows exactly like onSubmit below.
  $effect(() =>
    registerFollow(session.id, () => {
      atBottom = true;
      queueBottomScroll(true);
    }),
  );

  function onSubmit(text: string, images: ImageAttachment[], afterTurn = false): boolean {
    // The daemon owns delivery semantics so reconnect/replay stay exact: a
    // mid-turn send waits in the pending stack until the agent reads it at
    // its next step (or, `afterTurn`, until the turn ends). Returns false
    // when the socket isn't open so the composer keeps the draft.
    const accepted = sendMessage(text, images, afterTurn);
    if (accepted) {
      // Submission is stronger intent than merely clearing a draft: the user
      // expects to see the delivered/queued bubble and the reply it starts.
      atBottom = true;
      queueBottomScroll(true);
    }
    return accepted;
  }

  /** One never-lose-a-click path for every interactive AgentCommand. A closed
   *  socket cannot queue locally (replay would make that ambiguous), so keep
   *  the authoritative UI state unchanged and tell the user to retry. */
  function sendCommand(command: Record<string, unknown>, failure: string): boolean {
    if (socket.send(command)) return true;
    store.notice(`not connected — ${failure}, try again in a moment`, "error");
    return false;
  }

  function decide(requestId: string, optionId: string, destination?: string, feedback?: string) {
    // Never lose a decision to a closed socket: the card stays answerable
    // (no resolved event will arrive), so say why nothing happened.
    sendCommand(
      {
        type: "permission",
        request_id: requestId,
        option_id: optionId,
        ...(destination !== undefined ? { destination } : {}),
        ...(feedback !== undefined ? { feedback } : {}),
      },
      "decision not sent",
    );
  }

  function answer(requestId: string, answers: Record<string, string[]>) {
    sendCommand({ type: "answer", request_id: requestId, answers }, "answer not sent");
  }

  function interrupt() {
    sendCommand({ type: "interrupt" }, "stop not sent");
  }

  // Stop/background ride the same never-lose-a-click contract as decide():
  // a send into a closed socket says why nothing happened.
  function stopTask(id: string) {
    sendCommand({ type: "stop_task", task_id: id }, "stop not sent");
  }
  function backgroundTool(id: string) {
    sendCommand({ type: "background_tool", tool_call_id: id }, "background request not sent");
  }

  /** Pull back a still-queued message before the agent consumes it. The store
   *  removes it on the resulting `user_message_update{cancelled}` (deterministic
   *  from the wire, so replay agrees). Respect the closed-socket rule: if it
   *  can't send, the message stays queued and the user can retry. */
  function cancelQueued(id: string) {
    sendCommand({ type: "cancel_queued", id }, "couldn't cancel");
  }

  /** Stop the running turn so this waiting message — and every other one
   *  still waiting — is read right away. The bubble stays until the daemon
   *  resolves it `sent`, so a disconnect or a no-op never lies about delivery. */
  function sendQueuedNow(id: string) {
    sendCommand({ type: "send_now", id }, "couldn't send now");
  }

  /** Dialog-only slash commands get native UI here instead of the CLI's
   *  "isn't available in this environment" dead end. Arguments resolve
   *  directly ("/effort high", "/model opus"); bare commands open pickers. */
  function onSlash(name: string, args = ""): boolean {
    if (["model", "mode", "effort"].includes(name) && store.pendingModel !== null) {
      store.notice("Wait for the model change to finish.", "info");
      return true;
    }
    const arg = args.trim().toLowerCase();
    switch (name) {
      case "voice": {
        // Chimaera's /voice shows or hides the composer's mic — the chat's
        // voice mode, the same for every agent, dictated through Claude's
        // speech service. Claude Code's own modes (hold / tap — push-to-talk
        // on Space) are a terminal's; here they just mean "on".
        const on = arg === "on" || arg === "hold" || arg === "tap";
        if (arg !== "" && !on && arg !== "off") {
          store.notice(`Unknown option “${args.trim()}” — use on or off.`, "info");
          return true;
        }
        // Bare /voice turns it off only when the mic is actually there: where
        // it's hidden (no login on this host), /voice says why instead.
        if (arg === "off" || (arg === "" && getSetting("chat.voice") && hostCanDictate())) {
          setSetting("chat.voice", false);
          store.notice("Voice dictation off.", "info");
          return true;
        }
        void enableVoice();
        return true;
      }
      case "rename": {
        // The agent CLIs can't rename their own thread from here (claude
        // punts, codex has no such command) — but chimaera owns the session
        // name. Pin it, so the tab, rail, and recents all follow. Case is
        // preserved (not the lowercased `arg`).
        const next = args.trim();
        if (next.length === 0) {
          store.notice("usage: /rename <new name>", "info");
          return true;
        }
        void renameSession(session.id, next)
          .then(() => store.notice(`renamed to “${next}”`, "info"))
          .catch((e: unknown) => store.notice(`rename failed: ${String(e)}`, "error"));
        return true;
      }
      case "model": {
        if (!supports("set_model")) return false;
        const hit = modelChoices.find(
          (m) => m.id.toLowerCase() === arg || m.label.toLowerCase() === arg,
        );
        if (arg.length > 0 && hit !== undefined) {
          return pickModel(hit.id);
        }
        // "Auto review" reads like a model to humans but is a Codex approval
        // mode. Accept the common slip without silently opening the wrong
        // menu, while teaching the canonical command for next time.
        const modeHit = store.modes.find(
          (m) => m.id.toLowerCase() === arg || m.label.toLowerCase() === arg,
        );
        if (arg.length > 0 && modeHit !== undefined) {
          store.notice(`“${args.trim()}” is a mode — switching it (use /mode next time)`, "info");
          return pickMode(modeHit.id);
        }
        if (args.trim().length > 0 && allowCustomModel) {
          const selection = customModelSelection(args);
          if (selection.id !== null) return pickModel(selection.id);
          store.notice(selection.error, "error");
          return true;
        }
        menu = "model";
        return true;
      }
      case "mode": {
        const hit = store.modes.find(
          (m) => m.id.toLowerCase() === arg || m.label.toLowerCase() === arg,
        );
        if (arg.length > 0 && hit !== undefined) {
          return pickMode(hit.id);
        } else if (store.modes.length > 0) {
          menu = "mode";
        } else {
          return false;
        }
        return true;
      }
      case "remote-control":
      case "rc": {
        // Claude's own /remote-control|/rc is a client-side toggle in every
        // official host; here too. "on"/"off" pin the direction, bare toggles.
        const live =
          store.remoteControl !== null && store.remoteControl.state !== "error";
        const enable = arg === "on" ? true : arg === "off" ? false : !live;
        if (enable && agentKind === "claude" && !store.remoteControlAvailable) {
          store.notice("Remote Control is not offered on this deployment", "info");
          return true;
        }
        setRemoteControl(enable);
        return true;
      }
      case "usage":
      case "cost":
        // Answered by a usage_report event (plan-limit windows — the honest
        // signal on subscription plans; dollars are not shown). Codex reads
        // the same data from account/read.
        if (!supports("get_usage")) return false;
        return sendCommand({ type: "get_usage" }, "usage request not sent");
      case "compact":
        // Codex has no slash catalog; thread/compact/start is the native
        // path (the compaction turn's notice confirms completion). Claude's
        // own /compact rides its catalog — fall through to the CLI send.
        if (!supports("compact")) return false;
        if (!sendCommand({ type: "compact" }, "compact request not sent")) return false;
        return true;
      case "mcp":
        if (supports("get_mcp")) {
          if (!sendCommand({ type: "get_mcp" }, "MCP request not sent")) return false;
          store.mcpServers = null;
          menu = "mcp";
          return true;
        }
        return false;
      case "effort":
        if (!hasEffort) return false;
        if (arg.length > 0 && effortChoices.includes(arg)) {
          return pickEffort(arg);
        } else {
          menu = "effort";
        }
        return true;
      case "ultracode":
        if (!hasUltracode) return false;
        if (arg === "on" || arg === "off") {
          return setUltracode(arg === "on");
        } else {
          return toggleUltracode();
        }
      case "login":
        // /login is an interactive OAuth / setup-token / SSO flow the `-p`
        // stream-json CLI can't run ("/login isn't available in this
        // environment") — so an expired session dead-ends in chat with no way
        // back. Flip to the real TUI, where claude's own /login handles every
        // auth method safely (chimaera never sees the credentials); sign in
        // there, then toggle back to chat with the pane-bar button. Claude
        // only for now — codex's auth flow (`codex login`) is a follow-up.
        if (agentKind !== "claude" || onSwitchToTerminal === undefined) return false;
        onSwitchToTerminal();
        return true;
      default:
        return false;
    }
  }

  /** Turn dictation on — after the checks /voice makes in Claude Code: a
   *  login the speech service takes (on the daemon's host) and a microphone
   *  this window may use (its permission prompt comes now, not mid-word). */
  async function enableVoice() {
    const problem = await voiceProblem();
    if (problem !== null) {
      store.notice(problem, "error");
      return;
    }
    setSetting("chat.voice", true);
    const chord = keyHint("dictate");
    store.notice(`Voice dictation on — click the mic${chord ? ` or press ${chord}` : ""} to talk.`, "info");
  }

  /** Words dictation should favor: where this chat works, and who it's with. */
  const voiceTerms = $derived.by(() => {
    const ctx = linkContext();
    const base = (p: string | null | undefined) =>
      p ? (p.replace(/\/+$/, "").split("/").pop() ?? "") : "";
    return ["Chimaera", "Claude", "Codex", base(ctx.root), base(ctx.cwd)].filter(
      (t) => t.length > 0,
    );
  });

  function setUltracode(enabled: boolean): boolean {
    return sendCommand({ type: "set_ultracode", enabled }, "ultracode change not sent");
  }

  function toggleUltracode(): boolean {
    return setUltracode(!store.ultracode);
  }

  /** Where this chat's relative references resolve: App's context for the
   *  session (live cwd, spawn cwd, workspace root — the terminal's answer
   *  too), else what the session row itself knows. */
  function linkContext(): LinkContext {
    return (
      chatLinkContext(session.id) ?? {
        cwd: session.cwd_current ?? session.cwd,
        spawnCwd: session.cwd,
        root: null,
        workspaceId: session.workspace_id ?? null,
      }
    );
  }

  /** Every path candidate in this chat (prose, code spans, links, user
   *  messages, tool locations) resolves through one batched, cached
   *  resolver: one request per base ladder per batch, the workspace id
   *  enabling the daemon's unique-basename / path-suffix fallbacks. */
  async function validateProse(candidates: string[]): Promise<ValidateAnswer> {
    const ctx = linkContext();
    const answer: Required<ValidateAnswer> = { valid: {}, ambiguous: {}, unchecked: [] };
    const groups = groupByBases(candidates, ctx);
    const results = await Promise.allSettled(
      groups.map((g) => fsValidate(g.candidates, g.bases[0], ctx.workspaceId, g.bases.slice(1))),
    );
    if (results.length > 0 && results.every((r) => r.status === "rejected")) {
      throw (results[0] as PromiseRejectedResult).reason;
    }
    results.forEach((r, i) => {
      if (r.status === "rejected") {
        answer.unchecked.push(...groups[i].candidates);
        return;
      }
      Object.assign(answer.valid, r.value.valid);
      Object.assign(answer.ambiguous, r.value.ambiguous);
      answer.unchecked.push(...r.value.unchecked);
    });
    return answer;
  }
  // The scope reads the session and workspace untracked: a template that
  // peeks must not re-render on every session update, only on answers.
  const prosePaths = new PathResolver(validateProse, {
    root: () => linkContext().root,
    scope: (c) => untrack(() => resolveScope(linkContext(), c)),
  });
  /** Files the chat shows as embed cards (prose `![](…)`, the turn
   *  gallery's shell-written files) resolve against the same directories,
   *  strictly: an embed names one file. */
  const proseEmbeds = new EmbedResolver(() => untrack(() => linkContext()));
  /** What the transcript's path links and document chips preview on a
   *  rest (the document view's hover preview, over the transcript). */
  const hoverTargets = new HoverTargets();
  let chatEl = $state<HTMLElement | null>(null);
  let hoverPreviews: HoverPreviews | null = null;
  $effect(() => {
    const root = transcriptEl;
    const layer = chatEl;
    if (root === null || layer === null) return;
    const h = new HoverPreviews({
      root,
      layer: () => layer,
      docPath: () => "",
      mode: () => "reading",
      text: () => null,
      links: () => {
        const ctx = untrack(() => linkContext());
        return { wsRoot: ctx.root, workspaceId: ctx.workspaceId };
      },
      // Asked fresh, not through proseEmbeds (whose hits stand 30 s): a
      // preview shows the file as it is now, and the agent may just have
      // rewritten it. The chat's targets are absolute paths.
      ask: (ref) =>
        resolveTargets([ref.target], "/").then(
          (r) => r[ref.target] ?? null,
          () => null,
        ),
      theme: () => untrack(() => activeTheme().kind),
      fontSize: () => untrack(() => chatFontSize),
      targetOf: (el) => hoverTargets.targetOf(el),
      anchors: false,
      standalone: true,
    });
    hoverPreviews = h;
    return () => {
      h.destroy();
      if (hoverPreviews === h) hoverPreviews = null;
    };
  });
  // A hidden tab keeps its DOM: a preview must not outlive the view.
  $effect(() => {
    if (!visible) hoverPreviews?.hide();
  });

  // A turn end is when files the agent mentioned have come to exist: drop
  // the misses so the renderers holding them ask again.
  let wasRunning = false;
  $effect(() => {
    const running = store.running;
    if (wasRunning && !running) prosePaths.expireMisses();
    wasRunning = running;
  });

  /** Open a resolved path through the workbench opener (shared/openPath.ts):
   *  files at their line, Cmd/Ctrl-click in a split, dirs in the Finder. */
  function openProsePath(path: string, kind: PathKind, opts: OpenPathOptions = {}) {
    if (openPath(path, kind, opts)) return;
    if (onOpenPath !== undefined) onOpenPath(path, kind);
    else if (kind === "file") onOpenFile?.(path);
  }

  /** A path from structured data (a tool location, an artifact tile) that
   *  may be relative: resolve it against the session first. */
  function openLocation(path: string) {
    void resolveAndOpen(prosePaths, path, openProsePath, { at: { x: 0, y: 0 } });
  }

  /** The composer's palette: chimaera-native pickers first (they don't
   *  exist in the CLI's -p catalog), then the CLI's own commands. */
  const composerCommands = $derived.by((): ComposerCommand[] => {
    const native: ComposerCommand[] = [];
    native.push({ name: "rename", description: "rename this session — chimaera" });
    if (supports("set_model")) native.push({
      name: "model",
      description: `switch model — chimaera picker`,
      options: modelChoices.map((model) => ({
        value: model.id,
        label: model.label,
        description:
          "description" in model && typeof model.description === "string"
            ? model.description
            : model.id,
      })),
    });
    if (supports("set_mode") && store.modes.length > 0) {
      native.push({
        name: "mode",
        description: "permission mode — chimaera picker",
        options: store.modes.map((mode) => ({ value: mode.id, label: mode.label })),
      });
    }
    if (hasEffort) {
      native.push({
        name: "effort",
        description: `reasoning effort (${effortChoices.join("/")}) — chimaera picker`,
        options: effortChoices.map((choice) => ({ value: choice, label: choice })),
      });
    }
    if (hasUltracode) {
      native.push({
        name: "ultracode",
        description: "toggle ultracode (on/off) — session only",
        options: [
          { value: "on", label: "On" },
          { value: "off", label: "Off" },
        ],
      });
    }
    if (supports("get_usage")) native.push({ name: "usage", description: "plan usage limits — chimaera panel" });
    native.push({
      name: "voice",
      description: "voice dictation: show or hide the mic (on/off) — Claude's speech service",
      options: [
        { value: "on", label: "on", description: "the mic button dictates into the message" },
        { value: "off", label: "off", description: "hide the mic" },
      ],
    });
    if (agentKind === "claude" && store.remoteControlAvailable) {
      native.push({
        name: "remote-control",
        description: "take this session with you — Claude app / claude.ai/code",
      });
    }
    if (supports("get_mcp")) native.push({ name: "mcp", description: "MCP servers — chimaera panel" });
    if (agentKind === "claude") {
      native.push({ name: "login", description: "sign in — opens the terminal for Claude's native auth" });
    }
    if (supports("compact")) {
      native.push({ name: "compact", description: "compact conversation context" });
    }
    const nativeNames = new Set(native.map((n) => n.name.toLowerCase()));
    return [
      ...native,
      ...store.slashCommands.filter((c) => !nativeNames.has(c.name.toLowerCase())),
    ];
  });

  // --- checkpoint rewind ------------------------------------------------------
  // Claude: click "rewind" on a user message → dry-run report (rewind_files) →
  // confirm dialog → restore files → optionally fork the conversation there.
  // Codex: no file restore exists — the dialog is a plain confirmation and the
  // rewind is conversation-only (the daemon rolls the thread back in place).
  // The intent flag keeps replayed RewindResult events from reopening UI.
  const conversationOnlyRewind = agentKind !== "claude";
  let rewindIntent = $state<null | {
    id: string;
    preceding: string | null;
    fork: boolean;
    stage: "dry" | "confirm" | "applying";
  }>(null);
  const rewindReport = $derived(
    rewindIntent !== null && store.rewind?.userMessageId === rewindIntent.id
      ? store.rewind
      : null,
  );

  function askRewind(checkpoint: { id: string; preceding: string | null }) {
    store.rewind = null;
    if (conversationOnlyRewind) {
      rewindIntent = { id: checkpoint.id, preceding: checkpoint.preceding, fork: true, stage: "confirm" };
    } else {
      rewindIntent = { id: checkpoint.id, preceding: checkpoint.preceding, fork: false, stage: "dry" };
      if (
        !sendCommand(
          { type: "rewind", user_message_id: checkpoint.id, dry_run: true },
          "rewind check not sent",
        )
      ) {
        // Do not leave the dialog forever in its loading state. The user can
        // retry the rewind button once the socket is ready.
        rewindIntent = null;
      }
    }
  }

  function confirmRewind(fork: boolean) {
    if (rewindIntent === null) return;
    if (conversationOnlyRewind) {
      const preceding = rewindIntent.preceding;
      if (preceding === null) {
        rewindIntent = null;
        return;
      }
      rewindIntent = { ...rewindIntent, stage: "applying" };
      void rewindSession(session.id, preceding)
        .then(() => {
          rewindIntent = null;
        })
        .catch((e: unknown) => {
          rewindIntent = null;
          store.notice(`rewind failed: ${String(e)}`, "error");
      });
      return;
    }
    const intent = rewindIntent;
    if (
      !sendCommand(
        { type: "rewind", user_message_id: intent.id, dry_run: false },
        "rewind request not sent",
      )
    ) {
      return;
    }
    rewindIntent = { ...intent, fork, stage: "applying" };
    store.rewind = null;
  }

  // The apply answer arrived: finish (and fork the conversation if asked).
  $effect(() => {
    const intent = rewindIntent;
    const report = rewindReport;
    if (intent === null || report === null || intent.stage !== "applying") return;
    rewindIntent = null;
    if (!report.applied) {
      store.notice(report.error ?? "rewind failed", "error");
      return;
    }
    if (intent.fork && intent.preceding !== null) {
      void rewindSession(session.id, intent.preceding).catch((e: unknown) => {
        store.notice(`fork failed: ${String(e)}`, "error");
      });
    } else {
      store.notice("files restored to checkpoint", "info");
    }
  });

  function pickModel(id: string): boolean {
    if (store.pendingModel !== null) return false;
    if (!sendCommand({ type: "set_model", model_id: id }, "model change not sent")) return false;
    store.markModelPending(id);
    menu = null;
    return true;
  }

  /** Remote Control on/off. The bare name sent is this session's DISPLAY
   *  name (what the rail shows — `session.name` is the agent kind for chat
   *  rows); the driver spells the claude.ai row `chimaera · <name>`. */
  function setRemoteControl(enabled: boolean): boolean {
    return sendCommand(
      {
        type: "set_remote_control",
        enabled,
        ...(enabled ? { name: displayName(session) } : {}),
      },
      "remote control request not sent",
    );
  }

  function pickMode(id: string): boolean {
    if (store.pendingModel !== null) return false;
    if (!sendCommand({ type: "set_mode", mode_id: id }, "mode change not sent")) return false;
    menu = null;
    return true;
  }

  /** Shift+Tab from the composer advances to the next permission mode, wrapping
   *  round — the same cycle the agent TUIs offer. No-op when the agent exposes
   *  no modes; an unknown current mode starts the cycle at the first entry. */
  function cycleMode() {
    if (store.modes.length === 0) return;
    const cur = store.modes.findIndex((m) => m.id === store.currentMode);
    const next = store.modes[(cur + 1) % store.modes.length];
    if (next.id !== store.currentMode) pickMode(next.id);
  }

  function pickEffort(id: string): boolean {
    if (store.pendingModel !== null) return false;
    if (!sendCommand({ type: "set_effort", effort_id: id }, "effort change not sent")) return false;
    menu = null;
    return true;
  }

  const EFFORT_HINT: Record<string, string> = {
    claude: "reasoning effort — applies immediately, this session only",
    codex: "reasoning effort — saved to this thread for the next message",
  };

  /** Extended-thinking toggle (claude). ON by default — chimaera's chat is a
   *  workbench for real coding work, where the reasoning pass earns its keep;
   *  Chat options shows an explicit on/off beside the toggle. The preference lives in
   *  the pooled store, not here, so a tab remount keeps it. */
  const hasThinking = $derived(supports("set_thinking"));
  /** Effective thinking state: the user's explicit choice, or ON by default
   *  (the reasoning pass earns its keep in a coding workbench). `null` in the
   *  store means "unchosen" — so a toggle-off (a real `false`) is never
   *  mistaken for the default and re-forced on. */
  const thinkingOn = $derived(store.thinkingEnabled ?? true);
  function toggleThinking() {
    const next = !thinkingOn;
    store.setThinking(next);
    if (!sendCommand({ type: "set_thinking", enabled: next }, "thinking change not sent")) {
      // Keep the user's preference, but mark it unsynchronized so the existing
      // connected-effect retries it on the next ready frame.
      store.markThinkingPending();
    }
  }
  // Push the effective preference to the live driver, once per driver process.
  // It pushes whatever the user's effective choice IS (never forces a value),
  // so it can't override a toggle-off; `thinkingPushed` is reset on each `init`
  // (a fresh process defaults thinking OFF) so a respawn/resume/view-toggle
  // re-syncs, and is marked only once the send actually leaves so an
  // undelivered push retries instead of stranding the chip out of sync.
  $effect(() => {
    if (!hasThinking || !store.connected || store.thinkingPushed) return;
    if (socket.send({ type: "set_thinking", enabled: thinkingOn })) {
      store.markThinkingPushed();
    }
  });

  const modeLabel = $derived(
    store.modes.find((m) => m.id === store.currentMode)?.label ?? store.currentMode,
  );
  /** Unlisted IDs stay intact: namespaces and punctuation can identify a provider. */
  const modelLabel = $derived.by(() => {
    if (store.pendingModel !== null) {
      const pending = modelChoices.find((m) => m.id === store.pendingModel);
      return `${pending?.label ?? store.pendingModel} · applying…`;
    }
    if (currentModel !== undefined) return currentModel.label;
    const m = store.model;
    if (m === null) {
      // Catalog order is not the user's configured model. In particular a
      // failed handshake must never claim the first curated model is active.
      return store.initialized ? "agent default" : null;
    }
    const choice = modelChoices.find((c) => c.id === m);
    if (choice !== undefined) return choice.label;
    return m;
  });

  /** Live status line under the transcript: what the agent is doing NOW —
   *  the agent's own phrase when it offers one (claude `task_summary`,
   *  "Counting files"), else the phase: starting → thinking / writing /
   *  running tools → working (between steps). */
  const agentBusy = $derived(store.running || store.compacting);
  const activityLabel = $derived.by(() => {
    if (store.compacting) return "Compacting context";
    if (store.activityLine !== null) return store.activityLine;
    const a = store.activity;
    if (a === null) return "Working";
    switch (a.kind) {
      case "thinking":
        return "Thinking";
      case "writing":
        return "Writing";
      case "tool":
        return "Running tools";
      default:
        return a.detail === "starting" ? "Starting" : "Working";
    }
  });
  /** The exact phase detail for the tooltip (a tool title, a thinking-token
   *  estimate) — the line itself stays short. */
  const activityDetail = $derived.by(() => {
    const a = store.activity;
    if (a === null || a.detail === "" || a.detail === "starting") return undefined;
    return a.kind === "thinking" ? `thinking · ${a.detail}` : a.detail;
  });
  /** Output tokens generated this turn ("12.3k tokens"); hidden until the
   *  agent reports any. */
  const turnTokensLabel = $derived.by(() => {
    const n = store.turnTokens;
    if (n <= 0) return null;
    return n >= 1000 ? `${(n / 1000).toFixed(1)}k tokens` : `${n} tokens`;
  });
  /** Work running beside the turn: live subagents plus the background set
   *  (commands, monitors, backgrounded agents; housekeeping excluded). A
   *  backgrounded agent's launch row completed at launch, so it is counted
   *  once — by the set. */
  const runningTasks = $derived.by(() => {
    // A foreground agent moved to the background (Ctrl-B) is still its live
    // row AND now an agent lane — count it once.
    const rows = new Set(store.activeAgents.map((a) => a.title.replace(/^(Agent|Task): /, "")));
    const lanes = store.backgroundTasks.filter(
      (t) => !t.ambient && !(backgroundKind(t) === "agent" && rows.has(t.description)),
    );
    return store.activeAgents.length + lanes.length;
  });

  // Elapsed-turn timer: a quiet counter that surfaces once a turn passes 5s and
  // ticks each second. The START is held in the chat pool (per session), so
  // switching away mid-turn and back keeps counting from the real turn start
  // instead of resetting — and performance.now() never leaks into replay. The
  // interval tears down when the turn ends or the component unmounts.
  let turnElapsedMs = $state(0);
  $effect(() => {
    const start = chatTurnStart(session.id, agentBusy, performance.now());
    if (start === null) {
      turnElapsedMs = 0;
      return;
    }
    turnElapsedMs = performance.now() - start;
    // Retained background chats keep their transcript DOM, but a counter the
    // user cannot see must not wake the main thread every second. Re-entering
    // the tab recomputes from the pool's original turn start before painting.
    if (!visible) return;
    const iv = setInterval(() => {
      turnElapsedMs = performance.now() - start;
    }, 1000);
    return () => clearInterval(iv);
  });
  /** Upward elapsed for the live status row (shared ladder: "7s", "1m 04s",
   *  "1h 02m 03s") — how long the running turn has been going, from its
   *  first second. */
  const turnElapsedLabel = $derived.by(() => {
    const total = Math.floor(turnElapsedMs / 1000);
    if (total < 1) return null;
    return formatElapsedSeconds(total);
  });
  /** A completed turn's duration for the turn-end badge. Sub-minute keeps one
   *  decimal ("2.4s"); a minute or more switches to the shared ladder so a
   *  long turn never renders as a raw "2664.6s". */
  function formatDurationMs(ms: number): string {
    const totalSec = ms / 1000;
    if (totalSec < 60) return `${totalSec.toFixed(1)}s`;
    return formatElapsedSeconds(Math.floor(totalSec));
  }

  type ActiveAgent = Extract<ChatBlock, { kind: "tool" }>;

  /** Live auxiliary surfaces are deliberately distinct from transcript rows.
   *  While visible they keep the reducer's proxies, so progress updates land
   *  immediately. On a retained hidden tab they become one plain snapshot:
   *  no hidden plan-row, task-dot, permission, or queued-message churn, while
   *  the keyed components stay mounted and keep local open/input state. */
  // Maintained incrementally by the reducer (audit B2): the old derived here
  // re-filtered EVERY block through its Svelte proxy on each structural/tool
  // event — O(blocks) per event just to keep a usually-empty tray current.
  const liveActiveAgents = $derived(store.activeAgents);
  let pinnedAgents = $state.raw<ActiveAgent[]>([]);
  let pinnedBackgroundTasks = $state.raw<BackgroundTask[]>([]);
  let pinnedPlan = $state.raw<PlanEntry[]>([]);
  let pinnedPermissions = $state.raw<PendingPermission[]>([]);
  let pinnedQuestions = $state.raw<PendingQuestion[]>([]);
  let pinnedElicitations = $state.raw<PendingElicitation[]>([]);
  let pinnedSends = $state.raw<PendingSend[]>([]);
  $effect(() => {
    if (!visible) {
      untrack(() => {
        pinnedAgents = $state.snapshot(liveActiveAgents);
        pinnedBackgroundTasks = $state.snapshot(store.backgroundTasks);
        pinnedPlan = $state.snapshot(store.plan);
        pinnedPermissions = $state.snapshot(store.pending);
        pinnedQuestions = $state.snapshot(store.questions);
        pinnedElicitations = $state.snapshot(store.elicitations);
        pinnedSends = $state.snapshot(store.pendingSends);
      });
      return;
    }
    pinnedAgents = liveActiveAgents;
    pinnedBackgroundTasks = store.backgroundTasks;
    pinnedPlan = store.plan;
    pinnedPermissions = store.pending;
    pinnedQuestions = store.questions;
    pinnedElicitations = store.elicitations;
    pinnedSends = store.pendingSends;
  });

  const jumpLabel = $derived(
    followedVersion !== store.transcriptVersion
      ? "jump to newest — new activity"
      : pinnedPermissions.length > 0
        ? "jump to newest — permission needed"
        : "jump to newest",
  );

  const planDone = $derived(pinnedPlan.filter((p) => p.status === "done").length);
  /** The step the agent is on now — surfaced in the plan summary so the
   *  current goal is legible without expanding the panel. `activeForm` is the
   *  agent's own present-continuous phrasing for exactly this spot ("Running
   *  tests"), so prefer it and fall back to the subject. */
  const planActive = $derived.by(() => {
    const active = pinnedPlan.find((p) => p.status === "in_progress");
    return active ? (active.activeForm ?? active.content) : null;
  });

  /** Blocked is orthogonal to status: a blocked task is still `todo`, so it
   *  would otherwise render identically to one that simply hasn't started.
   *  The server already filters `blockedBy` to blockers that are still open. */
  const isBlocked = (entry: PlanEntry) => entry.status !== "done" && entry.blockedBy.length > 0;
  const planMark = (entry: PlanEntry) =>
    entry.status === "done"
      ? "✓"
      : entry.status === "in_progress"
        ? "◐"
        : isBlocked(entry)
          ? "⊘"
          : "○";
  /** Agents often restate the subject as the description; showing both then is
   *  pure noise in a panel this small. */
  const planDetail = (entry: PlanEntry) => {
    const detail = entry.description?.trim();
    return detail && detail !== entry.content.trim() ? detail : null;
  };
  /** Finished work folds away: on a long plan the ✓ rows are the majority and
   *  push what's actually next out of view. They stay one click away, and when
   *  EVERYTHING is done there is nothing else to show, so the fold steps
   *  aside rather than leaving an empty panel. */
  let showFinished = $state(false);
  let planOpen = $state(false);
  const planLive = $derived(pinnedPlan.filter((p) => p.status !== "done"));
  const planFinished = $derived(pinnedPlan.filter((p) => p.status === "done"));
  const planFolds = $derived(planLive.length > 0 && planFinished.length > 0);
  const planRows = $derived(planFolds && !showFinished ? planLive : pinnedPlan);
  const planLabel = $derived(
    `plan · ${planDone}/${pinnedPlan.length}` +
      (planActive !== null ? ` · ◐ ${planActive}` : planLive.length === 0 ? " · all done" : ""),
  );

  /** Render list for the bounded page: consecutive tool blocks coalesce into
   *  one ToolGroup, a settled run of activity rows folds under the reply
   *  that followed it, and a settled run of finished-work lines folds on its
   *  own (activityFold.ts). Visible tail rows are live proxies;
   *  hidden/history rows are inert snapshots. Every item carries its absolute
   *  source index for scroll anchoring and boundary-sensitive actions. */
  type RowItem =
    | {
        t: "group";
        key: string;
        index: number;
        endIndex: number;
        tools: Extract<ChatBlock, { kind: "tool" }>[];
        /** The turn's later calls, where a retry clears this group's failure. */
        tail: TurnTail;
      }
    | { t: "single"; key: string; index: number; block: ChatBlock };
  /** The rows an activity fold absorbs. Finished-work lines never join it:
   *  they are results (and a woken turn's only stated cause), so they settle
   *  the run above them the way a reply does, and fold only among themselves
   *  (`finished-fold`). */
  type ActivityRow =
    | Extract<RowItem, { t: "group" }>
    | { t: "single"; key: string; index: number; block: Extract<ChatBlock, { kind: "thought" }> };
  type RenderItem =
    | RowItem
    | {
        t: "fold";
        key: string;
        index: number;
        endIndex: number;
        uid: number;
        items: ActivityRow[];
        tools: Extract<ChatBlock, { kind: "tool" }>[];
        tail: TurnTail | undefined;
        thoughts: number;
      }
    | {
        /** A settled run of finished-work lines (activityFold.ts). */
        t: "finished-fold";
        key: string;
        index: number;
        endIndex: number;
        uid: number;
        items: FinishedItem[];
      };
  type FinishedItem = { t: "single"; key: string; index: number; block: Extract<ChatBlock, { kind: "finished" }> };
  const isFinishedItem = (item: RowItem): item is FinishedItem =>
    item.t === "single" && item.block.kind === "finished";
  const isActivityRow = (item: RowItem): item is ActivityRow =>
    item.t === "group" || item.block.kind === "thought";
  const renderItems = $derived.by((): RenderItem[] => {
    const items: RowItem[] = [];
    let group: Extract<RowItem, { t: "group" }> | null = null;
    // One shared array per turn; each group reads it from its own end on.
    let turnTools: Extract<ChatBlock, { kind: "tool" }>[] = [];
    renderBlocks.forEach((block, i) => {
      const originalIndex = renderStart + i;
      if (
        block.kind === "user" ||
        block.kind === "wake" ||
        block.kind === "turn_end" ||
        (block.kind === "agent_message" && block.via === "send")
      )
        turnTools = [];
      // Every user block in `blocks` is delivered — queued/undelivered sends
      // live in the pending transcript tail (`store.pendingSends`), never
      // here — so they all render inline in transcript order.
      if (block.kind === "tool") {
        if (group === null) {
          group = {
            t: "group",
            key: `g-${block.id}`,
            index: originalIndex,
            endIndex: originalIndex,
            tools: [],
            tail: { tools: turnTools, from: 0 },
          };
          items.push(group);
        }
        group.tools.push(block);
        turnTools.push(block);
        group.tail.from = turnTools.length;
        group.endIndex = originalIndex;
      } else {
        group = null;
        // Keyed by the store's stable uid, never the array index: at cap every
        // append front-splices the array, and index keys would remount the
        // whole window (a full marked+KaTeX+DOMPurify pass per row) on the
        // next range write.
        items.push({ t: "single", key: `b-${block.uid}`, index: originalIndex, block });
      }
    });
    const spans = foldSpans(
      items,
      isActivityRow,
      // Whatever follows a run settles it — a reply, a finished line, a
      // permission decision, a message sent mid-turn. Only the trailing run
      // is live work.
      () => true,
    );
    // Finished-work lines never join an activity fold, but a long settled
    // run of them folds on its own. The two kinds of run never overlap.
    const finishedSpans = foldSpans(items, isFinishedItem, () => true, FINISHED_FOLD_MIN);
    if (spans.length === 0 && finishedSpans.length === 0) return items;
    const finishedAt = new Set(finishedSpans.map(([start]) => start));
    const allSpans = [...spans, ...finishedSpans].sort((a, b) => a[0] - b[0]);
    const folded: RenderItem[] = [];
    let at = 0;
    for (const [start, end] of allSpans) {
      folded.push(...items.slice(at, start));
      if (finishedAt.has(start)) {
        const run = items.slice(start, end).filter(isFinishedItem);
        folded.push({
          t: "finished-fold",
          key: `ff-${items[end].key}`,
          index: run[0].index,
          endIndex: run[run.length - 1].index,
          uid: run[0].block.uid,
          items: run,
        });
        at = end;
        continue;
      }
      // Every row in a span is an activity row; the filter only narrows.
      const run = items.slice(start, end).filter(isActivityRow);
      const first = run[0];
      const last = run[run.length - 1];
      const fold: Extract<RenderItem, { t: "fold" }> = {
        t: "fold",
        // Keyed by the row that settled it: the run's own first row can
        // change under a page trim or a history prepend, the closer cannot.
        key: `f-${items[end].key}`,
        index: first.index,
        endIndex: last.t === "group" ? last.endIndex : last.index,
        uid: first.t === "group" ? first.tools[0].uid : first.block.uid,
        items: run,
        tools: run.flatMap((item) => (item.t === "group" ? item.tools : [])),
        tail: run.reduce<TurnTail | undefined>((tail, item) => (item.t === "group" ? item.tail : tail), undefined),
        thoughts: run.filter((item) => item.t === "single").length,
      };
      folded.push(fold);
      at = end;
    }
    folded.push(...items.slice(at));
    return folded;
  });

  /** A finished turn's duration, kept out of the page (the live elapsed on
   *  the status line is the number that matters while it runs) and offered
   *  on the closing message's timestamp tooltip instead. Keyed by uid. */
  const turnDurations = $derived.by(() => {
    const byMessage = new Map<number, string>();
    renderBlocks.forEach((block, i) => {
      if (block.kind !== "turn_end" || block.durationMs < 100 || i === 0) return;
      const prev = renderBlocks[i - 1];
      if (prev.kind === "message") byMessage.set(prev.uid, formatDurationMs(block.durationMs));
    });
    return byMessage;
  });

  /** Identity of the last block (the streaming reveal keys off it). Queued
   *  sends render from their own pending tail, not `blocks`, so this is
   *  simply the delivered-block tail. Compared by UID, not index: a frozen
   *  hidden row keeps its live counterpart's uid, so the check stays true for
   *  exactly the still-streaming tail while the tab is hidden (the freeze
   *  contract) and can never alias a different row after trims shift
   *  indices. */
  const lastInlineUid = $derived(
    store.blocks.length > 0 ? store.blocks[store.blocks.length - 1].uid : -1,
  );

  /** One precise wall-clock timer for every assistant timestamp in this view.
   *  Each row reports its next label boundary; scheduling the earliest avoids
   *  a timer per message and leaves old transcripts idle between midnights. */
  let messageTimeNowMs = $state(Date.now());
  const messageTimestamps = $derived(
    visible
      ? renderBlocks.flatMap((block) => (block.kind === "message" ? [block.sentAtMs] : []))
      : [],
  );
  $effect(() => {
    if (messageTimestamps.length === 0) return;
    const refreshIn = messageTimestamps.reduce<number | null>((soonest, timestampMs) => {
      const next = messageTimestampRefreshIn(timestampMs, messageTimeNowMs);
      if (next === null) return soonest;
      return soonest === null ? next : Math.min(soonest, next);
    }, null);
    if (refreshIn === null) return;
    const timer = setTimeout(() => {
      messageTimeNowMs = Date.now();
    }, refreshIn);
    return () => clearTimeout(timer);
  });

  // --- context bridge: quoting a passage of this transcript -----------------
  // A selection here is published like a file view's, and the chip (or the
  // reference chord) quotes it into THIS view's composer. The chip floats on
  // the chat root, not in the column: the reading anchor binary-searches the
  // column's children as a vertical stack, which a floating child would
  // break. A chat that can't take a message offers no quote.
  const quoteOwner = {};
  let quoteChip = $state<{ x: number; y: number } | null>(null);
  const composerDisabled = $derived(store.exited !== null || store.degraded || store.fatalError !== null);
  const incompatibleRuntime = $derived(/GLIBC_[\d.]+[^\n]*not found/.test(store.fatalError ?? ""));

  function dropQuote(): void {
    quoteChip = null;
    clearSelection(quoteOwner);
  }

  /** The chip's rendered box once shown; before that, an estimate from the
   *  chat font (its label is `--text-xs`, fifteen mono glyphs). */
  function quoteChipSize(host: HTMLElement): { width: number; height: number } {
    const chip = host.querySelector<HTMLElement>(":scope > .ref-chip");
    if (chip !== null) return { width: chip.offsetWidth, height: chip.offsetHeight };
    const xs = Math.max(9, chatFontSize - 2);
    return { width: Math.ceil(xs * 0.62 * 15 + 26), height: Math.ceil(xs + 12) };
  }

  function placeQuoteChip(range: Range): void {
    const host = chatEl;
    const scroller = transcriptEl;
    if (host === null || scroller === null) return;
    const next = quoteChipPosition(range, host, scroller, quoteChipSize(host));
    if (quoteChip === null || quoteChip.x !== next.x || quoteChip.y !== next.y) quoteChip = next;
  }

  /** Geometry only: the selection moved (a scroll, a reflow), not what is
   *  selected. */
  function reanchorQuoteChip(): void {
    const column = columnEl;
    if (quoteChip === null || column === null) return;
    const range = quotableRange(column);
    if (range !== null) placeQuoteChip(range);
  }

  function syncQuoteSelection(): void {
    const column = columnEl;
    const range = column !== null ? quotableRange(column) : null;
    const text = range !== null ? (document.getSelection()?.toString() ?? "") : "";
    if (range === null || text.trim() === "") {
      dropQuote();
      return;
    }
    // A drag fires this per tick: publish only a change, so the app's
    // target resolution and every selection subscriber stay still.
    const current = get(activeSelection);
    if (current?.kind !== "chat" || current.view !== quoteOwner || current.text !== text) {
      setSelection(quoteOwner, { kind: "chat", sessionId: session.id, text, view: quoteOwner });
    }
    const shown = quoteChip !== null;
    placeQuoteChip(range);
    // First show places against an estimate; re-place once the chip has a box.
    if (!shown) void tick().then(reanchorQuoteChip);
  }

  $effect(() => {
    const scroller = transcriptEl;
    const column = columnEl;
    if (scroller === null || column === null || !visible || composerDisabled) return;
    // Re-anchor once per frame on a scroll (capturing, so a wide table's own
    // scroll counts) and on a reflow with none (a pane resize, the column
    // growing under a streamed reply).
    let frame = 0;
    const reanchor = () => {
      if (frame !== 0 || quoteChip === null) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        reanchorQuoteChip();
      });
    };
    const opts = { capture: true, passive: true } as const;
    const reflow = new ResizeObserver(reanchor);
    reflow.observe(scroller);
    reflow.observe(column);
    document.addEventListener("selectionchange", syncQuoteSelection);
    scroller.addEventListener("scroll", reanchor, opts);
    return () => {
      document.removeEventListener("selectionchange", syncQuoteSelection);
      scroller.removeEventListener("scroll", reanchor, opts);
      reflow.disconnect();
      if (frame !== 0) cancelAnimationFrame(frame);
      dropQuote();
    };
  });
</script>

<!-- The outside-dismiss action closes any open header menu / the /mcp panel on
     an outside pointerdown or Escape; `.menu-host` marks the surfaces that must
     stay open (the chips live in ChatHeader, the panel is a sibling overlay). -->
<div
  class="chat"
  bind:this={chatEl}
  class:focused
  class:visible
  style:--chat-font-size={`${chatFontSize}px`}
  style:--chat-line-height={chatLineHeight}
  style:--chat-measure={`${chatContentWidth}px`}
  style:--chat-font-family={chatFontFamily}
  style:padding-right={agentKind === "claude" && store.exited === null ? `${modDockWidth}px` : undefined}
  use:dismiss={{
    enabled: menu !== null,
    onDismiss: () => (menu = null),
    keepOpenWithin: ".menu-host",
  }}
>
  <ChatHeader
    {store}
    {agentKind}
    {agentName}
    mods={agentKind === "claude" ? mods : undefined}
    {visible}
    bind:menu
    {allowCustomModel}
    canPickModel={supports("set_model") && (modelChoices.length > 0 || allowCustomModel) && store.connected && store.exited === null && store.fatalError === null && store.pendingModel === null}
    canPickMode={supports("set_mode")}
    {modelChoices}
    {modelLabel}
    {modeLabel}
    {hasEffort}
    {effortChoices}
    {effortShown}
    effortHint={EFFORT_HINT[agentKind] ?? "reasoning effort"}
    {hasUltracode}
    {hasThinking}
    thinking={thinkingOn}
    onPickModel={pickModel}
    onPickMode={pickMode}
    onPickEffort={pickEffort}
    onToggleUltracode={toggleUltracode}
    onToggleThinking={toggleThinking}
    onInterrupt={interrupt}
    onSetRemoteControl={setRemoteControl}
  />

  <ChatFind target={chatEl} blocks={() => store.blocks} revision={store.transcriptVersion} {visible}
    trimmed={store.trimmedCount > 0} reveal={revealFindMessage} />

  <!-- Focusable so keyboard scrolling works in WKWebView (Safari never
       auto-focuses scrollers); role="log" announces new agent output. The
       input listeners only note that the reader is scrolling (onScroll). -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div
    class="transcript"
    role="log"
    aria-label="conversation"
    tabindex="0"
    bind:this={transcriptEl}
    onscroll={onScroll}
    ontouchmove={noteScrollIntent}
    onpointerdown={noteScrollIntent}
    onkeydown={noteScrollIntent}
  >
    <!-- Room for earlier history ahead of the rendered window: it absorbs
         every height change above a scrolled-up reader (see "reading
         position" in the script). Height is written directly, never bound. -->
    <div class="history-spacer" bind:this={spacerEl} aria-hidden="true"></div>
    <!-- One real reading column (the Claude Desktop measure): agent prose
         fills it from the left, user bubbles right-align inside it. -->
    <div class="column" bind:this={columnEl}>
    {#if store.hydrating}
      <div class="empty hydrate" aria-live="polite">
        <SessionGlyph kind="agent" {agentKind} size={18} state="alive" />
        <span>loading recent conversation…</span>
      </div>
    {:else}
    {#if store.exited === null && !store.fatalError && (!store.initialized || store.blocks.length === 0)}
      <div class="empty" role="status">
        <SessionGlyph kind="agent" {agentKind} size={18} />
        <span>{!store.connected ? `connecting to ${agentName}…` : store.initialized ? `${agentName} is ready` : `starting ${agentName}…`}</span>
        {#if store.connected && !store.initialized && store.startupDetail}
          <span class="startup-detail">{store.startupDetail}</span>
        {/if}
      </div>
    {/if}
    {#if renderStart > 0}
      {#if canAutoLoadHistory}
        <span class="history-sentinel" bind:this={historySentinelEl} aria-hidden="true"></span>
      {:else}
        <button
          class="history-more"
          title="Automatic history loading is unavailable in this browser"
          onclick={revealEarlier}
        >
          ↑ show {renderStart.toLocaleString()} earlier conversation item{renderStart === 1 ? "" : "s"}
        </button>
      {/if}
    {/if}
    {#snippet sentImages(paths: string[])}
      <div class="sent-images">
        <AttachmentStrip
          {paths}
          onOpen={(path, e) => openProsePath(path, "file", { split: e.metaKey || e.ctrlKey })}
        />
      </div>
    {/snippet}
    {#snippet activityRow(item: ActivityRow)}
      {#if item.t === "group"}
        <ToolGroup
          tools={item.tools}
          tail={item.tail}
          sourceIndex={item.index}
          sourceEnd={item.endIndex}
          sourceUid={item.tools[0]?.uid}
          {visible}
          onOpenPath={openProsePath}
          resolvePaths={prosePaths}
          onBackground={supports("background_tool") ? backgroundTool : undefined}
          onStopTask={supports("stop_task") ? stopTask : undefined}
          mods={agentKind === "claude" ? mods : undefined}
        />
      {:else}
        <ThoughtRow
          text={item.block.text}
          live={store.running && item.block.uid === lastInlineUid}
          {visible}
          onOpenPath={openProsePath}
          resolvePaths={prosePaths}
          embeds={proseEmbeds}
          {hoverTargets}
          sourceIndex={item.index}
          sourceUid={item.block.uid}
        />
      {/if}
    {/snippet}
    {#snippet finishedRow(block: Extract<ChatBlock, { kind: "finished" }>, index: number)}
      <FinishedRow
        {block}
        {visible}
        onOpenFile={openLocation}
        onOpenPath={openProsePath}
        resolvePaths={prosePaths}
        embeds={proseEmbeds}
        {hoverTargets}
        sourceIndex={index}
        sourceUid={block.uid}
      />
    {/snippet}
    {#each renderItems as item (item.key)}
      {#if item.t === "fold"}
        <ActivityFold
          tools={item.tools}
          tail={item.tail}
          thoughts={item.thoughts}
          steps={item.items.length}
          {visible}
          sourceIndex={item.index}
          sourceEnd={item.endIndex}
          sourceUid={item.uid}
        >
          {#each item.items as row (row.key)}
            {@render activityRow(row)}
          {/each}
        </ActivityFold>
      {:else if item.t === "finished-fold"}
        <FinishedFold
          rows={item.items.map((row) => row.block)}
          {visible}
          sourceIndex={item.index}
          sourceEnd={item.endIndex}
          sourceUid={item.uid}
        >
          {#each item.items as row (row.key)}
            {@render finishedRow(row.block, row.index)}
          {/each}
        </FinishedFold>
      {:else if isActivityRow(item)}
        {@render activityRow(item)}
      {:else if item.block.kind === "user"}
        {@const block = item.block}
        <!-- Only delivered (sent) user messages render inline; queued/dropped
             ones live in the pending tail below. -->
        {@const pictureOnly = block.text.length === 0 && block.attachmentPaths.length > 0}
        <div class="msg user" data-block-index={item.index} data-block-uid={block.uid}>
          {#if block.attachmentPaths.length > 0 && !pictureOnly}
            {@render sentImages(block.attachmentPaths)}
          {/if}
          <div class="bubble-row">
            <button
              class="message-action fork-btn"
              title="fork from this message into a new session (source keeps running)"
              aria-label="fork conversation from this message"
              onclick={() => askFork(block, item.index)}
            >
              ⑂
            </button>
            <!-- Codex rewinds whole turns from a preceding anchor, so its
                 first message (nothing precedes it) offers no button; claude
                 can still restore files there. -->
            {#if block.checkpoint !== null && (agentKind === "claude" || block.checkpoint.preceding !== null)}
              <button
                class="message-action rewind-btn"
                title={agentKind === "claude"
                  ? "rewind to before this message (restores files; optionally forks the conversation)"
                  : "rewind the conversation to before this message"}
                onclick={() => askRewind(block.checkpoint!)}
              >
                ↺
              </button>
            {/if}
            {#if pictureOnly}
              {@render sentImages(block.attachmentPaths)}
            {:else}
              <div class="bubble">
                {#if agentKind === "claude" && block.checkpoint?.id}
                  <ModSite {mods} component="UserMessage" instanceId={block.checkpoint.id} active={visible && $pageVisible} props={{ text: block.text, origin: { kind: block.origin ? "unclassified" : "composer" }, isExpanded: true }}>
                    {#snippet children(draw)}<UserText text={typeof draw.text === "string" ? draw.text : block.text} onOpenPath={openProsePath} resolvePaths={prosePaths} />{/snippet}
                  </ModSite>
                {:else}<UserText text={block.text} onOpenPath={openProsePath} resolvePaths={prosePaths} />{/if}
              </div>
            {/if}
          </div>
          {#if unsavedImages(block) !== "" || block.origin === "remote" || block.origin === "restart" || block.origin === "worker"}
            <span class="bubble-meta">
              {#if block.origin === "remote"}
                <span class="origin" title="sent from a Remote Control client (the Claude app or claude.ai/code)">via Remote Control</span>
              {:else if block.origin === "restart"}
                <span class="origin auto" title="chimaera sent this itself: the daemon restarted while this chat had work running, so it asked the resumed agent to pick that work back up (setting: Pick Up Interrupted Work After a Restart)">sent by chimaera after a restart</span>
              {:else if block.origin === "worker"}
                <span class="origin auto" title="a worker in this workspace sent this to the Mastermind (before agent communication); chimaera delivered it because the Mastermind acts on its own (auto)">from a worker</span>
              {/if}
              {#if unsavedImages(block) !== ""}
                <span class="attach">{unsavedImages(block)}</span>
              {/if}
            </span>
          {/if}
        </div>
      {:else if item.block.kind === "agent_message"}
        <!-- Other agents' messages: a card each, sender and vendor named;
             never a fork or rewind point. -->
        <AgentMessageCards
          messages={item.block.messages}
          caption={item.block.caption}
          text={item.block.text}
          mastermind={item.block.mastermind}
          {visible}
          onOpenPath={openProsePath}
          resolvePaths={prosePaths}
          embeds={proseEmbeds}
          {hoverTargets}
          sourceIndex={item.index}
          sourceUid={item.block.uid}
        />
      {:else if item.block.kind === "message"}
        {@const message = item.block}
        <div
          class="msg agent"
          class:streaming={store.running && item.block.uid === lastInlineUid}
          data-block-index={item.index}
          data-block-uid={item.block.uid}
        >
          <!-- streaming is TURN state (this row is the streaming tail);
               visibility rides separately so hiding a tab mid-stream freezes
               the live segment DOM in place instead of paying a synchronous
               whole-message canonical parse at tab-switch-away, and thaw
               resumes the reveal cursor instead of re-animating the row. -->
          {#snippet assistantProse(text: string)}
          <Markdown
            {text}
            streaming={store.running && item.block.uid === lastInlineUid}
            {visible}
            onOpenPath={openProsePath}
            resolvePaths={prosePaths}
            embeds={proseEmbeds}
            {hoverTargets}
            onReveal={() => {
              if (visible && atBottom && !composerEngaged) queueBottomScroll();
            }}
          />
          {/snippet}
          {#if agentKind === "claude" && item.block.nativeMessageId}
            <ModSite {mods} component="AssistantMessage" instanceId={item.block.nativeMessageId} active={visible && $pageVisible} props={{ text: item.block.text, isFirstOfReply: item.block.nativeFirstOfReply === true }}>
              {#snippet children(draw)}{@render assistantProse(typeof draw.text === "string" ? draw.text : message.text)}{/snippet}
            </ModSite>
          {:else}{@render assistantProse(item.block.text)}{/if}
          <AgentMessageMeta
            text={item.block.text}
            sentAtMs={item.block.sentAtMs}
            nowMs={messageTimeNowMs}
            onFork={() => askFork(item.block, item.index)}
            turnDuration={turnDurations.get(item.block.uid) ?? null}
          />
        </div>
      {:else if item.block.kind === "question"}
        <!-- The transcript's memory of an ask: invisible while the pending
             overlay below is the answerable card, a quiet question+answer
             card once resolved (replay rebuilds the same). -->
        {#if item.block.resolved}
          <div class="source-block" data-block-index={item.index} data-block-uid={item.block.uid}>
            <QuestionCard
              request={{ requestId: item.block.id, questions: item.block.questions, expiresAtMs: null }}
              answered={item.block.answers}
              {visible}
            />
          </div>
        {/if}
      {:else if item.block.kind === "finished"}
        {@render finishedRow(item.block, item.index)}
      {:else if item.block.kind === "wake"}
        <div class="wake activity" data-block-index={item.index} data-block-uid={item.block.uid}>
          <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true"
            ><path
              d="M13 8a5 5 0 1 1-1.5-3.55M13 3v2.5h-2.5"
              fill="none"
              stroke="currentColor"
              stroke-width="1.4"
              stroke-linecap="round"
              stroke-linejoin="round"
            /></svg
          >
          <span
            >{item.block.cause === "monitor"
              ? `Woke on a monitor event${item.block.label !== null ? ` · “${item.block.label}”` : ""}`
              : item.block.cause === "background"
                ? "Woke on background work"
                : "Resumed on its own"}</span
          >
        </div>
      {:else if item.block.kind === "notice"}
        {@const hook = item.block.tone === "info" ? hookNotice(item.block.text) : null}
        {#if hook}
          <HookRow said={hook} sourceIndex={item.index} sourceUid={item.block.uid} />
        {:else if !(incompatibleRuntime && item.block.text === store.fatalError)}
        <div
          class="notice"
          class:error={item.block.tone === "error"}
          data-block-index={item.index}
          data-block-uid={item.block.uid}>{item.block.text}</div
        >
        {/if}
      {:else if item.block.kind === "turn_end"}
        {@const block = item.block}
        <div class="source-block" data-block-index={item.index} data-block-uid={item.block.uid}>
          <!-- What the turn wrote, as one chip line after the closing prose. -->
          {#if block.artifacts.length > 0 || block.mentioned.length > 0}
            <ArtifactGallery
              paths={block.artifacts}
              mentioned={block.mentioned}
              covered={block.covered}
              startedAtMs={block.startedAtMs}
              endedAtMs={block.endedAtMs}
              resolver={proseEmbeds}
              onOpenPath={openProsePath}
              {hoverTargets}
            />
          {/if}

        </div>
      {:else if item.block.kind === "usage"}
        <div class="source-block" data-block-index={item.index} data-block-uid={item.block.uid}>
          <UsagePanel windows={item.block.windows} />
        </div>
      {/if}
    {/each}

    {#if !atLiveEdge}
      {#if canAutoLoadHistory}
        <span class="history-sentinel" bind:this={laterSentinelEl} aria-hidden="true"></span>
      {:else}
        <button
          class="history-more history-later"
          title="Automatic history loading is unavailable in this browser"
          onclick={revealLater}
        >
          ↓ show {Math.min(64, store.blocks.length - renderEnd).toLocaleString()} later conversation item{Math.min(64, store.blocks.length - renderEnd) === 1 ? "" : "s"}
        </button>
      {/if}
    {/if}

    <!-- Live-tail chrome must never be spliced directly after a historical
         page with newer transcript rows omitted in between. Page forward or
         jump first, so chronology remains visually honest. -->
    {#if !visible || atLiveEdge}
    {#each pinnedPermissions as request (request.requestId)}
      {#if request.plan !== null}
        <PlanApprovalCard
          {request}
          {visible}
          onDecide={(opt, feedback) => decide(request.requestId, opt, undefined, feedback)}
          onOpenPath={openProsePath}
          resolvePaths={prosePaths}
        />
      {:else}
        <PermissionCard
          {request}
          canFeedback={supports("permission_feedback")}
          canChooseDestination={supports("permission_destination")}
          {visible}
          onDecide={(opt, dest, feedback) => decide(request.requestId, opt, dest, feedback)}
        />
      {/if}
    {/each}

    {#each pinnedQuestions as request (request.requestId)}
      <QuestionCard {request} {visible} onAnswer={(answers) => answer(request.requestId, answers)} />
    {/each}

    {#each pinnedElicitations as request (request.requestId)}
      <ElicitationCard {request} {visible} onRespond={(action, content) => sendCommand({type: "elicitation", request_id: request.requestId, action, content}, "MCP response not sent")} />
    {/each}

    {#if agentBusy && pinnedPermissions.length === 0 && pinnedQuestions.length === 0 && pinnedElicitations.length === 0}
      <div class="status-row" aria-live={visible ? "polite" : "off"}>
        <span class="status-spark">
          <SessionGlyph kind="agent" {agentKind} size={12} state="alive" />
        </span>
        {#if turnElapsedLabel !== null}
          <span class="status-elapsed">{turnElapsedLabel}</span>
        {/if}
        {#if turnTokensLabel !== null}
          <span class="status-part" title="output tokens this turn">{turnTokensLabel}</span>
        {/if}
        {#if runningTasks > 0}
          <span class="status-part">{runningTasks} running task{runningTasks === 1 ? "" : "s"}</span>
        {/if}
        {#if agentKind === "claude"}
          <ModSite {mods} component="Spinner" instanceId="spinner" active={visible && $pageVisible} props={{ word: activityLabel, message: null, suffix: "…", mode: store.activity?.kind === "thinking" ? "thinking" : store.activity?.kind === "writing" ? "responding" : store.activity?.kind === "tool" ? "tool-use" : "requesting" }}>
            {#snippet children(draw)}<span class="status-label" title={activityDetail}>{typeof draw.message === "string" ? draw.message : typeof draw.word === "string" ? draw.word : activityLabel}</span>{/snippet}
          </ModSite>
        {:else}<span class="status-label" title={activityDetail}>{activityLabel}</span>{/if}
        {#if store.compacting}
          <span class="compaction-progress" role="progressbar" aria-label="Compacting conversation">
            <span></span>
          </span>
        {/if}
      </div>
    {/if}

    {#if store.fatalError !== null}
      <div class="notice error" class:runtime-error={incompatibleRuntime}>
        {#if incompatibleRuntime}
          <strong>This agent can't start on this host</strong>
          <p>This agent runtime needs newer Linux system libraries than this host provides. Use a compatible host or configure a compatible runtime in Settings → Agents. Reinstalling the same package will not fix this.</p>
          <details><summary>Startup details</summary><pre>{store.fatalError}</pre></details>
        {:else}
          {store.fatalError}
        {/if}
      </div>
    {/if}
    {#if store.degraded}
      <div class="notice">continued in terminal — this pane will switch</div>
    {:else if store.exited !== null}
      <div class="notice">
        agent exited{store.exited.status !== null ? ` (status ${store.exited.status})` : ""}
      </div>
    {/if}

    <!-- Pending sends are part of the scrollable reading surface, but remain
         OUT of `blocks` until the agent reads them: a waiting send must not
         splice the agent's current response. Keeping the stack at the
         transcript tail makes waiting text inspectable without pinning it to
         (and crowding) the composer. When the agent reads it — at its next
         step, mid-turn, or after the turn for an after-turn send — it leaves
         this stack and enters `blocks` right there. Send now interrupts the
         turn so every waiting message is read at once; a Stop preserves them;
         ✕ cancels one. Dropped sends remain visible as "not delivered" until
         dismissed, with replay-safe state owned by the daemon. -->
    {#if pinnedSends.length > 0}
      {@const waiting = pinnedSends.filter((s) => s.state === "queued").length}
      <div
        class="pending"
        aria-label="messages waiting for the agent"
        aria-live={visible ? "polite" : "off"}
      >
        {#each pinnedSends as send (send.id)}
          {@const pictureOnly = send.text.length === 0 && send.attachmentPaths.length > 0}
          {#if isAgentOrigin(send.origin)}
            <!-- Another agent's message steered in (Codex): it waits for the
                 agent's next step like a queued send; one that misses its
                 turn stays in the agent's inbox. -->
            {@const parsed = parseAgentText(send.text)}
            <AgentMessageCards
              messages={parsed.messages}
              caption={parsed.caption}
              text={send.text}
              mastermind={send.origin === "mastermind"}
              state={send.state}
              onDismiss={() => cancelQueued(send.id)}
              {visible}
              onOpenPath={openProsePath}
              resolvePaths={prosePaths}
              embeds={proseEmbeds}
              {hoverTargets}
            />
          {:else}
          <div class="msg user pending-msg" class:dropped={send.state === "dropped"}>
            {#if send.attachmentPaths.length > 0 && !pictureOnly}
              {@render sentImages(send.attachmentPaths)}
            {/if}
            <div class="bubble-row">
              {#if pictureOnly}
                {@render sentImages(send.attachmentPaths)}
              {:else}
                <div class="bubble">
                  <UserText
                    text={send.text}
                    onOpenPath={openProsePath}
                    resolvePaths={prosePaths}
                  />
                </div>
              {/if}
              {#if send.state === "queued" && store.running}
                <button
                  class="send-now-btn"
                  title={waiting > 1
                    ? "stop the current turn and send the waiting messages now"
                    : "stop the current turn and send this now"}
                  aria-label="send now (stops the current turn)"
                  onclick={() => sendQueuedNow(send.id)}
                >Send now</button>
              {/if}
              <button
                class="cancel-btn"
                title={send.state === "dropped"
                  ? "dismiss (this message was never delivered)"
                  : "cancel this message (remove it before the agent reads it)"}
                aria-label={send.state === "dropped"
                  ? "dismiss undelivered message"
                  : "cancel waiting message"}
                onclick={() => cancelQueued(send.id)}
              >
                ✕
              </button>
            </div>
            <span class="delivery" class:dropped={send.state === "dropped"}>
              {send.state === "dropped"
                ? "not delivered"
                : send.afterTurn
                  ? "after this turn"
                  : "next step"}
            </span>
            {#if unsavedImages(send) !== ""}
              <span class="attach">{unsavedImages(send)}</span>
            {/if}
          </div>
          {/if}
        {/each}
      </div>
    {/if}
    {/if}

    {#if hasDeferredActivity || !atBottom}
      <button
        class="jump"
        title={jumpLabel}
        aria-label={jumpLabel}
        onclick={scrollToBottom}
      >
        <span aria-hidden="true">↓</span>
      </button>
    {/if}
    {/if}
    </div>
    <!-- Room for the later rows past a history page (the spacer's twin). -->
    <div class="later-spacer" bind:this={laterSpacerEl} aria-hidden="true"></div>
  </div>

  {#if quoteChip !== null}
    <ReferenceChip x={quoteChip.x} y={quoteChip.y} quote />
  {/if}

  {#if pinnedAgents.length > 0}
    <AgentsTray
      agents={pinnedAgents}
      {visible}
      onStop={supports("stop_task") ? stopTask : undefined}
    />
  {/if}

  {#if pinnedBackgroundTasks.length > 0}
    <!-- Background work (backgrounded Bash / workflows) — stopTask sends the
         native task key the wire gave us; the driver passes it through. -->
    <BackgroundTray
      tasks={pinnedBackgroundTasks}
      {visible}
      onStop={supports("stop_task") ? stopTask : undefined}
    />
  {/if}

  {#if pinnedPlan.length > 0}
    <!-- Same shell as the subagent/background strips: one collapsible family
         above the composer instead of three different-looking bars. The glyph
         only breathes while a step is actually in flight. -->
    <WorkTray
      glyph="≡"
      label={planLabel}
      bind:open={planOpen}
      pulse={planActive !== null}
      {visible}
    >
      {#if planFolds}
        <button class="plan-fold" onclick={() => (showFinished = !showFinished)}>
          <Chevron open={showFinished} />
          <span>{planFinished.length} done</span>
        </button>
      {/if}
      {#each planRows as entry, i (entry.id ? `id:${entry.id}` : `ix:${i}`)}
        <div
          class="plan-row"
          class:done={entry.status === "done"}
          class:blocked={isBlocked(entry)}
        >
          <span class="plan-mark">{planMark(entry)}</span>
          <span class="plan-body">
            <span class="plan-line">
              <span class="plan-subject">{entry.content}</span>
              {#if entry.owner}<span class="plan-owner">@{entry.owner}</span>{/if}
              {#if isBlocked(entry)}<span class="plan-blocked"
                  >blocked by {entry.blockedBy.map((id) => `#${id}`).join(", ")}</span
                >{/if}
            </span>
            {#if planDetail(entry)}
              <span class="plan-desc">{planDetail(entry)}</span>
            {/if}
          </span>
        </div>
      {/each}
    </WorkTray>
  {/if}

  {#if rewindIntent !== null}
    <RewindDialog
      intent={rewindIntent}
      report={rewindReport}
      conversationOnly={conversationOnlyRewind}
      onCancel={() => (rewindIntent = null)}
      onConfirm={confirmRewind}
      {onOpenFile}
    />
  {/if}

  {#if forkIntent !== null}
    <ForkDialog
      agents={availableForkAgents}
      sourceAgent={agentKind}
      nativeAt={forkIntent.nativeAt}
      restoreDraft={forkIntent.draft !== null}
      applying={forkIntent.applying}
      onCancel={() => (forkIntent = null)}
      onConfirm={confirmFork}
    />
  {/if}

  {#if authServer !== null && session.workspace_id && agentKind === "claude"}
    <ConnectionDialog wsId={session.workspace_id} agent="claude" name={authServer} {visible}
      onClose={() => { authServer = null; sendCommand({type: "get_mcp"}, "MCP refresh not sent"); }}
      onConnected={() => { if (authServer) sendCommand({type: "reconnect_mcp", server: authServer}, "reconnect not sent"); }} />
  {/if}

  {#if menu === "mcp"}
    <McpPanel
      servers={store.mcpServers}
      onReconnect={(server) => sendCommand({ type: "reconnect_mcp", server }, "reconnect not sent")}
      onAuthenticate={session.workspace_id && agentKind === "claude" ? (server) => { authServer = server; menu = null; } : undefined}
      onToggleEnabled={(server, enabled) =>
        sendCommand({ type: "set_mcp_enabled", server, enabled }, "MCP change not sent")}
    />
  {/if}

  {#if store.promptSuggestion !== null && !agentBusy}
    <div class="suggestion-row">
      <button
        class="suggestion"
        title="suggested next prompt — click to use"
        onclick={() => {
          const text = store.promptSuggestion;
          store.promptSuggestion = null;
          if (text !== null) insertIntoComposer(session.id, text);
        }}
      >
        <span class="suggestion-mark">↳</span>
        <span class="suggestion-text">{store.promptSuggestion}</span>
      </button>
      <button
        class="suggestion-x"
        aria-label="dismiss suggestion"
        onclick={() => (store.promptSuggestion = null)}>×</button
      >
    </div>
  {/if}

  {#if session.git || sameFile.notesFor(session.id).length > 0}
    <!-- One quiet line just above the input. Left: the branch this
         conversation works on (only in a repository; hover names the
         worktree; never a prompt to do anything with git). Right: another
         live session wrote a file this one wrote. -->
    <div class="branch-line">
      {#if session.git}
        <span class="branch-slot"
          ><BranchChip git={session.git} onOpen={() => session.git && openBranchChanges(session.git)} /></span
        >
      {/if}
      <span class="branch-line-end"><SameFileNotice sessionId={session.id} {visible} /></span>
    </div>
  {/if}

  {#if agentKind === "claude" && store.exited === null}
    <ModsWorkbench transport={socket.nativeUi} host={chatEl} onDockWidth={(width) => (modDockWidth = width)} {visible} {focused} running={agentBusy} hasSurvey={store.questions.length > 0 || store.elicitations.length > 0} composer={composerApi} canEdit={!composerDisabled && store.pending.length === 0 && store.questions.length === 0 && store.elicitations.length === 0 && rewindIntent === null && forkIntent === null} canFocus={focused && !composerEngaged && !agentBusy && store.pending.length === 0 && store.questions.length === 0 && store.elicitations.length === 0 && rewindIntent === null && forkIntent === null} />
  {/if}

  <Composer
    bind:this={composerApi}
    onNativeEdit={agentKind === "claude" && mods.attached ? (request) => socket.nativeUi.request(request) : undefined}
    sessionId={session.id}
    imageInput={capabilities.image_input}
    view={quoteOwner}
    running={agentBusy}
    disabled={composerDisabled}
    slashCommands={composerCommands}
    workspaceId={session.workspace_id ?? null}
    {terminals}
    {focused}
    {visible}
    {onSubmit}
    {voiceTerms}
    onDraftState={(active) => (composerEngaged = active)}
    onInterrupt={interrupt}
    onCycleMode={cycleMode}
    {onSlash}
  />
</div>

<style>
  .chat {
    box-sizing: border-box;
    position: relative; /* anchors the rewind dialog + /mcp panel overlays */
    height: 100%;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--term-bg);
    color: var(--fg);
    font-family: var(--chat-font-family);
    --text-xs: max(9px, calc(var(--chat-font-size) - 2px));
    --text-sm: max(10px, calc(var(--chat-font-size) - 1px));
    --text-md: var(--chat-font-size);
    /* The transcript's column: 48em of the message text — Claude.ai's
       reading measure, ~100 characters — so a bigger font keeps its line
       length, capped by the Reading Width setting (--chat-measure). The
       document views use the same rule (previews/MarkdownView). */
    --chat-column: min(calc(48 * var(--chat-font-size)), var(--chat-measure));
    --text-lg: calc(var(--chat-font-size) + 2px);
  }
  .chat:not(.visible) .status-spark,
  .chat:not(.visible) .status-label,
  .chat:not(.visible) .compaction-progress > span {
    animation: none;
  }
  .transcript {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    /* The reading anchor + history spacer are the one anchoring mechanism on
       every engine; Chromium's native anchoring would correct the same shift
       a second time. */
    overflow-anchor: none;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
    padding: 14px 18px;
    display: flex;
    flex-direction: column;
  }
  /* The pane clips at its padding box and the transcript fills it edge to
     edge — the global outside ring (app.css) would be cut on three sides, so
     paint it inside. */
  .transcript:focus-visible {
    outline-offset: -2px;
  }
  /* One real column element, not per-child margin tricks (a block's own
     margin shorthand silently defeated those). It GROWS with content
     (flex-basis auto, no shrink), so children with overflow!=visible
     (tool cards, zero automatic min-size) are never squeezed by an
     overflowing transcript — and it fills the viewport when short, so
     .empty can center in it. */
  .column {
    --row-gap: 3px;
    flex: 1 0 auto;
    display: flex;
    flex-direction: column;
    gap: var(--row-gap);
    width: 100%;
    max-width: var(--chat-column);
    margin: 0 auto;
  }
  /* Activity lines (tool runs, thoughts, finished work, wakes) are the
     margin notes to the prose: one quieter tone, clustered tight, with a
     little air where prose begins and ends so messages stay the page's
     voice. */
  .column {
    --activity-fg: color-mix(in srgb, var(--muted) 82%, transparent);
  }
  .column > :global(.activity + .activity) {
    margin-top: -2px;
  }
  /* Below prose the message's hover rail already holds the space; above
     it, a cluster ends with a breath. */
  .column > :global(.activity + .msg.agent) {
    margin-top: 5px;
  }
  /* Blocks size to content and never absorb shrink: a tool card
     (overflow:hidden → zero automatic min-size) would otherwise collapse to
     its borders in a tall transcript. Agent prose and cards stretch to the
     column width (default align); user bubbles opt out via align-self. */
  .column > :global(*) {
    flex: none;
  }
  .source-block {
    display: flex;
    flex-direction: column;
    gap: 3px;
    width: 100%;
  }
  /* The composer and its satellites share the column; their 18px side
     padding rides OUTSIDE the measure so text edges line up with it. */
  .chat > :global(.composer),
  .chat > .suggestion-row,
  .chat > .branch-line,
  /* Every pinned strip (subagents, background, plan) — they were full-bleed
     while the plan alone was inset, so the group never lined up. */
  .chat > :global(.tray) {
    width: 100%;
    max-width: calc(var(--chat-column) + 36px);
    margin-left: auto;
    margin-right: auto;
    box-sizing: border-box;
    padding-left: 18px;
    padding-right: 18px;
  }
  /* No full-width rule under a centered column — the input's own border
     is the boundary (the Claude Desktop treatment). */
  .chat > :global(.composer) {
    border-top: none;
  }
  .startup-detail {
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .empty {
    margin: auto;
    color: var(--muted);
    font-size: var(--text-sm);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
  }
  .history-more {
    align-self: center;
    margin: 2px 0 10px;
    padding: 3px 10px;
    border: 1px solid color-mix(in srgb, var(--edge) 72%, transparent);
    border-radius: 999px;
    background: color-mix(in srgb, var(--fg) 3%, transparent);
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }
  .history-more:hover {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--accent) 48%, var(--edge));
  }
  .history-sentinel {
    width: 100%;
    height: 1px;
    pointer-events: none;
  }
  .history-spacer,
  .later-spacer {
    flex: none;
    height: 0;
  }
  .history-later {
    margin-top: 10px;
  }
  .msg {
    word-break: break-word;
    line-height: var(--chat-line-height);
    font-size: var(--text-md);
    animation: rise 0.15s ease; /* @keyframes rise lives in app.css */
  }
  @media (prefers-reduced-motion: reduce) {
    .msg {
      animation: none;
    }
  }
  /* User messages: quiet bubbles RIGHT-ALIGNED inside the column, agent
     prose plain from the left — the Claude Desktop shape. Longhand
     margins only: a shorthand here once zeroed the column's centering. */
  .msg.user {
    align-self: flex-end;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    max-width: min(85%, 40rem);
    margin-top: 14px;
    margin-bottom: 6px;
  }
  .msg.user .bubble {
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    border-radius: 14px;
    padding: 8px 14px;
    max-width: 100%;
  }
  .attach {
    color: var(--muted);
    font-size: var(--text-sm);
    margin-top: 2px;
  }
  /* The message's pictures sit above its bubble, on the user's side. */
  .sent-images {
    max-width: 100%;
    margin-bottom: 6px;
  }
  .bubble-row > .sent-images {
    margin-bottom: 0;
  }
  .pending-msg .sent-images {
    opacity: 0.55;
  }
  .bubble-meta {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  /* A message that arrived through the agent's Remote Control bridge (phone /
     claude.ai): a quiet accent tag under the bubble — same pill language as
     the header chip, so the two read as one feature. */
  .origin {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--accent);
    border: 1px solid color-mix(in srgb, var(--accent) 40%, var(--edge));
    border-radius: 999px;
    padding: 0 6px;
    line-height: 1.5;
  }
  /* The daemon's own message (a restart pick-up), not a feature: muted, so
     it reads as provenance rather than as another chip to act on. */
  .origin.auto {
    color: var(--muted);
    border-color: var(--edge);
  }
  /* Undelivered messages occupy the transcript tail, not fixed composer
     chrome. The transcript's own scrollbar can therefore move a large queue
     out of the way while the pending state remains visible at the live end. */
  .pending {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-top: 8px;
    padding-bottom: 10px;
  }
  /* A pending bubble is half-present — not in the conversation yet (waiting
     for the agent's next step, or for the turn to end). Reuses .msg.user's
     right-alignment so the queued→sent transition is visually continuous:
     the same bubble un-fades and moves up into the transcript on delivery.
     Tighter margins than an inline turn (the stack sets its own gap). */
  .pending-msg {
    margin-top: 0;
    margin-bottom: 0;
  }
  .pending-msg .bubble {
    opacity: 0.55;
    /* Outline, not border: follows the radius with zero layout shift. */
    outline: 1px dashed color-mix(in srgb, var(--fg) 30%, transparent);
    outline-offset: -1px;
  }
  /* Dropped (e.g. the agent process died before delivery): the text stays
     readable — no strikethrough — so it can be copied and re-sent by hand. */
  .pending-msg.dropped .bubble {
    outline-style: solid;
    outline-color: color-mix(in srgb, var(--err) 45%, transparent);
  }
  .send-now-btn {
    flex: none;
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
    padding: 3px 4px;
    border-radius: 6px;
    transition:
      color 0.12s ease,
      background 0.12s ease;
  }
  .send-now-btn:hover,
  .send-now-btn:focus-visible {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 9%, transparent);
  }
  .delivery {
    color: var(--muted);
    font-size: var(--text-xs);
    margin-top: 2px;
  }
  .delivery.dropped {
    color: var(--err);
  }
  .msg.agent {
    padding: 2px 0;
  }
  /* A settled message and its hover rail are one inline flow: the rail
     follows the last word and takes a line of its own only when that line
     is full — no row reserved under every message. The markdown shell is
     layout-neutral (its children lay out here; inherited type still
     applies) and a closing paragraph goes inline; any other closing block
     (list, table, code) keeps its box, and the rail starts flush below it.
     While streaming the rail has nothing to act on, so it takes no space. */
  .msg.agent > :global(.md) {
    display: contents;
  }
  .msg.agent > :global(.md > p:last-child) {
    display: inline;
  }
  .msg.agent:not(:has(> :global(.md > p:last-child))) :global(.agent-message-meta) {
    margin-left: 0;
  }
  .msg.agent.streaming :global(.agent-message-meta) {
    display: none;
  }
  .notice {
    color: var(--muted);
    font-size: var(--text-sm);
    text-align: center;
    padding: 6px 0;
  }
  .notice.error {
    color: var(--err);
  }
  .runtime-error {
    max-width: 64ch;
    margin: 24px auto 8px;
    padding: 16px 20px;
    border: 1px solid var(--edge);
    border-radius: 10px;
    text-align: left;
  }
  .runtime-error p {
    color: var(--fg);
    margin: 8px 0 12px;
  }
  .runtime-error details {
    color: var(--muted);
  }
  .runtime-error summary {
    cursor: pointer;
  }
  .runtime-error pre {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: var(--text-xs);
  }
  /* "✳ 9m 43s · 12.3k tokens · 1 running task · Running tools…" — every
     part journal-derived (turn start, turn_tokens, the live sets, the phase),
     separated by quiet middots so the line reads as one sentence. */
  .status-row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px 6px;
    padding: 8px 0 2px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  /* A turn the agent started itself: a quiet marker above its reply. */
  .wake {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 0;
    color: var(--activity-fg, var(--muted));
    font-size: var(--text-xs);
    line-height: 1.4;
  }
  .status-part {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .status-row > :is(.status-elapsed, .status-part) + :is(.status-part, .status-label)::before {
    content: "·";
    margin-right: 6px;
    color: color-mix(in srgb, var(--muted) 60%, transparent);
  }
  .status-spark {
    display: inline-flex;
    animation: spark-pulse 1.6s ease-in-out infinite;
  }
  .status-label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    animation: label-pulse 1.6s ease-in-out infinite;
  }
  /* Hidden document: nobody sees the shimmer — stop burning frames for the
     length of an agent turn (the html.app-hidden contract; see app.css). */
  :global(html.app-hidden) .status-spark,
  :global(html.app-hidden) .status-label,
  :global(html.app-hidden) .compaction-progress > span {
    animation-play-state: paused;
  }
  /* Ellipsis that breathes with the spark, without layout shift. */
  .status-label::after {
    content: "…";
  }
  /* Elapsed counter: a still, muted number beside the pulsing label (only
     appears past 5s). No animation — reduced-motion safe by construction. */
  .status-elapsed {
    font-family: var(--mono, monospace);
    font-variant-numeric: tabular-nums;
    color: color-mix(in srgb, var(--muted) 80%, transparent);
  }
  /* Compaction has no honest percentage on either agent wire. Show a bounded
     indeterminate track instead of inventing one; start/completion still come
     from journaled protocol events, so reconnect never resets the truth. */
  .compaction-progress {
    position: relative;
    width: clamp(48px, 12vw, 112px);
    height: 2px;
    overflow: hidden;
    border-radius: 999px;
    background: color-mix(in srgb, var(--edge) 65%, transparent);
  }
  .compaction-progress > span {
    position: absolute;
    inset-block: 0;
    width: 42%;
    border-radius: inherit;
    background: var(--accent);
    animation: compact-sweep 1.35s ease-in-out infinite;
  }
  @keyframes compact-sweep {
    from {
      transform: translateX(-110%);
    }
    to {
      transform: translateX(340%);
    }
  }
  @keyframes spark-pulse {
    0%,
    100% {
      opacity: 1;
      transform: scale(1);
    }
    50% {
      opacity: 0.45;
      transform: scale(0.88);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .status-spark,
    .status-label,
    .compaction-progress > span {
      animation: none;
    }
    .compaction-progress > span {
      inset-inline-start: 29%;
    }
  }
  /* The strip chrome (border, padding, collapse header, bounded scroll) now
     comes from the shared WorkTray, so only the rows are styled here. */
  .plan-fold {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 1px 0 3px;
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    text-align: left;
    cursor: pointer;
  }
  .plan-row {
    display: flex;
    gap: 8px;
    padding: 1px 0;
  }
  .plan-row.done {
    color: var(--muted);
  }
  .plan-mark {
    color: var(--accent);
    flex: none;
  }
  /* Blocked reads as "waiting", not "active": the mark drops to muted so a
     ⊘ can't be mistaken for progress at a glance. */
  .plan-row.blocked .plan-mark {
    color: var(--muted);
  }
  .plan-body {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .plan-line {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
  }
  /* Every text span clips rather than wraps — the panel is a glance surface,
     and an agent subject can be arbitrarily long. */
  .plan-subject,
  .plan-desc {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .plan-owner,
  .plan-blocked {
    flex: none;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  /* Mixed toward --fg, not --muted: accent-over-muted lands near 3.5:1 on the
     light background, too weak for an 11px chip. Same blend as .plan-active. */
  .plan-owner {
    color: color-mix(in srgb, var(--accent) 70%, var(--fg));
  }
  .plan-desc {
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .bubble-row {
    display: flex;
    align-items: center;
    gap: 6px;
    max-width: 100%;
  }
  .message-action {
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-md);
    cursor: pointer;
    padding: 0 2px;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }
  .msg.user:hover .message-action,
  .message-action:focus-visible {
    opacity: 1;
  }
  .message-action:hover {
    color: var(--accent);
  }
  /* The ✕ on a queued bubble: pull it back before the agent sees it. Quiet by
     default (mirrors .rewind-btn), reveals on hover/focus of the pending row,
     and warms to --err on hover since it discards. */
  .cancel-btn {
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    line-height: 1;
    cursor: pointer;
    padding: 0 2px;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }
  .pending-msg:hover .cancel-btn,
  .cancel-btn:focus-visible {
    opacity: 1;
  }
  .cancel-btn:hover {
    color: var(--err);
  }
  .branch-line {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    min-width: 0;
    padding-top: 4px;
  }
  .branch-slot {
    display: inline-flex;
    min-width: 0;
    flex: 0 1 auto;
  }
  .branch-line-end {
    display: inline-flex;
    justify-content: flex-end;
    min-width: 0;
    flex: 0 1 auto;
    margin-left: auto;
  }
  .branch-line-end:empty {
    display: none;
  }
  .suggestion-row {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 4px 10px 0;
    animation: rise 0.15s ease; /* @keyframes rise lives in app.css */
  }
  .suggestion {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    background: none;
    border: 1px dashed color-mix(in srgb, var(--accent) 40%, var(--edge));
    border-radius: 999px;
    padding: 2px 12px;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
    transition:
      color 0.12s ease,
      border-color 0.12s ease;
  }
  .suggestion:hover {
    color: var(--fg);
    border-color: var(--accent);
  }
  .suggestion-mark {
    color: var(--accent);
    flex: none;
  }
  .suggestion-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .suggestion-x {
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    padding: 0 4px;
    font-size: var(--text-md);
    flex: none;
  }
  .suggestion-x:hover {
    color: var(--fg);
  }
  .jump {
    position: sticky;
    bottom: 4px;
    align-self: center;
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    /* No net height in the column (its own plus the column's gap): it
       appears and goes as the reader leaves and reaches the bottom, and a
       row coming and going there clamped scrollTop — a snap on every
       arrival at the live edge. */
    margin-top: calc(-24px - var(--row-gap));
    padding: 0;
    font: inherit;
    font-size: var(--text-lg);
    line-height: 1;
    color: color-mix(in srgb, var(--fg) 40%, transparent);
    background: none;
    border: none;
    text-shadow:
      0 1px 2px var(--term-bg),
      0 0 6px var(--term-bg);
    cursor: pointer;
    transition:
      color 0.12s ease,
      transform 0.12s ease;
  }
  .jump:hover,
  .jump:focus-visible {
    color: color-mix(in srgb, var(--fg) 72%, transparent);
  }
  .jump:hover {
    transform: translateY(1px);
  }
  .jump:focus-visible {
    outline: 1px solid var(--focus-ring);
    outline-offset: 2px;
  }
</style>

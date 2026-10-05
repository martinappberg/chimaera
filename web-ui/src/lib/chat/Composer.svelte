<script lang="ts">
  import { tick, untrack } from "svelte";
  import { paneTabHasKeyboardFocus } from "../shared/tabNavigation";
  import { fsQuickOpen, parentName, type QuickOpenEntry } from "../previews/files";
  import FileIcon from "../shared/FileIcon.svelte";
  import FolderIcon from "../shared/FolderIcon.svelte";
  import { composeAgentPathReference } from "../shared/reference";
  import {
    composerHeightForContent,
    type ManualComposerHeight,
  } from "./composerHeight";
  import AttachmentStrip from "./AttachmentStrip.svelte";
  import ComposerMentions from "./ComposerMentions.svelte";
  import { composerInputKey, composerKey, decorationRuns, fillComposerDraft, type NativeComposerKey } from "./nativeComposer";
  import { uiStyle, type UiRecord } from "./nativeUi";
  import ImagePreview from "./ImagePreview.svelte";
  import { registerComposer, registerComposerAttach, registerComposerReturn } from "./composerBus";
  import {
    attachmentSrc,
    imageToAttachment,
    IMAGE_MAX_ATTACHMENTS,
    type ImageAttachment,
  } from "./images";
  import { loadDraft, saveDraft } from "./drafts";
  import { getSetting, setSetting } from "../settings/store.svelte";
  import { contextMenu, type ContextMenuEntry } from "../shared/contextMenu.svelte";
  import { keyHintSuffix, matchAction } from "../shared/keybindings";
  import { displayChord, matchChord, type ParsedChord } from "../shared/keys";
  import { Dictation, dictationParts, hostCanDictate, joinParts } from "./voice.svelte";
  import { CaptureError, listMicrophones } from "./voiceCapture";
  import VoiceMeter from "./VoiceMeter.svelte";
  import { uploadChips } from "./uploadChips";
  import {
    collapseUploadMentions,
    expandUploadMentions,
    tokenSpans,
    type UploadTokens,
  } from "./uploadTokens";
  import {
    draftWithInsert,
    slashChoices as choicesForSlash,
    slashContextAt,
    type ComposerCommand,
    type SlashChoice,
  } from "./composer";

  export interface TerminalOption {
    id: string;
    name: string;
  }

  interface Props {
    /** Registers this composer for workbench insert flows (references,
     *  provenance tags) when set. */
    sessionId: string | null;
    /** The mounting view's token, so an insert meant for this view (a quote
     *  of its own transcript) finds it when the chat is mounted twice. */
    view?: object;
    running: boolean;
    disabled: boolean;
    /** Why the composer is disabled, in plain words; defaults to an ended chat. */
    disabledReason?: string;
    slashCommands: ComposerCommand[];
    /** Quick-open scope for @-mentions; null disables them. */
    workspaceId: string | null;
    /** Workspace terminals offered by @term: mentions (linked-terminal
     *  grants — the daemon's UserPromptSubmit hook resolves them). */
    terminals: TerminalOption[];
    focused: boolean;
    /** False while the owning retained chat tab is hidden. The draft remains
     *  mounted, but invisible running chrome must stay still. */
    visible?: boolean;
    /** Returns whether the message was accepted (false during reconnect, so
     *  the composer keeps the draft instead of losing it). `afterTurn`: the
     *  after-this-turn chord sent it — hold it until the running turn ends
     *  rather than have the agent read it at its next step. */
    onSubmit(text: string, images: ImageAttachment[], afterTurn?: boolean): boolean;
    onInterrupt(): void;
    /** Shift+Tab: advance to the next permission mode (agent-TUI parity). */
    onCycleMode(): void;
    /** Intercept a dialog-only slash command with native UI. True = handled. */
    onSlash(name: string, args?: string): boolean;
    /** Non-empty draft/attachment state. The transcript uses this to suspend
     *  live-following while the user is actively composing. */
    onDraftState(active: boolean): void;
    /** Words dictation should favor (project and agent names). */
    voiceTerms?: string[];
    /** The pictures attached here changed (or the composer just mounted):
     *  a returned message that was waiting for room may fit now. */
    onReturnRoom?: () => void;
    imageInput?: boolean;
    onNativeEdit?: (request: UiRecord) => Promise<UiRecord>;
  }

  let {
    sessionId,
    view,
    running,
    disabled,
    disabledReason = undefined,
    slashCommands,
    workspaceId,
    terminals,
    focused,
    visible = true,
    onSubmit,
    onInterrupt,
    onCycleMode,
    onSlash,
    onDraftState,
    voiceTerms = [],
    onReturnRoom = undefined,
    imageInput = true,
    onNativeEdit,
  }: Props = $props();

  const uid = $props.id();
  const COMPOSER_MIN_HEIGHT = 38;
  const COMPOSER_MAX_HEIGHT = 352;

  // The parent keys ChatView (and so this composer) per session — one
  // instance, one session. The bounded pane live-set can still evict/remount
  // it, so the draft must live in the session-keyed module store, not here.
  // svelte-ignore state_referenced_locally
  const savedDraft = sessionId !== null ? loadDraft(sessionId) : { text: "", images: [] };
  /** A dropped file's mention reads as its name here, where it sits in the
   *  sentence, and edits as one unit (`uploadTokens.ts`, `uploadChips.ts`);
   *  the whole mention goes back into every text that leaves the composer —
   *  the send, a copy, and the saved draft. */
  const uploadTokens: UploadTokens = new Map();
  let draft = $state(collapseUploadMentions(savedDraft.text, uploadTokens));
  let images = $state<ImageAttachment[]>(savedDraft.images.slice(0, IMAGE_MAX_ATTACHMENTS));
  let attachmentError = $state<string | null>(null);

  /** The attachment shown large (ImagePreview), by index. */
  let previewing = $state<number | null>(null);

  function removeImage(index: number): void {
    images = images.filter((_, j) => j !== index);
    attachmentError = null;
  }

  /** Back to typing where the preview was opened from. */
  function closePreview(): void {
    previewing = null;
    el?.focus();
  }

  function addImage(image: ImageAttachment): boolean {
    if (!imageInput) {
      attachmentError = "this agent does not support images in chat";
      return false;
    }
    if (images.length >= IMAGE_MAX_ATTACHMENTS) {
      attachmentError = `maximum ${IMAGE_MAX_ATTACHMENTS} images per message`;
      return false;
    }
    images.push(image);
    attachmentError = null;
    return true;
  }

  // Write-through persistence: every draft/attachment change (typing, paste,
  // popover picks, the post-send clear) lands in the session's draft slot.
  // snapshot, not the proxy: it tracks in-place pushes (onPaste mutates) and
  // stores plain data. Reads $state, writes the module map — no read+write
  // loop, no timer.
  $effect(() => {
    const text = expandUploadMentions(draft, uploadTokens);
    const imgs = $state.snapshot(images);
    if (sessionId === null) return;
    saveDraft(sessionId, text, imgs);
  });
  let el = $state<HTMLTextAreaElement | null>(null);
  let nativeSuggestion = $state("");
  let nativeDecorations = $state<unknown>([]);
  let decoratedMirror = $state<HTMLDivElement | null>(null);
  let nativeRevision = 0;
  let lastNativeText: string | null = null;
  let nativeTimer: ReturnType<typeof setTimeout> | null = null;
  let pendingNativeKey: NativeComposerKey | undefined;
  let nativeComposing = false;
  const decoratedRuns = $derived(decorationRuns(draft, nativeDecorations));
  const hasNativeDecorations = $derived(Array.isArray(nativeDecorations) && nativeDecorations.length > 0);
  export function nativeRead(): { text: string; cursor: number } { return { text: draft, cursor: el?.selectionStart ?? caret }; }
  export function nativeFill(text: string, mode: string, decorations?: unknown): boolean {
    if (!el || disabled || !visible || spoken !== null || nativeComposing || text.length > 64_000) return false;
    const next = fillComposerDraft(draft, text, mode, el.selectionStart, el.selectionEnd, decorations);
    if (!next) return false;
    draft = next.text; caret = next.cursor; nativeDecorations = next.decorations;
    nativeSuggestion = "";
    // Even an identical-text fill supersedes a pending person's edit response.
    editNative("app", true);
    const revision = nativeRevision;
    void tick().then(() => { if (revision === nativeRevision && draft === next.text) el?.setSelectionRange(next.cursor, next.cursor); });
    return true;
  }
  export function nativeSuggest(text: string): boolean {
    if (disabled || !visible || running || draft.length || images.length || text.length > 64_000) return false;
    nativeSuggestion = text; return true;
  }
  function editNative(by: "person" | "app", preserveDecorations = false, key?: NativeComposerKey): void {
    const revision = ++nativeRevision;
    if (nativeTimer) { clearTimeout(nativeTimer); nativeTimer = null; }
    if (!preserveDecorations) nativeDecorations = [];
    if (!onNativeEdit || !visible || disabled || nativeComposing) return;
    const text = draft, cursor = by === "app" ? caret : el?.selectionStart ?? caret;
    const selectionEnd = el?.selectionEnd ?? cursor;
    lastNativeText = text;
    nativeTimer = setTimeout(() => {
      nativeTimer = null;
      void onNativeEdit!({ subtype: "ui_prompt_edit", text, cursor, by, ...(key ? { key } : {}) }).then((result) => {
        // App edits only synchronize Claude's previous-box state. Its ack has
        // no decoration runs and must not erase the fill the app just painted.
        if (by === "app") return;
        if (revision !== nativeRevision || draft !== text || result.superseded || typeof result.text !== "string") return;
        // Arrows and selection change the person's intent without editing the
        // draft. An old hook must not put their caret back where it used to be.
        if (el && (el.selectionStart !== cursor || el.selectionEnd !== selectionEnd)) return;
        if (result.text.length > 64_000) return;
        lastNativeText = result.text; draft = result.text;
        caret = Math.max(0, Math.min(draft.length, Number(result.cursor) || 0));
        nativeDecorations = result.decorations ?? [];
        void tick().then(() => { if (el && document.activeElement === el) el.setSelectionRange(caret, caret); });
      }).catch(() => {});
    }, by === "person" ? 35 : 0);
  }
  $effect(() => { const text = draft; if (onNativeEdit && visible) untrack(() => { if (text !== lastNativeText) editNative("app"); }); });
  $effect(() => () => { if (nativeTimer) clearTimeout(nativeTimer); ++nativeRevision; });
  $effect(() => { if (draft.length || running) nativeSuggestion = ""; });
  // svelte-ignore state_referenced_locally
  let caret = $state(draft.length);
  let paneHeight = $state(0);
  /** Null follows content; an object remembers the height chosen with the
   *  top-edge grip and how much content it held at that moment. */
  let manualHeight = $state<ManualComposerHeight | null>(null);
  let currentHeight = $state(COMPOSER_MIN_HEIGHT);
  let resizing = $state(false);
  let resizeStartY = 0;
  let resizeStartHeight = 0;
  let resizeStartContentHeight = 0;
  let resizeMoved = false;
  let selected = $state(0);
  let fileMatches = $state<QuickOpenEntry[]>([]);
  /** The @token the current matches were computed FOR (position + text) —
   *  pick-time caret state is unreliable once a popover button takes focus. */
  let fileToken = $state<{ start: number; text: string } | null>(null);
  let quickOpenTimer: ReturnType<typeof setTimeout> | null = null;

  $effect(() => {
    onDraftState(draft.trim().length > 0 || images.length > 0 || resizing);
  });

  $effect(() => {
    if (focused && !paneTabHasKeyboardFocus()) el?.focus();
  });

  // Workbench splits resize without changing the browser viewport. Observe
  // the owning chat pane so both auto and manual heights stay inside the live
  // reading area; disconnect on remount/unmount per the runes teardown rule.
  $effect(() => {
    const t = el;
    if (t === null) return;
    const pane = t.closest<HTMLElement>(".chat");
    if (pane === null) return;
    const measure = () => (paneHeight = pane.clientHeight);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(pane);
    return () => observer.disconnect();
  });

  function maxComposerHeight(): number {
    // A composer should help with a long prompt without swallowing the chat.
    // Measure the pane rather than the window because workbench splits can be
    // much shorter than the app viewport.
    const measuredHeight =
      paneHeight || el?.closest<HTMLElement>(".chat")?.clientHeight || window.innerHeight;
    return Math.max(
      COMPOSER_MIN_HEIGHT,
      Math.min(COMPOSER_MAX_HEIGHT, Math.floor(measuredHeight * 0.42)),
    );
  }

  function clampComposerHeight(height: number): number {
    return Math.max(COMPOSER_MIN_HEIGHT, Math.min(maxComposerHeight(), height));
  }

  /** Measure the content independently of the current inline height. */
  function naturalComposerHeight(t: HTMLTextAreaElement): number {
    const previousHeight = t.style.height;
    t.style.height = "auto";
    // +2: 1px border × 2, box-sizing is border-box.
    const height = t.scrollHeight + 2;
    t.style.height = previousHeight;
    return height;
  }

  function chooseComposerHeight(height: number | null, contentHeight?: number) {
    manualHeight =
      height === null
        ? null
        : {
            height: clampComposerHeight(height),
            contentHeight:
              contentHeight ??
              (el === null ? COMPOSER_MIN_HEIGHT : naturalComposerHeight(el)),
          };
  }

  /** Every successfully consumed draft returns the next input to content-fit,
   *  whether it became an agent turn or a native slash-command action. */
  function clearSubmittedDraft() {
    draft = "";
    caret = 0;
    chooseComposerHeight(null);
  }

  // Autosize from rendered height, not "\n" count — soft-wrapped pastes
  // grow the box too. A manual resize reserves (or contracts) space without
  // locking the box: further content growth still expands it to the pane cap.
  $effect(() => {
    void draft;
    const t = el;
    if (t === null) return;
    const chosen = manualHeight;
    const contentHeight = naturalComposerHeight(t);
    t.style.height = `${composerHeightForContent(
      contentHeight,
      chosen,
      COMPOSER_MIN_HEIGHT,
      maxComposerHeight(),
    )}px`;
    currentHeight = t.getBoundingClientRect().height;
  });

  /** The grip rides the text area's top edge. Dragging upward increases the
   *  height, matching the bottom-anchored composer; pointer capture avoids
   *  document listeners and keeps the drag alive outside the narrow handle. */
  function startResize(e: PointerEvent) {
    if (e.button !== 0 || el === null) return;
    e.preventDefault();
    resizeStartY = e.clientY;
    resizeStartHeight = el.getBoundingClientRect().height;
    resizeStartContentHeight = naturalComposerHeight(el);
    resizeMoved = false;
    resizing = true;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function moveResize(e: PointerEvent) {
    if (!resizing) return;
    if (Math.abs(resizeStartY - e.clientY) > 2) resizeMoved = true;
    chooseComposerHeight(
      resizeStartHeight + resizeStartY - e.clientY,
      resizeStartContentHeight,
    );
  }

  function endResize(e: PointerEvent) {
    if (!resizing) return;
    resizing = false;
    const handle = e.currentTarget as HTMLElement;
    if (handle.hasPointerCapture(e.pointerId)) handle.releasePointerCapture(e.pointerId);
  }

  /** A click is the discoverable companion to the precision drag: expand an
   *  auto-sized draft to the pane cap, or return a manually sized box to fit.
   *  Pointerup also emits click after a drag, so consume that synthetic click
   *  without undoing the height the user just chose. */
  function toggleComposerHeight() {
    if (resizeMoved) {
      resizeMoved = false;
      return;
    }
    chooseComposerHeight(manualHeight === null ? maxComposerHeight() : null);
  }

  function resizeWithKeyboard(e: KeyboardEvent) {
    if (el === null) return;
    if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      e.preventDefault();
      const delta = e.key === "ArrowUp" ? 24 : -24;
      chooseComposerHeight(el.getBoundingClientRect().height + delta);
    } else if (e.key === "Home") {
      e.preventDefault();
      chooseComposerHeight(null);
    }
  }

  // Workbench insert flows (selection references, provenance tags, quoted
  // passages) land in the draft exactly like they would type into a PTY's
  // input — appended, never submitted.
  $effect(() => {
    if (sessionId === null) return;
    return registerComposer(
      sessionId,
      (inserted, placement) => {
        const text = collapseUploadMentions(inserted, uploadTokens, draft);
        draft = draftWithInsert(draft, text, placement);
        focusAt(draft.length);
      },
      view,
    );
  });

  // A message that did not arrive comes back here by itself: above whatever
  // is being written, with its pictures, and without taking focus. A composer
  // that has focus keeps its caret where it was in the text being written
  // (setting the value would throw it to the end).
  $effect(() => {
    if (sessionId === null) return;
    return registerComposerReturn(sessionId, {
      room: () => IMAGE_MAX_ATTACHMENTS - images.length,
      take: (returned) => {
        const before = draft.length;
        const focused = el !== null && document.activeElement === el;
        const start = el?.selectionStart ?? before;
        const end = el?.selectionEnd ?? start;
        if (returned.text.length > 0) draft = draftWithInsert(draft, returned.text, "above");
        images.push(...returned.images);
        const shift = draft.length - before;
        caret += shift;
        if (focused) void tick().then(() => el?.setSelectionRange(start + shift, end + shift));
      },
    }, view);
  });
  // The host keeps a returned message whose pictures do not fit until there
  // is room: tell it whenever the number attached changes (and at mount).
  $effect(() => {
    void images.length;
    untrack(() => onReturnRoom?.());
  });

  // Workbench attach flow (an image dropped from the OS desktop onto this
  // chat pane): rides the same attachment state as clipboard paste.
  $effect(() => {
    if (sessionId === null) return;
    return registerComposerAttach(sessionId, (image) => {
      addImage(image);
      el?.focus();
    });
  });

  // --- voice dictation ------------------------------------------------------------
  // The mic button IS voice mode in chat: click to talk, click again when done
  // (Enter: stop and send; Esc: discard), or the Dictate chord while this
  // composer has focus. Typing is never taken over — the hold-Space
  // push-to-talk is a terminal's answer to having no buttons, and stays with
  // the agents' own TUIs. Right-click picks the microphone.
  //
  // The words stream INTO the draft at the caret as they're heard, so a long
  // dictation fills (and grows) the box like typing would. While it runs the
  // textarea is read-only with its text transparent, and `ghost` — a mirror
  // with the textarea's exact box, font and wrapping — draws the same text:
  // the user's own in full, settled words slightly dimmed, forming words
  // dimmer. Stopping just drops the mirror; Esc restores the draft as it was.
  const dictation = new Dictation();
  const voiceOn = $derived(getSetting("chat.voice") && !disabled && hostCanDictate());
  /** The draft around a dictation in progress, split where it started. */
  let dictating = $state<{ original: string; before: string; after: string } | null>(null);
  let ghost = $state<HTMLDivElement | null>(null);
  const spoken = $derived(
    dictating === null
      ? null
      : dictationParts(dictating.before, dictating.after, dictation.finals, dictation.interim),
  );
  /** Right padding for the stop button, send button and waveform. */
  const DICTATING_PAD = 96;

  // The draft follows the words. Reads the transcript, writes only `draft`.
  $effect(() => {
    if (spoken === null) return;
    draft = joinParts(spoken);
    void tick().then(followDictation);
  });

  /** Keep the newest words in view, the mirror scrolled with the textarea,
   *  and its padding matched to the textarea's (a classic scrollbar takes
   *  width from the text box). */
  function followDictation() {
    const t = el;
    const g = ghost;
    if (t === null || g === null) return;
    if (dictating !== null && dictating.after === "") t.scrollTop = t.scrollHeight;
    const scrollbar = Math.max(0, t.offsetWidth - t.clientWidth - 2);
    g.style.paddingRight = `${DICTATING_PAD + scrollbar}px`;
    g.scrollTop = t.scrollTop;
  }

  async function startDictation() {
    // An unfocused textarea's selection is wherever it was last left (0 for
    // a restored draft): words then go at the end, like picking up a thought.
    const focused = el !== null && document.activeElement === el;
    const start = focused ? el!.selectionStart : draft.length;
    const end = focused ? el!.selectionEnd : start;
    dictating = { original: draft, before: draft.slice(0, start), after: draft.slice(end) };
    if (!(await dictation.start({ keyterms: voiceTerms }))) restoreDraft();
  }

  /** The words are final: they stay in the draft as ordinary text. */
  function settleDictation() {
    const d = dictating;
    if (d === null) return;
    const parts = dictationParts(d.before, d.after, dictation.finals, "");
    draft = joinParts(parts);
    dictating = null;
    placeCaret(parts.before.length + parts.finals.length);
  }

  /** Discard: the draft exactly as it was before the mic opened. */
  function restoreDraft() {
    const d = dictating;
    if (d === null) return;
    dictating = null;
    draft = d.original;
    placeCaret(d.before.length);
  }

  /** The caret after dictation — focused only when the composer still has
   *  focus: a recording that ends in a hidden tab, or while the user works in
   *  another pane, must not pull focus back here. */
  function placeCaret(position: number) {
    if (el !== null && document.activeElement === el) focusAt(position);
    else caret = position;
  }

  /** Stop and keep the words — then send, for Enter (or the after-turn
   *  chord, which keeps its meaning through the stop). */
  async function finishDictation(send: boolean, afterTurn = false) {
    const text = await dictation.finish();
    settleDictation();
    if (send && text !== null && text.length > 0) {
      await tick();
      submit(afterTurn);
    }
  }

  function cancelDictation() {
    restoreDraft();
    dictation.cancel();
  }

  function toggleDictation() {
    if (!dictation.active) void startDictation();
    else if (dictation.state !== "finishing") void finishDictation(false);
  }

  /** Dictation's keys: the Dictate chord, and while recording Esc (discard)
   *  and Enter (stop and send). True = consumed. */
  function voiceKey(e: KeyboardEvent): boolean {
    if (voiceOn && matchAction(e)?.id === "dictate") {
      e.preventDefault();
      toggleDictation();
      return true;
    }
    if (!dictation.active) return false;
    if (e.key === "Escape") {
      e.preventDefault();
      cancelDictation();
      return true;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      // The after-turn chord keeps its meaning through the stop.
      const afterTurn = matchChord(e, AFTER_TURN_CHORD) !== null && matchAction(e) === null;
      if (dictation.state !== "finishing") void finishDictation(true, afterTurn);
      return true;
    }
    return false;
  }

  /** Right-click the mic: this machine's microphones, the chosen one checked. */
  async function openMicMenu(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    const { clientX: x, clientY: y } = e;
    const chosen = getSetting("chat.voiceMicrophone");
    const pick = (name: string) => () => setSetting("chat.voiceMicrophone", name);
    let entries: ContextMenuEntry[];
    try {
      const mics = await listMicrophones();
      entries = [
        { label: "System default", checked: chosen === "", onSelect: pick("") },
        ...(mics.length > 0 ? (["separator"] as const) : []),
        ...mics.map((m) => ({ label: m.label, checked: m.label === chosen, onSelect: pick(m.label) })),
      ];
      if (chosen !== "" && !mics.some((m) => m.label === chosen)) {
        entries.push({
          label: `${chosen} (not connected)`,
          checked: true,
          disabled: true,
          hint: "dictation uses the system default until it's back",
          onSelect: () => {},
        });
      }
    } catch (err) {
      entries = [
        {
          label: err instanceof CaptureError ? err.message : `Couldn't list microphones: ${String(err)}`,
          disabled: true,
          onSelect: () => {},
        },
      ];
    }
    contextMenu.openAtPoint(x, y, entries);
  }

  // A hidden tab keeps what was said: finish into the draft. Untracked, so the
  // state change this makes doesn't re-run the effect.
  $effect(() => {
    if (visible && !disabled) return;
    untrack(() => {
      if (dictation.active && dictation.state !== "finishing") void finishDictation(false);
    });
  });

  // A recording that ended on its own (the relay failed, the length cap)
  // keeps what was settled.
  $effect(() => {
    if (dictation.active || dictating === null) return;
    untrack(settleDictation);
  });

  // An unmounted composer can't take the words: stop and release the mic.
  $effect(() => () => dictation.cancel());

  $effect(() => {
    if (dictation.error === null || dictation.active) return;
    const timer = setTimeout(() => dictation.clearError(), 8000);
    return () => clearTimeout(timer);
  });

  /** Escape-dismissed slash token text — suppresses the popover for exactly
   *  that token so Escape closes it without clearing a mid-draft message;
   *  typing on (the token text changes) brings it back. */
  let slashDismissed = $state<string | null>(null);

  /** The token under the caret matching `re` — group 1 is the leading boundary,
   *  group 2 the token itself. Shared core of the `/`-command and `@`-mention
   *  scanners. Read from the PRE-focus caret (a popover click steals
   *  selectionStart), so both popovers survive a mouse pick. */
  function caretToken(re: RegExp): { start: number; text: string } | null {
    const at = Math.max(0, Math.min(caret, draft.length));
    const match = re.exec(draft.slice(0, at));
    if (match === null) return null;
    return { start: at - match[2].length, text: match[2] };
  }

  /** Slash discovery follows the live caret, not just draft mutations: moving
   *  back into an existing inline token should reopen its completion menu. */
  const slashContext = $derived.by(() => {
    void draft;
    void caret;
    return slashContextAt(draft, caret, slashCommands);
  });
  const slashMatches = $derived.by(() => {
    const context = slashContext;
    if (context === null || slashKey(context) === slashDismissed) return [];
    return choicesForSlash(context, slashCommands);
  });
  function slashKey(context: NonNullable<typeof slashContext>): string {
    return `${context.kind}:${context.start}:${context.text}`;
  }
  // Forget an Escape-dismissal once its token is edited away (the draft cleared
  // or sent, or the token changed) — otherwise re-typing the same command later
  // stays suppressed for the rest of the session. Settles: after the reset the
  // guard is false.
  $effect(() => {
    if (
      slashDismissed !== null &&
      (slashContext === null || slashKey(slashContext) !== slashDismissed)
    ) {
      slashDismissed = null;
    }
  });

  /** The @token under the caret, if any (mention autocomplete). ":" admits
   *  @term:NAME (linked-terminal grants) alongside file paths. A dropped
   *  file's short form is already a finished mention: nothing to complete. */
  function atToken(): { start: number; text: string } | null {
    if (tokenSpans(draft, uploadTokens).some((s) => s.end === caret)) return null;
    return caretToken(/(^|\s)(@[\w./:-]*)$/);
  }

  // Debounced quick-open lookup for the @token.
  $effect(() => {
    void draft;
    const token = atToken();
    if (token === null || token.text.length < 2 || workspaceId === null) {
      fileToken = null;
      fileMatches = [];
      return;
    }
    fileToken = token;
    // @term: tokens complete against workspace terminals, not files.
    if (token.text.startsWith("@term")) {
      fileMatches = [];
      return;
    }
    if (quickOpenTimer !== null) clearTimeout(quickOpenTimer);
    quickOpenTimer = setTimeout(() => {
      // dirs=true: @-mentions tag folders too, exactly like the agent TUIs.
      void fsQuickOpen(workspaceId, token.text.slice(1), 8, true)
        .then((entries) => {
          // The draft may have moved on while the request flew.
          if (fileToken?.text === token.text) fileMatches = entries;
        })
        .catch(() => (fileMatches = []));
    }, 150);
    // Cancel a pending lookup on teardown (keystroke or unmount) so it can't
    // fire a stray request and write state after the component is destroyed.
    return () => {
      if (quickOpenTimer !== null) {
        clearTimeout(quickOpenTimer);
        quickOpenTimer = null;
      }
    };
  });

  /** @term: mentions — Chimaera's linked-terminal grants. */
  const termMatches = $derived.by(() => {
    const token = fileToken;
    if (token === null || !token.text.startsWith("@term:")) return [];
    const q = token.text.slice(6).toLowerCase();
    return terminals.filter((t) => t.name.toLowerCase().includes(q) || t.id.includes(q)).slice(0, 8);
  });

  const popover = $derived(
    slashMatches.length > 0
      ? "slash"
      : termMatches.length > 0
        ? "term"
        : fileMatches.length > 0
          ? "file"
          : null,
  );
  // Reset the highlighted row whenever the popover kind OR its contents change
  // (a narrowing match list can leave `selected` past the end, and Enter would
  // then index undefined).
  $effect(() => {
    void popover;
    void slashMatches.length;
    void termMatches.length;
    void fileMatches.length;
    selected = 0;
  });

  function focusAt(position: number) {
    caret = position;
    void tick().then(() => {
      el?.focus();
      el?.setSelectionRange(position, position);
    });
  }

  function pickSlash(choice: SlashChoice) {
    const context = slashContext;
    // A slash that IS the whole draft takes the command path: dialog-only
    // commands open native UI (onSlash), the rest complete in place. Argument
    // choices execute too when the slash is the whole draft; inline choices
    // remain prompt text and leave the surrounding message intact.
    const end = context === null ? 0 : context.start + context.text.length;
    const commandStart = context?.kind === "argument" ? context.commandStart : context?.start;
    const wholeDraft =
      context !== null && commandStart === 0 && draft.slice(end).trim() === "";
    if (wholeDraft && onSlash(choice.command.name, choice.option?.value)) {
      clearSubmittedDraft();
      return;
    }
    const replacement =
      choice.option === undefined ? `/${choice.command.name} ` : `${choice.option.value} `;
    if (context === null) {
      draft = replacement;
      focusAt(replacement.length);
    } else {
      draft = `${draft.slice(0, context.start)}${replacement}${draft.slice(end)}`;
      focusAt(context.start + replacement.length);
    }
    slashDismissed = null;
  }

  function pickFile(entry: QuickOpenEntry) {
    // Directories mention with a trailing slash (the TUI's own convention —
    // it also reads unambiguously as "this folder" in the prompt); a spaced
    // path takes claude's quoted form, like a drag-to-reference drop.
    const rel = entry.kind === "dir" ? `${entry.rel}/` : entry.rel;
    const mention = composeAgentPathReference(rel);
    // A workspace file written exactly like a dropped file's short form
    // (`@data.csv` beside a dropped data.csv) would be taken for the drop:
    // name it from the workspace root instead.
    replaceToken(uploadTokens.has(mention.trim()) ? composeAgentPathReference(`./${rel}`) : mention);
  }

  function pickTerm(t: TerminalOption) {
    // The daemon's mention resolver tokenizes on whitespace: a spaced name
    // can only be granted by id.
    const handle = /^\S+$/.test(t.name) ? t.name : t.id;
    replaceToken(`@term:${handle} `);
  }

  function replaceToken(replacement: string) {
    const token = fileToken;
    if (token === null) return;
    draft = `${draft.slice(0, token.start)}${replacement}${draft.slice(
      token.start + token.text.length,
    )}`;
    fileToken = null;
    fileMatches = [];
    focusAt(token.start + replacement.length);
  }

  function trackCaret() {
    if (el !== null) caret = el.selectionStart;
  }

  function submit(afterTurn = false) {
    // The send button mid-dictation means "stop and send": the words settle
    // first (finishDictation then calls back here).
    if (dictating !== null) {
      if (dictation.state !== "finishing") void finishDictation(true, afterTurn);
      return;
    }
    const text = expandUploadMentions(draft, uploadTokens).trim();
    if (text.length === 0 && images.length === 0) return;
    // Dialog-only slash commands get native UI, not a dead-end CLI reply;
    // arguments ride along ("/effort high"). Unhandled names fall through
    // to the CLI as ordinary prompt text.
    if (text.startsWith("/")) {
      const [name, ...rest] = text.slice(1).split(/\s+/);
      if (onSlash(name, rest.join(" "))) {
        clearSubmittedDraft();
        return;
      }
    }
    // Only clear the draft if the send was actually accepted — during a
    // reconnect window the socket is not OPEN and the message would otherwise
    // vanish silently.
    if (onSubmit(text, images, afterTurn)) {
      clearSubmittedDraft();
      images = [];
    }
  }

  // Send after this turn: ⌥↩ / Alt+Enter — concrete modifiers, not the
  // rebindable `Mod` (the Codex desktop app's ⇧⌘↩ is Zoom Pane here). No
  // default action uses it under any base modifier; one the user binds to
  // it wins (App's capture-phase handler takes it first), and then the
  // chord is neither matched nor advertised.
  const AFTER_TURN_KEYS = "Alt+Enter";
  const AFTER_TURN_CHORD: ParsedChord = {
    meta: false,
    ctrl: false,
    alt: true,
    shift: false,
    key: "Enter",
  };
  /** The chord's label while it is ours ("" while an app action owns it).
   *  matchAction reads the live keys.* settings, so a rebind updates it. */
  const afterTurnHint = $derived(
    matchAction(
      new KeyboardEvent("keydown", {
        key: "Enter",
        code: "Enter",
        altKey: AFTER_TURN_CHORD.alt,
      }),
    ) === null
      ? displayChord(AFTER_TURN_KEYS, "auto")
      : "",
  );

  function onKeydown(e: KeyboardEvent) {
    pendingNativeKey = composerKey(e);
    // IME composition: Enter/arrows select a conversion candidate, not a chat
    // action. WebKit (the Tauri shell's WKWebView) fires the committing Enter
    // after compositionend with isComposing=false but keyCode 229 — check both.
    if (e.isComposing || e.keyCode === 229) return;
    if (e.key === "Tab" && !e.shiftKey && nativeSuggestion && !draft.length && !running) {
      e.preventDefault(); nativeFill(nativeSuggestion, "replace"); return;
    }
    if (voiceKey(e)) return;
    if (popover !== null) {
      const items =
        popover === "slash"
          ? slashMatches.length
          : popover === "term"
            ? termMatches.length
            : fileMatches.length;
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        selected = (selected + (e.key === "ArrowDown" ? 1 : items - 1)) % items;
        return;
      }
      if (e.key === "Tab" || e.key === "Enter") {
        e.preventDefault();
        if (popover === "slash") pickSlash(slashMatches[selected]);
        else if (popover === "term") pickTerm(termMatches[selected]);
        else pickFile(fileMatches[selected]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        if (popover === "slash") {
          // Dismiss the popover in place (never wipe a mid-draft message); a
          // whole-draft "/cmd" still clears, matching the old quick-escape.
          if (
            slashContext?.kind === "command" &&
            slashContext.start === 0 &&
            draft.trim() === slashContext.text
          ) {
            draft = "";
            caret = 0;
          } else {
            slashDismissed = slashContext === null ? null : slashKey(slashContext);
          }
        } else {
          fileMatches = [];
          fileToken = null;
        }
        return;
      }
    }
    // Shift+Tab cycles the permission mode, mirroring the agent TUIs. Only
    // reached with no popover open (there Tab accepts a completion). No-op
    // when the agent exposes no modes.
    if (e.key === "Tab" && e.shiftKey) {
      e.preventDefault();
      onCycleMode();
      return;
    }
    // Also only reached with no popover open — there Enter, with or without
    // modifiers, accepts a completion.
    if (matchChord(e, AFTER_TURN_CHORD) !== null && matchAction(e) === null) {
      e.preventDefault();
      submit(true);
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    } else if (e.key === "Escape" && running) {
      e.preventDefault();
      onInterrupt();
    }
  }

  async function onPaste(e: ClipboardEvent) {
    const items = [...(e.clipboardData?.items ?? [])].filter((i) =>
      i.type.startsWith("image/"),
    );
    if (items.length === 0) return;
    e.preventDefault();
    for (const item of items) {
      const file = item.getAsFile();
      if (file === null) continue;
      // Unreadable/oversized clipboard images resolve null: nothing to attach.
      const attachment = await imageToAttachment(file);
      if (attachment !== null && !addImage(attachment)) break;
    }
  }
</script>

<div class="composer" class:visible>
  {#if popover === "slash"}
    <div class="overlay-surface pop" id="{uid}-pop" role="listbox" aria-label="slash commands">
      {#each slashMatches as choice, i (choice.key)}
        <button
          class="overlay-row pop-row"
          class:sel={i === selected}
          id={`${uid}-opt-${i}`}
          role="option"
          aria-selected={i === selected}
          title={choice.description}
          onclick={() => pickSlash(choice)}
        >
          <span class="pop-name">{choice.label}</span>
          {#if choice.description}
            <span class="pop-desc">{choice.description}</span>
          {/if}
        </button>
      {/each}
    </div>
  {:else if popover === "term"}
    <div class="overlay-surface pop" id="{uid}-pop" role="listbox" aria-label="terminals">
      {#each termMatches as t, i (t.id)}
        <button
          class="overlay-row pop-row"
          class:sel={i === selected}
          id={`${uid}-opt-${i}`}
          role="option"
          aria-selected={i === selected}
          onclick={() => pickTerm(t)}
        >
          <span class="pop-name">@term:{t.name}</span>
          <span class="pop-desc">link this terminal to the agent</span>
        </button>
      {/each}
    </div>
  {:else if popover === "file"}
    <div class="overlay-surface pop" id="{uid}-pop" role="listbox" aria-label="files and folders">
      {#each fileMatches as entry, i (entry.path)}
        <button
          class="overlay-row pop-row"
          class:sel={i === selected}
          id={`${uid}-opt-${i}`}
          role="option"
          aria-selected={i === selected}
          title="@{entry.rel}{entry.kind === 'dir' ? '/' : ''}"
          onclick={() => pickFile(entry)}
        >
          <span class="pop-icon">
            {#if entry.kind === "dir"}
              <FolderIcon size={14} />
            {:else}
              <FileIcon path={entry.path} size={14} />
            {/if}
          </span>
          <span class="pop-name">{entry.name}{entry.kind === "dir" ? "/" : ""}</span>
          <span class="pop-desc">{parentName(entry.rel)}</span>
        </button>
      {/each}
    </div>
  {/if}

  {#if images.length > 0}
    <div class="attachments">
      <AttachmentStrip drafts={images} onRemove={removeImage} onPreview={(i) => (previewing = i)} />
    </div>
  {/if}
  {#if previewing !== null && images[previewing] !== undefined}
    {@const shown = images[previewing]}
    {@const index = previewing}
    <ImagePreview
      src={attachmentSrc(shown)}
      label={shown.label}
      {visible}
      onClose={closePreview}
      onRemove={() => {
        removeImage(index);
        closePreview();
      }}
    />
  {/if}
  {#if attachmentError !== null}
    <div class="attachment-error" role="status">{attachmentError}</div>
  {/if}
  {#if !dictation.active && dictation.error !== null}
    <div class="attachment-error" role="status">{dictation.error}</div>
  {/if}

  <div class="input-row">
    <button
      type="button"
      class="resize-handle"
      class:resizing
      aria-label={`resize message composer, ${Math.round(currentHeight)} pixels high`}
      title="drag to resize · click to expand or fit content"
      onpointerdown={startResize}
      onpointermove={moveResize}
      onpointerup={endResize}
      onpointercancel={endResize}
      onkeydown={resizeWithKeyboard}
      onclick={toggleComposerHeight}
    ></button>
    <ComposerMentions text={draft} tokens={uploadTokens} field={el} quiet={popover !== null} />
    <textarea
      bind:this={el}
      bind:value={draft}
      {@attach uploadChips(uploadTokens, trackCaret)}
      onkeydown={onKeydown}
      onkeyup={() => { trackCaret(); pendingNativeKey = undefined; }}
      onselect={trackCaret}
      onblur={() => (pendingNativeKey = undefined)}
      oncompositionstart={() => {
        nativeComposing = true; pendingNativeKey = undefined;
        // A hook must never rewrite the browser's unfinished IME candidate.
        editNative("person");
      }}
      oncompositionend={(event) => {
        nativeComposing = false; pendingNativeKey = undefined;
        draft = event.currentTarget.value; trackCaret(); editNative("person");
      }}
      oninput={(event) => {
        const key = composerInputKey(pendingNativeKey, event instanceof InputEvent ? event : {}, nativeTimer !== null);
        pendingNativeKey = undefined;
        draft = event.currentTarget.value; trackCaret(); editNative("person", false, key);
      }}
      onpaste={onPaste}
      onscroll={() => {
        if (ghost !== null && el !== null) ghost.scrollTop = el.scrollTop;
        if (decoratedMirror !== null && el !== null) decoratedMirror.scrollTop = el.scrollTop;
      }}
      role="combobox"
      aria-expanded={popover !== null}
      aria-controls="{uid}-pop"
      aria-autocomplete="list"
      aria-activedescendant={popover !== null ? `${uid}-opt-${selected}` : undefined}
      class:voice={voiceOn}
      class:dictating={spoken !== null}
      class:mod-decorated={hasNativeDecorations && spoken === null}
      readonly={spoken !== null}
      placeholder={disabled
        ? (disabledReason ?? "chat ended")
        : spoken !== null
          ? dictation.state === "starting"
            ? "Starting the mic…"
            : "Listening…"
          : running
            ? afterTurnHint !== ""
              ? `add to this turn… (${afterTurnHint} after it ends · Esc to stop)`
              : "add to this turn… (Esc to stop)"
            : nativeSuggestion ? `${nativeSuggestion} (Tab to accept)` : "message the agent… (Enter to send · / commands · @ files)"}
      rows={1}
      {disabled}
    ></textarea>
    {#if hasNativeDecorations && spoken === null}
      <div class="ghost mod-decoration" bind:this={decoratedMirror} aria-hidden="true">{#each decoratedRuns as run, index (index)}<span style={uiStyle(run.props, "Text")}>{run.text}</span>{/each}&#8203;</div>
    {/if}
    {#if spoken !== null}
      <div class="ghost" bind:this={ghost} aria-hidden="true">{spoken.before}<span class="g-final"
          >{spoken.finals}</span
        >{spoken.gap}<span class="g-interim">{spoken.interim}</span>{spoken.after}&#8203;</div>
      <span class="meter-slot">
        <VoiceMeter
          levels={dictation.levels}
          listening={dictation.state === "listening"}
          device={dictation.device}
        />
      </span>
    {/if}
    {#if voiceOn}
      <button
        type="button"
        class="mic"
        class:live={dictation.active}
        aria-pressed={dictation.active}
        aria-label={dictation.active ? "insert the dictated words" : "dictate"}
        title={dictation.active
          ? "Insert — Enter sends, Esc discards"
          : `Dictate${keyHintSuffix("dictate")} — right-click to choose the microphone`}
        disabled={dictation.state === "finishing"}
        onmousedown={(e) => e.preventDefault()}
        onclick={toggleDictation}
        oncontextmenu={openMicMenu}
      >
        {#if dictation.active}
          <!-- Recording: the button is "done" — a stop square, like the
               send button's stop while an agent runs. -->
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <rect x="4" y="4" width="8" height="8" rx="1.5" fill="currentColor" />
          </svg>
        {:else}
          <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
            <rect x="5.5" y="1.75" width="5" height="8" rx="2.5" fill="none" stroke="currentColor" stroke-width="1.5" />
            <path
              d="M3.5 7.5a4.5 4.5 0 0 0 9 0M8 12v2.25"
              fill="none"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
            />
          </svg>
        {/if}
      </button>
    {/if}
    <!-- The action button morphs with the turn: send when idle, stop while the
         agent works. Enter-to-send and Esc-to-stop keep working unchanged;
         mousedown is swallowed so a click never steals the textarea's focus
         (the popovers' pick-time caret logic is focus-fragile). Hidden when
         the chat has ended, matching the disabled textarea. -->
    {#if !disabled}
      {#if running}
        <button
          class="action stop"
          aria-label="interrupt the agent"
          title="interrupt the agent (Esc)"
          onmousedown={(e) => e.preventDefault()}
          onclick={onInterrupt}
        >
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <rect x="4" y="4" width="8" height="8" rx="1.5" fill="currentColor" />
          </svg>
        </button>
      {:else}
        <button
          class="action send"
          aria-label="send message"
          title="send message (Enter)"
          disabled={draft.trim().length === 0 && images.length === 0}
          onmousedown={(e) => e.preventDefault()}
          onclick={() => submit()}
        >
          <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
            <path
              d="M8 12.5v-9M4.5 7 8 3.5 11.5 7"
              fill="none"
              stroke="currentColor"
              stroke-width="1.8"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
      {/if}
    {/if}
  </div>
</div>

<style>
  .composer {
    position: relative;
    flex: none;
    border-top: 1px solid var(--edge);
    padding: 8px 10px;
  }
  /* .overlay-surface / .overlay-row (surface + button reset + hover) live in
     app.css; .pop and .pop-row add only this popover's position and layout. */
  .pop {
    bottom: 100%;
    left: 10px;
    right: 10px;
    margin-bottom: 4px;
    z-index: 10;
  }
  .pop-row {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  /* Higher specificity than the shared .overlay-row:hover, so the keyboard
     highlight (.sel) wins even on a hovered selected row. */
  .pop-row.sel {
    background: var(--row-active);
  }
  .pop-icon {
    flex: none;
    display: inline-flex;
    align-items: center;
  }
  .pop-name {
    font-family: var(--mono, monospace);
    flex: none;
  }
  .pop-desc {
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .attachments {
    padding-bottom: 8px;
  }
  .attachment-error {
    color: var(--warn);
    font-size: var(--text-xs);
    margin: 0 4px 4px;
  }
  /* flex: kills the inline-block baseline gap under the textarea, so the
     bottom-anchored action button measures from the real input edge. */
  .input-row {
    position: relative;
    display: flex;
    /* The mention highlights sit under the textarea's (translucent) fill:
       their layer goes negative inside this row, not under the page. */
    isolation: isolate;
  }
  /* A top-edge grip is the natural geometry for a bottom-anchored composer:
     dragging up makes room, while click toggles expanded/content-fit.
     The line only appears on approach/focus, keeping the idle input quiet. */
  .resize-handle {
    position: absolute;
    z-index: 2;
    top: -5px;
    left: 18px;
    right: 18px;
    height: 10px;
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: ns-resize;
    touch-action: none;
    padding: 0;
    border: none;
    background: none;
    outline: none;
  }
  .resize-handle::after {
    content: "";
    width: 34px;
    height: 2px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--edge) 65%, transparent);
    transition:
      background-color 0.12s ease,
      width 0.12s ease;
  }
  .resize-handle:hover::after,
  .resize-handle:focus-visible::after,
  .resize-handle.resizing::after {
    width: 42px;
    background: color-mix(in srgb, var(--accent) 48%, var(--edge));
  }
  textarea {
    width: 100%;
    resize: none;
    background: color-mix(in srgb, var(--fg) 3%, transparent);
    border: 1px solid var(--edge);
    border-radius: 8px;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-md);
    line-height: var(--chat-line-height, 1.45);
    padding: 7px 38px 7px 10px; /* right clears the 26px action button */
    min-height: 38px;
    max-height: min(42vh, 22rem);
    overflow-y: auto;
    outline: none;
    box-sizing: border-box;
  }
  textarea:focus {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }
  /* Room for the mic beside the action button. */
  textarea.voice {
    padding-right: 66px;
  }
  /* Dictating: the textarea keeps the text (so its size, wrapping and scroll
     are the real ones) but draws none of it; .ghost draws it instead, with
     the spoken words dimmed. Every box and font property must match. */
  textarea.dictating {
    padding-right: 96px; /* DICTATING_PAD: stop + send + waveform */
    color: transparent;
    caret-color: transparent;
  }
  textarea.mod-decorated { color: transparent; caret-color: var(--fg); }
  .ghost.mod-decoration { padding-right: 38px; }
  textarea.voice ~ .ghost.mod-decoration { padding-right: 66px; }
  .ghost {
    position: absolute;
    inset: 0;
    z-index: 1;
    pointer-events: none;
    box-sizing: border-box;
    border: 1px solid transparent;
    padding: 7px 96px 7px 10px;
    font: inherit;
    font-size: var(--text-md);
    line-height: var(--chat-line-height, 1.45);
    color: var(--fg);
    white-space: pre-wrap;
    overflow-wrap: break-word;
    overflow: hidden;
  }
  .g-final {
    color: color-mix(in srgb, var(--fg) 78%, transparent);
  }
  .g-interim {
    color: color-mix(in srgb, var(--fg) 52%, transparent);
  }
  .meter-slot {
    position: absolute;
    right: 67px;
    bottom: 10px;
    z-index: 2;
    display: inline-flex;
    pointer-events: auto;
  }
  textarea:disabled {
    opacity: 0.5;
  }
  /* Bottom-anchored so it stays put while the textarea autosizes upward. */
  .action {
    position: absolute;
    right: 5px;
    bottom: 5px;
    width: 26px;
    height: 26px;
    box-sizing: border-box;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: 1px solid transparent;
    border-radius: 6px;
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease,
      border-color 0.12s ease;
  }
  /* The workbench's active-accent treatment (.chip.on / UpdateToast .primary):
     tinted, not solid — there is no on-accent token, and the composer is quiet
     chrome. */
  .send {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    color: var(--accent);
  }
  .send:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    border-color: var(--accent);
  }
  .send:disabled {
    background: none;
    border-color: color-mix(in srgb, var(--edge) 70%, transparent);
    color: var(--muted);
    opacity: 0.55;
    cursor: default;
  }
  /* Quiet until recording: then it takes the recording red. */
  .mic {
    position: absolute;
    right: 35px;
    bottom: 5px;
    width: 26px;
    height: 26px;
    box-sizing: border-box;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: 1px solid transparent;
    border-radius: 6px;
    background: none;
    color: var(--muted);
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease,
      border-color 0.12s ease;
  }
  .mic:hover:not(:disabled) {
    color: var(--fg);
    background: var(--row-hover);
  }
  .mic.live {
    color: var(--err);
    background: color-mix(in srgb, var(--err) 12%, transparent);
    border-color: color-mix(in srgb, var(--err) 45%, var(--edge));
  }
  .mic:disabled {
    cursor: default;
    opacity: 0.6;
  }
  .stop {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    color: var(--accent);
  }
  /* Faint breathing ring while the agent works — presence, not alarm. A
     STATIC halo on a pseudo-element breathed via opacity: animating
     box-shadow repaints every frame for the whole turn; opacity composites. */
  .stop::after {
    content: "";
    position: absolute;
    inset: -1px;
    border-radius: 7px; /* .action's 6px, outside its 1px border */
    pointer-events: none;
    box-shadow: 0 0 0 4px color-mix(in srgb, var(--accent) 8%, transparent);
    opacity: 0;
    animation: stop-breathe 1.8s ease-in-out infinite;
  }
  .composer:not(.visible) .stop::after {
    animation: none;
  }
  /* Hidden document: nobody sees the breath — stop burning frames (the
     html.app-hidden contract; see app.css). */
  :global(html.app-hidden) .stop::after {
    animation-play-state: paused;
  }
  .stop:hover {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    border-color: var(--accent);
  }
  @keyframes stop-breathe {
    50% {
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .stop::after {
      animation: none;
    }
  }
</style>

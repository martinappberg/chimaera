<script lang="ts">
  import { tick } from "svelte";
  import { fsQuickOpen, parentName, type QuickOpenEntry } from "../previews/files";
  import FileIcon from "../shared/FileIcon.svelte";
  import FolderIcon from "../shared/FolderIcon.svelte";
  import {
    composerHeightForContent,
    type ManualComposerHeight,
  } from "./composerHeight";
  import { registerComposer, registerComposerAttach } from "./composerBus";
  import {
    imageToAttachment,
    IMAGE_MAX_ATTACHMENTS,
    type ImageAttachment,
  } from "./images";
  import { loadDraft, saveDraft } from "./drafts";
  import {
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
    running: boolean;
    disabled: boolean;
    slashCommands: ComposerCommand[];
    /** Quick-open scope for @-mentions; null disables them. */
    workspaceId: string | null;
    /** Workspace terminals offered by @term: mentions (linked-terminal
     *  grants — the daemon's UserPromptSubmit hook resolves them). */
    terminals: TerminalOption[];
    focused: boolean;
    /** Returns whether the message was accepted (false during reconnect, so
     *  the composer keeps the draft instead of losing it). */
    onSubmit(text: string, images: ImageAttachment[]): boolean;
    onInterrupt(): void;
    /** Shift+Tab: advance to the next permission mode (agent-TUI parity). */
    onCycleMode(): void;
    /** Intercept a dialog-only slash command with native UI. True = handled. */
    onSlash(name: string, args?: string): boolean;
  }

  let {
    sessionId,
    running,
    disabled,
    slashCommands,
    workspaceId,
    terminals,
    focused,
    onSubmit,
    onInterrupt,
    onCycleMode,
    onSlash,
  }: Props = $props();

  const uid = $props.id();
  const COMPOSER_MIN_HEIGHT = 38;
  const COMPOSER_MAX_HEIGHT = 352;

  // The parent {#key}s ChatView (and so this composer) per session — one
  // instance, one session — and remounts it on every tab switch, so the
  // draft must live in the session-keyed module store, not the component.
  // svelte-ignore state_referenced_locally
  const savedDraft = sessionId !== null ? loadDraft(sessionId) : { text: "", images: [] };
  let draft = $state(savedDraft.text);
  let images = $state<ImageAttachment[]>(savedDraft.images.slice(0, IMAGE_MAX_ATTACHMENTS));
  let attachmentError = $state<string | null>(null);

  function addImage(image: ImageAttachment): boolean {
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
    const text = draft;
    const imgs = $state.snapshot(images);
    if (sessionId === null) return;
    saveDraft(sessionId, text, imgs);
  });
  let el = $state<HTMLTextAreaElement | null>(null);
  let caret = $state(savedDraft.text.length);
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
    if (focused) el?.focus();
  });

  // Workbench splits resize without changing the browser viewport. Observe
  // the owning chat pane so both auto and manual heights stay inside the live
  // reading area; disconnect on remount/tab switch per the runes teardown rule.
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

  // Workbench insert flows (selection references, provenance tags) land in
  // the draft exactly like they would type into a PTY's input — appended,
  // never submitted.
  $effect(() => {
    if (sessionId === null) return;
    return registerComposer(sessionId, (text) => {
      draft = draft.length > 0 && !draft.endsWith(" ") ? `${draft} ${text}` : draft + text;
      focusAt(draft.length);
    });
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
   *  @term:NAME (linked-terminal grants) alongside file paths. */
  function atToken(): { start: number; text: string } | null {
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
    // it also reads unambiguously as "this folder" in the prompt).
    replaceToken(entry.kind === "dir" ? `@${entry.rel}/ ` : `@${entry.rel} `);
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

  function submit() {
    const text = draft.trim();
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
    if (onSubmit(text, images)) {
      clearSubmittedDraft();
      images = [];
    }
  }

  function onKeydown(e: KeyboardEvent) {
    // IME composition: Enter/arrows select a conversion candidate, not a chat
    // action. WebKit (the Tauri shell's WKWebView) fires the committing Enter
    // after compositionend with isComposing=false but keyCode 229 — check both.
    if (e.isComposing || e.keyCode === 229) return;
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

<div class="composer">
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
      {#each images as img, i (i)}
        <span class="attachment">
          {img.label}
          <button
            class="attachment-x"
            aria-label="remove attachment"
            onclick={() => {
              images = images.filter((_, j) => j !== i);
              attachmentError = null;
            }}>×</button
          >
        </span>
      {/each}
    </div>
  {/if}
  {#if attachmentError !== null}
    <div class="attachment-error" role="status">{attachmentError}</div>
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
    <textarea
      bind:this={el}
      bind:value={draft}
      onkeydown={onKeydown}
      onkeyup={trackCaret}
      onselect={trackCaret}
      oninput={trackCaret}
      onpaste={onPaste}
      role="combobox"
      aria-expanded={popover !== null}
      aria-controls="{uid}-pop"
      aria-autocomplete="list"
      aria-activedescendant={popover !== null ? `${uid}-opt-${selected}` : undefined}
      placeholder={disabled
        ? "chat ended"
        : running
          ? "queue a follow-up for the next run (Esc to stop)"
          : "message the agent… (Enter to send · / commands · @ files)"}
      rows={1}
      {disabled}
    ></textarea>
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
          onclick={submit}
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
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding-bottom: 6px;
  }
  .attachment-error {
    color: var(--warn);
    font-size: var(--text-xs);
    margin: 0 4px 4px;
  }
  .attachment {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    padding: 1px 8px;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .attachment-x {
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    padding: 0;
    font-size: var(--text-md);
    transition: color 0.12s ease;
  }
  .attachment-x:hover {
    color: var(--err);
  }
  /* flex: kills the inline-block baseline gap under the textarea, so the
     bottom-anchored action button measures from the real input edge. */
  .input-row {
    position: relative;
    display: flex;
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
  .stop {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    color: var(--accent);
    /* Faint breathing ring while the agent works — presence, not alarm. */
    animation: stop-breathe 1.8s ease-in-out infinite;
  }
  .stop:hover {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    border-color: var(--accent);
  }
  @keyframes stop-breathe {
    0%,
    100% {
      box-shadow: 0 0 0 0 color-mix(in srgb, var(--accent) 22%, transparent);
    }
    50% {
      box-shadow: 0 0 0 4px color-mix(in srgb, var(--accent) 8%, transparent);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .stop {
      animation: none;
    }
  }
</style>

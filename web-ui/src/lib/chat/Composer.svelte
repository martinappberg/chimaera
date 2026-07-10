<script lang="ts">
  import { fsQuickOpen, parentName, type QuickOpenEntry } from "../previews/files";
  import FileIcon from "../shared/FileIcon.svelte";
  import FolderIcon from "../shared/FolderIcon.svelte";
  import { registerComposer } from "./composerBus";
  import { loadDraft, saveDraft } from "./drafts";
  import type { SlashCommand } from "./store.svelte";

  export interface ImageAttachment {
    media_type: string;
    data: string;
    /** Display label, e.g. "screenshot 412×280". */
    label: string;
  }

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
    slashCommands: SlashCommand[];
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
    onSlash,
  }: Props = $props();

  const uid = $props.id();

  // The parent {#key}s ChatView (and so this composer) per session — one
  // instance, one session — and remounts it on every tab switch, so the
  // draft must live in the session-keyed module store, not the component.
  // svelte-ignore state_referenced_locally
  const savedDraft = sessionId !== null ? loadDraft(sessionId) : { text: "", images: [] };
  let draft = $state(savedDraft.text);
  let images = $state<ImageAttachment[]>(savedDraft.images);

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
  let selected = $state(0);
  let fileMatches = $state<QuickOpenEntry[]>([]);
  /** The @token the current matches were computed FOR (position + text) —
   *  pick-time caret state is unreliable once a popover button takes focus. */
  let fileToken = $state<{ start: number; text: string } | null>(null);
  let quickOpenTimer: ReturnType<typeof setTimeout> | null = null;

  $effect(() => {
    if (focused) el?.focus();
  });

  // Autosize from rendered height, not "\n" count — soft-wrapped pastes
  // must grow the box too. Cap ≈ 6 lines; beyond that it scrolls.
  $effect(() => {
    void draft;
    const t = el;
    if (t === null) return;
    t.style.height = "auto";
    t.style.height = `${t.scrollHeight + 2}px`; // +2: 1px border × 2, box-sizing is border-box
  });

  // Workbench insert flows (selection references, provenance tags) land in
  // the draft exactly like they would type into a PTY's input — appended,
  // never submitted.
  $effect(() => {
    if (sessionId === null) return;
    return registerComposer(sessionId, (text) => {
      draft = draft.length > 0 && !draft.endsWith(" ") ? `${draft} ${text}` : draft + text;
      el?.focus();
    });
  });

  /** Slash popover: draft is a single "/prefix" token being typed. */
  const slashMatches = $derived.by(() => {
    if (!draft.startsWith("/") || /\s/.test(draft)) return [];
    const q = draft.slice(1).toLowerCase();
    return slashCommands
      .filter((c) => c.name.toLowerCase().startsWith(q))
      .slice(0, 8);
  });

  /** The @token under the caret, if any (mention autocomplete). */
  function atToken(): { start: number; text: string } | null {
    const textarea = el;
    if (textarea === null) return null;
    const caret = textarea.selectionStart;
    const before = draft.slice(0, caret);
    // ":" admits @term:NAME (linked-terminal grants) alongside file paths.
    const match = /(^|\s)(@[\w./:-]*)$/.exec(before);
    if (match === null) return null;
    return { start: caret - match[2].length, text: match[2] };
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

  function pickSlash(name: string) {
    if (onSlash(name)) {
      draft = "";
      return;
    }
    draft = `/${name} `;
    el?.focus();
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
    el?.focus();
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
        draft = "";
        return;
      }
    }
    // Only clear the draft if the send was actually accepted — during a
    // reconnect window the socket is not OPEN and the message would otherwise
    // vanish silently.
    if (onSubmit(text, images)) {
      draft = "";
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
        if (popover === "slash") pickSlash(slashMatches[selected].name);
        else if (popover === "term") pickTerm(termMatches[selected]);
        else pickFile(fileMatches[selected]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        fileMatches = [];
        fileToken = null;
        draft = draft.startsWith("/") ? "" : draft;
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    } else if (e.key === "Escape" && running) {
      e.preventDefault();
      onInterrupt();
    }
  }

  /** Downscale cap matching the API's optimal image size. */
  const IMAGE_MAX_DIM = 1568;
  /** Post-encode payload cap; journal stores a placeholder anyway. */
  const IMAGE_MAX_BASE64 = 2 * 1024 * 1024;

  async function onPaste(e: ClipboardEvent) {
    const items = [...(e.clipboardData?.items ?? [])].filter((i) =>
      i.type.startsWith("image/"),
    );
    if (items.length === 0) return;
    e.preventDefault();
    for (const item of items) {
      const file = item.getAsFile();
      if (file === null) continue;
      try {
        const bitmap = await createImageBitmap(file);
        const scale = Math.min(1, IMAGE_MAX_DIM / Math.max(bitmap.width, bitmap.height));
        const canvas = document.createElement("canvas");
        canvas.width = Math.round(bitmap.width * scale);
        canvas.height = Math.round(bitmap.height * scale);
        canvas.getContext("2d")?.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
        const url = canvas.toDataURL("image/png");
        const data = url.slice(url.indexOf(",") + 1);
        if (data.length > IMAGE_MAX_BASE64) continue;
        images.push({
          media_type: "image/png",
          data,
          label: `image ${canvas.width}×${canvas.height}`,
        });
      } catch {
        // Unreadable clipboard image: nothing to attach.
      }
    }
  }
</script>

<div class="composer">
  {#if popover === "slash"}
    <div class="overlay-surface pop" id="{uid}-pop" role="listbox" aria-label="slash commands">
      {#each slashMatches as cmd, i (cmd.name)}
        <button
          class="overlay-row pop-row"
          class:sel={i === selected}
          id={`${uid}-opt-${i}`}
          role="option"
          aria-selected={i === selected}
          title={cmd.description ?? ""}
          onclick={() => pickSlash(cmd.name)}
        >
          <span class="pop-name">/{cmd.name}</span>
          {#if cmd.description}
            <span class="pop-desc">{cmd.description}</span>
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
            onclick={() => (images = images.filter((_, j) => j !== i))}>×</button
          >
        </span>
      {/each}
    </div>
  {/if}

  <textarea
    bind:this={el}
    bind:value={draft}
    onkeydown={onKeydown}
    onpaste={onPaste}
    role="combobox"
    aria-expanded={popover !== null}
    aria-controls="{uid}-pop"
    aria-autocomplete="list"
    aria-activedescendant={popover !== null ? `${uid}-opt-${selected}` : undefined}
    placeholder={disabled
      ? "chat ended"
      : running
        ? "type through — the agent hears you mid-run (Esc to stop)"
        : "message the agent… (Enter to send · / commands · @ files)"}
    rows={1}
    {disabled}
  ></textarea>
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
  textarea {
    width: 100%;
    resize: none;
    background: color-mix(in srgb, var(--fg) 3%, transparent);
    border: 1px solid var(--edge);
    border-radius: 8px;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-md);
    line-height: 1.45;
    padding: 7px 10px;
    max-height: 130px; /* 6 lines at 13px/1.45 + 14px padding + 2px border */
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
</style>

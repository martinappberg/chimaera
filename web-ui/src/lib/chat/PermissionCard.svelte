<script lang="ts">
  import { approvalSubject, orderedOptions, type ApprovalTool } from "./approval";
  import DiffView from "./DiffView.svelte";
  import { readableToolTitle } from "./toolLabels";
  import type { PendingPermission } from "./store.svelte";

  interface Props {
    request: PendingPermission;
    /** The transcript row this request is about, when it has one: what the
     *  card shows (the command, the change) is read from it. */
    tool?: ApprovalTool | null;
    /** A path as the reader should see it (workspace-relative). */
    pathLabel?: (path: string) => string;
    /** Open the file an edit is about. */
    onOpenPath?: (path: string, e: MouseEvent, anchor: Element) => void;
    onDecide: (optionId: string, destination?: string, feedback?: string) => void;
    /** False while the owning retained chat tab is hidden. Hidden prompts
     *  remain mounted so typed feedback survives, but may never steal focus. */
    visible?: boolean;
    canFeedback?: boolean;
    canChooseDestination?: boolean;
  }

  let {
    request,
    tool = null,
    pathLabel,
    onOpenPath,
    onDecide,
    visible = true,
    canFeedback = true,
    canChooseDestination = true,
  }: Props = $props();
  let showInput = $state(false);

  /** What is being approved — the command or the change — shown outright;
   *  the raw arguments stay one click away. */
  const subject = $derived(approvalSubject(request.inputPreview, tool));
  const options = $derived(orderedOptions(request.options));
  const allowOption = $derived(options.find((o) => o.kind === "allow_once"));
  /** The row's readable title names a call that is neither ("Read notes.txt",
   *  an MCP tool); a request with no row keeps the agent's own title. */
  const heading = $derived(
    subject.kind === "command"
      ? (subject.description ?? "Run command")
      : subject.kind === "edit"
        ? "Edit"
        : tool !== null && tool.title.trim() !== ""
          ? readableToolTitle(tool)
          : request.title,
  );
  const hasRaw = $derived(
    request.inputPreview !== null &&
      request.inputPreview !== undefined &&
      !(typeof request.inputPreview === "object" && Object.keys(request.inputPreview as object).length === 0),
  );

  /** Deny-with-feedback: the typed reason rides the deny so the agent reacts
   *  to it instead of aborting (both drivers deliver it per their protocol). */
  let feedbackOpen = $state(false);
  let feedbackText = $state("");
  let feedbackEl = $state<HTMLInputElement | null>(null);
  const rejectOption = $derived(request.options.find((o) => o.kind === "reject_once"));
  $effect(() => {
    if (visible && feedbackOpen) feedbackEl?.focus({ preventScroll: true });
  });
  function submitFeedback() {
    const text = feedbackText.trim();
    if (rejectOption === undefined || text.length === 0) return;
    onDecide(rejectOption.id, undefined, text);
  }

  /** Where "Always allow" saves the rule — claude's destination cycler
   *  (option id "allow_always" is the claude driver's; codex "always"
   *  options carry their own semantics and take no destination). */
  const DESTINATIONS = ["localSettings", "userSettings", "projectSettings", "session"] as const;
  const DEST_LABELS: Record<string, string> = {
    localSettings: "this project (just you)",
    userSettings: "all projects",
    projectSettings: "this project (shared)",
    session: "this session",
  };
  const DEST_HINTS: Record<string, string> = {
    localSettings: "saves to .claude/settings.local.json (gitignored)",
    userSettings: "saves to ~/.claude/settings.json",
    projectSettings: "saves to .claude/settings.json (shared with team)",
    session: "only for this session (not saved)",
  };
  const DEST_KEY = "chimaera-permission-destination";
  function loadDestination(): string {
    try {
      const stored = localStorage.getItem(DEST_KEY);
      return DESTINATIONS.includes(stored as (typeof DESTINATIONS)[number])
        ? (stored as string)
        : "localSettings";
    } catch {
      // Storage can be disabled in hardened/private WebViews. A permission
      // card is security-critical UI and must remain answerable regardless.
      return "localSettings";
    }
  }
  let destination = $state(loadDestination());
  const hasDestination = $derived(canChooseDestination && request.options.some((o) => o.id === "allow_always"));
  function cycleDestination() {
    const next =
      DESTINATIONS[(DESTINATIONS.indexOf(destination as never) + 1) % DESTINATIONS.length];
    destination = next;
    try {
      localStorage.setItem(DEST_KEY, next);
    } catch {
      // Keep the in-memory choice usable even when persistence is unavailable.
    }
  }
  function decide(optionId: string) {
    onDecide(optionId, hasDestination && optionId === "allow_always" ? destination : undefined);
  }

  let cardEl = $state<HTMLDivElement | null>(null);
  // Mirror the TUI: the permission prompt captures input on arrival.
  // preventScroll keeps ChatView's stick-to-bottom-unless-reading contract —
  // its own effect (keyed on pending.length) scrolls when the user is at bottom.
  $effect(() => {
    if (visible) cardEl?.focus({ preventScroll: true });
  });

  const preview = $derived(JSON.stringify(request.inputPreview, null, 2));
  /** Keyboard affordance: Enter = first allow option, Esc = first reject.
   *  Enter only counts when the card itself has focus — keydown bubbles from
   *  the option buttons, and hijacking it would fire "allow" from a
   *  Tab-focused reject button. Escape stays unguarded: child buttons have no
   *  Escape default, and Esc-rejects-from-anywhere matches the TUI — except
   *  while the feedback row is open, where the first Esc just closes it. */
  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && e.target === e.currentTarget) {
      const allow = request.options.find((o) => o.kind === "allow_once");
      if (allow) {
        e.preventDefault();
        decide(allow.id);
      }
    } else if (e.key === "Escape") {
      e.preventDefault();
      if (feedbackOpen) {
        feedbackOpen = false;
        if (visible) cardEl?.focus({ preventScroll: true });
        return;
      }
      const deny = request.options.find((o) => o.kind === "reject_once");
      if (deny) {
        decide(deny.id);
      }
    }
  }
</script>

<!-- Focusable container so Enter/Esc answer the prompt without mousing;
     the buttons below remain the accessible path. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="permission" role="group" tabindex="0" bind:this={cardEl} onkeydown={onKeydown}>
  <div class="ask">
    <span class="mark">?</span>
    <span class="label">
      {heading}{#if subject.kind === "edit"}{" "}
        {#if onOpenPath !== undefined}
          <button class="path" onclick={(e) => onOpenPath?.(subject.path, e, e.currentTarget)}
            >{pathLabel?.(subject.path) ?? subject.path}</button
          >
        {:else}
          <span class="path static">{pathLabel?.(subject.path) ?? subject.path}</span>
        {/if}
      {/if}
    </span>
    {#if hasRaw}
      <button class="peek" aria-expanded={showInput} onclick={() => (showInput = !showInput)}>
        {showInput ? "hide" : "details"}
      </button>
    {/if}
  </div>
  {#if subject.kind === "command"}
    <pre class="command quiet-scrollbar"><span class="prompt" aria-hidden="true">❯ </span>{subject.command}</pre>
  {:else if subject.kind === "edit"}
    <div class="change quiet-scrollbar">
      {#each subject.diffs as diff, i (i)}
        <DiffView {diff} showPath={false} />
      {/each}
    </div>
  {/if}
  {#if showInput}
    <pre class="input quiet-scrollbar">{preview}</pre>
  {/if}
  <div class="actions">
    {#each options as option (option.id)}
      <button
        class="opt"
        class:primary={option === allowOption}
        class:quiet={option.kind.startsWith("reject")}
        onclick={() => decide(option.id)}
      >
        {option.label}{#if option === allowOption}<kbd>↵</kbd>{:else if option === rejectOption}<kbd>esc</kbd>{/if}
      </button>
    {/each}
    {#if canFeedback && rejectOption !== undefined}
      <button
        class="fb-toggle"
        class:open={feedbackOpen}
        title="deny and tell the agent what to do instead"
        onclick={() => (feedbackOpen = !feedbackOpen)}
      >
        deny with feedback…
      </button>
    {/if}
    {#if hasDestination}
      <button
        class="dest"
        title={`where "Always allow" saves — ${DEST_HINTS[destination]} (click to change)`}
        onclick={cycleDestination}
      >
        → {DEST_LABELS[destination]}
      </button>
    {/if}
  </div>
  {#if feedbackOpen}
    <div class="feedback">
      <input
        bind:this={feedbackEl}
        bind:value={feedbackText}
        placeholder="what should the agent do instead?"
        onkeydown={(e) => {
          if (e.key === "Enter" && feedbackText.trim().length > 0) {
            e.preventDefault();
            submitFeedback();
          }
        }}
      />
      <button class="opt quiet" disabled={feedbackText.trim().length === 0} onclick={submitFeedback}>
        deny + send
      </button>
    </div>
  {/if}
</div>

<style>
  .permission {
    border: 1px solid color-mix(in srgb, var(--warn) 55%, var(--edge));
    background: color-mix(in srgb, var(--warn) 7%, transparent);
    border-radius: 6px;
    padding: 8px 10px;
    margin: 6px 0;
    outline: none;
  }
  .permission:focus-visible {
    box-shadow: 0 0 0 2px var(--focus-ring);
  }
  .ask {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--text-sm);
  }
  .mark {
    flex: none;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--warn);
    /* Fixed dark ink: --warn is mid-gold in every theme, so a theme-tracking
       ink fails on the light side (near-white on gold ≈ 2:1). */
    color: rgba(0, 0, 0, 0.8);
    display: grid;
    place-items: center;
    font-size: var(--text-xs);
    font-weight: 700;
  }
  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .path {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent-text);
    font-family: var(--mono, monospace);
    font-size: inherit;
    cursor: pointer;
  }
  .path.static {
    cursor: default;
  }
  button.path:hover {
    text-decoration: underline;
  }
  /* The thing being approved: the command as it will run, or the change.
     Bounded so a long script or a big edit never pushes the answer buttons
     off the screen. */
  .command,
  .change {
    margin: 8px 0 0;
    padding: 6px 8px;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
    border-radius: 4px;
    max-height: 200px;
    overflow: auto;
  }
  .command {
    font-size: var(--text-sm);
    font-family: var(--mono, monospace);
    white-space: pre-wrap;
    word-break: break-word;
  }
  .prompt {
    color: var(--muted);
    user-select: none;
  }
  .change {
    padding: 4px 0;
  }
  kbd {
    margin-left: 7px;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .peek {
    background: none;
    border: none;
    color: var(--muted);
    font-size: var(--text-sm);
    cursor: pointer;
    padding: 0;
    transition: color 0.12s ease;
  }
  .peek:hover {
    color: var(--fg);
  }
  .input {
    margin: 6px 0 0;
    padding: 6px 8px;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
    border-radius: 4px;
    font-size: var(--text-sm);
    font-family: var(--mono, monospace);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 180px;
    overflow: auto;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin-top: 8px;
  }
  /* Base .opt / .opt.primary / .opt.quiet live in app.css. Reject options
     (the only quiet ones here) turn danger-red on hover instead of the shared
     neutral hover — the permission prompt's own affordance. */
  .opt.quiet:hover {
    color: var(--err);
    border-color: color-mix(in srgb, var(--err) 45%, var(--edge));
  }
  .dest {
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
    padding: 3px 4px;
    margin-left: auto;
    transition: color 0.12s ease;
  }
  .dest:hover {
    color: var(--fg);
  }
  .fb-toggle {
    background: none;
    border: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
    padding: 3px 4px;
    transition: color 0.12s ease;
  }
  .fb-toggle:hover,
  .fb-toggle.open {
    color: var(--err);
  }
  .feedback {
    display: flex;
    gap: 6px;
    margin-top: 6px;
  }
  .feedback input {
    flex: 1;
    min-width: 0;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 4px 10px;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
  }
  .feedback input:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--err) 45%, var(--edge));
  }
</style>

<script lang="ts">
  import type { PendingPermission } from "./store.svelte";

  interface Props {
    request: PendingPermission;
    onDecide: (optionId: string, destination?: string) => void;
  }

  let { request, onDecide }: Props = $props();
  let showInput = $state(false);

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
  let destination = $state(
    DESTINATIONS.includes(localStorage.getItem(DEST_KEY) as (typeof DESTINATIONS)[number])
      ? (localStorage.getItem(DEST_KEY) as string)
      : "localSettings",
  );
  const hasDestination = $derived(request.options.some((o) => o.id === "allow_always"));
  function cycleDestination() {
    const next =
      DESTINATIONS[(DESTINATIONS.indexOf(destination as never) + 1) % DESTINATIONS.length];
    destination = next;
    localStorage.setItem(DEST_KEY, next);
  }
  function decide(optionId: string) {
    onDecide(optionId, optionId === "allow_always" ? destination : undefined);
  }

  let cardEl = $state<HTMLDivElement | null>(null);
  // Mirror the TUI: the permission prompt captures input on arrival.
  // preventScroll keeps ChatView's stick-to-bottom-unless-reading contract —
  // its own effect (keyed on pending.length) scrolls when the user is at bottom.
  $effect(() => {
    cardEl?.focus({ preventScroll: true });
  });

  const preview = $derived(JSON.stringify(request.inputPreview, null, 2));
  /** Keyboard affordance: Enter = first allow option, Esc = first reject.
   *  Enter only counts when the card itself has focus — keydown bubbles from
   *  the option buttons, and hijacking it would fire "allow" from a
   *  Tab-focused reject button. Escape stays unguarded: child buttons have no
   *  Escape default, and Esc-rejects-from-anywhere matches the TUI. */
  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && e.target === e.currentTarget) {
      const allow = request.options.find((o) => o.kind === "allow_once");
      if (allow) {
        e.preventDefault();
        decide(allow.id);
      }
    } else if (e.key === "Escape") {
      const deny = request.options.find((o) => o.kind === "reject_once");
      if (deny) {
        e.preventDefault();
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
    <span class="label">{request.title} wants to run</span>
    <button class="peek" onclick={() => (showInput = !showInput)}>
      {showInput ? "hide" : "details"}
    </button>
  </div>
  {#if showInput}
    <pre class="input">{preview}</pre>
  {/if}
  <div class="actions">
    {#each request.options as option (option.id)}
      <button
        class="opt"
        class:primary={option.kind === "allow_once"}
        class:quiet={option.kind.startsWith("reject")}
        onclick={() => decide(option.id)}
      >
        {option.label}
      </button>
    {/each}
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
    font-size: 11px;
    font-weight: 700;
  }
  .label {
    flex: 1;
    min-width: 0;
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
    gap: 6px;
    margin-top: 8px;
  }
  .opt {
    font: inherit;
    font-size: var(--text-sm);
    padding: 3px 12px;
    border-radius: 5px;
    border: 1px solid var(--edge);
    background: none;
    color: var(--fg);
    cursor: pointer;
    transition:
      color 0.12s ease,
      border-color 0.12s ease,
      background-color 0.12s ease;
  }
  /* Accent tint + --fg ink, not accent-filled: no single fill ink survives
     both the light themes' dark accents and the dark themes' bright ones. */
  .opt.primary {
    background: color-mix(in srgb, var(--accent) 15%, transparent);
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    color: var(--fg);
  }
  .opt.primary:hover {
    background: color-mix(in srgb, var(--accent) 24%, transparent);
  }
  .opt.quiet {
    color: var(--muted);
  }
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
</style>

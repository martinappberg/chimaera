<script lang="ts">
  import { copyText } from "../shared/clipboard";
  import { formatFullTimestamp, formatMessageTimestamp } from "../shared/time";

  /**
   * The quiet action cluster on an agent message — time, then usage, copy,
   * rewind-to-here and fork-from-here as small icons — at the message's top
   * right, shown on hover or keyboard focus (the buttons stay in the tab
   * order, so focus is what reveals them). Each action is optional: a
   * read-only view offers copy alone; rewind exists only once a later
   * message of yours gives the agent a point to return to.
   */
  interface Props {
    text: string;
    sentAtMs: number;
    nowMs: number;
    onFork?: () => void;
    /** Rewind the conversation to just after this reply (the RewindDialog
     *  flow, anchored on the next message you sent). */
    onRewind?: () => void;
    rewindTitle?: string;
    /** Show plan usage (the UsagePanel flow). */
    onUsage?: () => void;
    /** The turn's footprint for the usage icon's tooltip ("12.3k tokens"). */
    turnTokens?: string | null;
    /** The turn's duration ("5.4s") when this is the turn's closing message —
     *  a detail for the timestamp's tooltip, not the page. */
    turnDuration?: string | null;
  }

  let {
    text,
    sentAtMs,
    nowMs,
    onFork,
    onRewind,
    rewindTitle = "rewind to here",
    onUsage,
    turnTokens = null,
    turnDuration = null,
  }: Props = $props();
  const usageTitle = $derived(
    [turnTokens !== null ? `this turn: ${turnTokens}` : null, turnDuration, "show plan usage"]
      .filter((part) => part !== null)
      .join(" · "),
  );

  const timeLabel = $derived(formatMessageTimestamp(sentAtMs, nowMs));
  const fullTime = $derived(
    turnDuration !== null
      ? `${formatFullTimestamp(sentAtMs)} · turn took ${turnDuration}`
      : formatFullTimestamp(sentAtMs),
  );
  const isoTime = $derived(sentAtMs > 0 ? new Date(sentAtMs).toISOString() : "");

  let copied = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;
  function copyMessage(event: MouseEvent) {
    const button = event.currentTarget as HTMLButtonElement;
    const pointerActivated = event.detail > 0;
    void copyText(text).then((ok) => {
      if (!ok) return;
      copied = true;
      if (copiedTimer !== null) clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => {
        copiedTimer = null;
        copied = false;
        if (pointerActivated) button.blur();
      }, 1400);
    });
  }
  $effect(() => () => {
    if (copiedTimer !== null) clearTimeout(copiedTimer);
  });
</script>

<div class="agent-message-meta">
  <!-- An imported conversation may not know when a message was sent
       (a subagent's replay): no time rather than the epoch. -->
  {#if sentAtMs > 0}
    <time datetime={isoTime} title={fullTime}>{timeLabel}</time>
  {/if}
  <span class="actions">
    {#if onUsage !== undefined}
      <button aria-label="show plan usage" title={usageTitle} onclick={onUsage}>
        <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
          <path
            d="M2 11.5l3.2-3.6 2.6 2.2L11 5.5l3 3"
            fill="none"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      </button>
    {/if}
    <button
      class="copy-action"
      class:copied
      aria-label={copied ? "copied full message" : "copy full message"}
      title={copied ? "copied" : "copy full message"}
      onclick={copyMessage}
    >
      <span class="copy-glyphs" aria-hidden="true">
        <svg class="copy-icon" viewBox="0 0 16 16" width="12" height="12">
          <rect
            x="6"
            y="6"
            width="7.5"
            height="7.5"
            rx="1.5"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
          />
          <path
            d="M4 10h-.5A1.5 1.5 0 0 1 2 8.5v-5A1.5 1.5 0 0 1 3.5 2h5A1.5 1.5 0 0 1 10 3.5V4"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
          />
        </svg>
        <svg class="check-icon" viewBox="0 0 16 16" width="12" height="12">
          <path
            d="M3.5 8.5l3 3 6-6.5"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      </span>
    </button>
    {#if onRewind !== undefined}
      <button aria-label={rewindTitle} title={rewindTitle} onclick={onRewind}>
        <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
          <path
            d="M3.5 5.5h6a3.5 3.5 0 0 1 0 7H6M3.5 5.5L6 3M3.5 5.5L6 8"
            fill="none"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      </button>
    {/if}
    {#if onFork !== undefined}
      <button
        aria-label="fork conversation from this message"
        title="fork from this message into a new session (source keeps running)"
        onclick={onFork}
      >
        <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true">
          <circle cx="4" cy="3.5" r="1.5" fill="none" stroke="currentColor" stroke-width="1.4" />
          <circle cx="4" cy="12.5" r="1.5" fill="none" stroke="currentColor" stroke-width="1.4" />
          <circle cx="12" cy="8" r="1.5" fill="none" stroke="currentColor" stroke-width="1.4" />
          <path
            d="M5.5 3.5h.5A2 2 0 0 1 8 5.5v5A2 2 0 0 1 6 12.5h-.5M8 8h2.5"
            fill="none"
            stroke="currentColor"
            stroke-width="1.4"
            stroke-linecap="round"
          />
        </svg>
      </button>
    {/if}
  </span>
</div>

<style>
  /* The message's top right, floating over the gap above it: it reserves no
     row, and when it shows it sits on the page's own background so the
     prose under its corner stays legible around it. */
  .agent-message-meta {
    position: absolute;
    top: -13px;
    right: 0;
    z-index: 1;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 22px;
    padding: 0 3px 0 8px;
    border-radius: 7px;
    background: var(--bg);
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--edge) 70%, transparent);
    color: color-mix(in srgb, var(--muted) 78%, transparent);
    font-size: var(--text-xs);
    line-height: 1;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    opacity: 0;
    /* Invisible, it must not catch the pointer over the prose it covers
       (selection drags start there); focus still reaches its buttons. */
    pointer-events: none;
    transition: opacity 0.12s ease;
  }
  :global(.msg.agent:hover) .agent-message-meta,
  .agent-message-meta:focus-within {
    opacity: 1;
    pointer-events: auto;
  }
  time {
    user-select: none;
  }
  .actions {
    display: inline-flex;
    align-items: center;
    gap: 1px;
  }
  button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 20px;
    padding: 0;
    background: none;
    border: none;
    border-radius: 5px;
    color: inherit;
    cursor: pointer;
    transition:
      color 0.12s ease,
      background 0.12s ease;
  }
  button:hover,
  button:focus-visible {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .copy-glyphs {
    position: relative;
    display: block;
    width: 12px;
    height: 12px;
  }
  .copy-glyphs svg {
    position: absolute;
    inset: 0;
    transform-origin: center;
    transition:
      opacity 0.16s ease,
      transform 0.16s ease;
  }
  .copy-icon {
    opacity: 1;
    transform: scale(1);
  }
  .check-icon {
    opacity: 0;
    transform: scale(0.72);
  }
  .copy-action.copied,
  .copy-action.copied:hover,
  .copy-action.copied:focus-visible {
    color: inherit;
    background: none;
    outline-color: transparent;
  }
  .copy-action.copied .copy-icon {
    opacity: 0;
    transform: scale(0.78);
  }
  .copy-action.copied .check-icon {
    opacity: 0.68;
    transform: scale(1);
  }
  /* Touch: nothing hovers, so the cluster stays visible — inline after
     the message's last word (ChatView flows it), never over the prose. */
  @media (hover: none) {
    .agent-message-meta {
      position: static;
      height: 20px;
      margin-left: 0.6em;
      padding: 0;
      vertical-align: middle;
      background: none;
      box-shadow: none;
      opacity: 1;
      pointer-events: auto;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .agent-message-meta {
      transition: none;
    }
    .copy-glyphs svg {
      transition: none;
    }
  }
</style>

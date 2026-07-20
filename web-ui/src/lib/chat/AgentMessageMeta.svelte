<script lang="ts">
  import { copyText } from "../shared/clipboard";
  import { formatFullTimestamp, formatMessageTimestamp } from "../shared/time";

  interface Props {
    text: string;
    sentAtMs: number;
    nowMs: number;
    onFork: () => void;
  }

  let { text, sentAtMs, nowMs, onFork }: Props = $props();

  const timeLabel = $derived(formatMessageTimestamp(sentAtMs, nowMs));
  const fullTime = $derived(formatFullTimestamp(sentAtMs));
  const isoTime = $derived(new Date(sentAtMs).toISOString());

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
  <time datetime={isoTime} title={fullTime}>{timeLabel}</time>
  <span class="actions">
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
  </span>
</div>

<style>
  .agent-message-meta {
    min-height: 20px;
    display: flex;
    align-items: center;
    gap: 5px;
    width: fit-content;
    color: color-mix(in srgb, var(--muted) 78%, transparent);
    font-size: var(--text-xs);
    line-height: 1;
    font-variant-numeric: tabular-nums;
    opacity: 0;
    transition: opacity 0.12s ease;
  }
  :global(.msg.agent:hover) .agent-message-meta,
  .agent-message-meta:focus-within {
    opacity: 1;
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
  @media (hover: none) {
    .agent-message-meta {
      opacity: 1;
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

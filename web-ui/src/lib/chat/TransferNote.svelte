<script lang="ts">
  import { formatFullTimestamp, formatMessageTimestamp } from "../shared/time";
  import { isBrowserGateway } from "../net/base";
  import { transferNote, type TransferOrigin } from "./transfer";

  interface Props {
    origin: TransferOrigin;
    /** The daemon's message to the agent: shown only on request, as plain
     *  text (it can carry agent-influenced background task names). */
    text: string;
    sentAtMs: number;
    /** ChatView's shared clock for relative timestamps. */
    nowMs: number;
    sourceIndex: number;
    sourceUid: number;
  }

  let { origin, text, sentAtMs, nowMs, sourceIndex, sourceUid }: Props = $props();

  const note = $derived(transferNote(origin, text, isBrowserGateway()));
  const timeLabel = $derived(formatMessageTimestamp(sentAtMs, nowMs));
  let open = $state(false);
</script>

<!-- A transfer is an event in the conversation, not something the person
     said: one quiet line across the column, with what the daemon told the
     agent behind a disclosure. -->
<div class="transfer" data-block-index={sourceIndex} data-block-uid={sourceUid}>
  <p class="rule">
    <span class="note"
      >{note}{#if timeLabel !== ""}<span class="sep" aria-hidden="true">{" · "}</span><time
          datetime={new Date(sentAtMs).toISOString()}
          title={formatFullTimestamp(sentAtMs)}>{timeLabel}</time
        >{/if}</span
    >
  </p>
  <details bind:open>
    <summary>{open ? "Hide what the agent was told" : "Show what the agent was told"}</summary>
    <p class="told">{text}</p>
  </details>
</div>

<style>
  .transfer {
    display: flex;
    flex-direction: column;
    align-items: center;
    margin: 16px 0 8px;
    color: var(--muted);
    font-size: var(--text-xs);
    line-height: 1.5;
  }
  .rule {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    margin: 0;
  }
  .rule::before,
  .rule::after {
    content: "";
    flex: 1;
    border-top: 1px solid var(--edge);
  }
  .note {
    max-width: 80%;
    text-align: center;
  }
  time {
    font-variant-numeric: tabular-nums;
  }
  details {
    width: 100%;
  }
  /* Only the words toggle, not the whole row (a stray click beside them
     would otherwise open it). */
  summary {
    display: block;
    width: fit-content;
    margin: 0 auto;
    list-style: none;
    cursor: pointer;
    padding: 2px 6px;
    border-radius: 4px;
  }
  summary::-webkit-details-marker {
    display: none;
  }
  summary:hover {
    color: var(--fg);
  }
  summary:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 2px;
  }
  .told {
    max-width: 40rem;
    margin: 6px auto 0;
    padding: 8px 12px;
    border: 1px solid var(--edge);
    border-radius: 8px;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
    color: var(--muted);
    font-size: var(--text-sm);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    text-align: left;
  }
</style>

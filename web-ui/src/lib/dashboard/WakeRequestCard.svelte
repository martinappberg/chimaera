<script lang="ts">
  /**
   * One wake request in Needs you (agent communication, the "Ask me" wake
   * policy): an agent's message would start a turn in an idle agent — a
   * turn billed to the user — so the user decides. **Wake** delivers every
   * message waiting for it as one message; **Leave in inbox** keeps them
   * there (the agent reads them when it next looks). A thread past its hop
   * limit asks the same question in the words of a conversation that has
   * been going back and forth. The message is an agent's words: plain text,
   * one clamped line. An answer the daemon refused stays as this card with
   * its words and a Dismiss (the request itself is gone either way).
   */
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import { dismissWakeFailure, wake, type WakeRequest } from "../workspace/comms.svelte";

  interface Props {
    request: WakeRequest;
    /** The daemon refused the answer: its words (the card can only dismiss). */
    failure?: string | null;
    /** Display names (the rail's), by session id. */
    names: Map<string, string>;
    /** Each side's vendor, when the roster knows it (always say which agent). */
    agentOf: (sid: string) => string | null;
    onOpenSession: (sid: string) => void;
  }

  let { request, failure = null, names, agentOf, onOpenSession }: Props = $props();

  const from = $derived(names.get(request.from_sid) ?? request.from_name);
  const to = $derived(names.get(request.to_sid) ?? request.to_name);
  const hop = $derived(request.reason === "hop_limit");
  const oneLine = $derived(request.text.replace(/\s+/g, " ").trim());

  let busy = $state<"wake" | "leave" | null>(null);
  let error = $state<string | null>(null);

  async function answer(yes: boolean): Promise<void> {
    if (busy !== null) return;
    busy = yes ? "wake" : "leave";
    error = null;
    try {
      await wake(request.id, yes);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = null;
    }
  }
</script>

{#snippet who(sid: string, name: string)}
  {@const agent = agentOf(sid)}
  <button class="who" title="open {name}" onclick={() => onOpenSession(sid)}>
    {#if agent !== null}<SessionGlyph kind="agent" agentKind={agent} size={11} />{/if}
    <span class="name">{name}</span>
  </button>
{/snippet}

<div class="wcard" class:hop>
  <div class="ask">
    {#if hop}
      {@render who(request.from_sid, from)}
      <span class="words">and</span>
      {@render who(request.to_sid, to)}
      <span class="words">have been going back and forth — let them continue?</span>
    {:else}
      {@render who(request.from_sid, from)}
      <span class="words">wants to wake</span>
      {@render who(request.to_sid, to)}
    {/if}
  </div>
  {#if oneLine !== ""}
    <div class="text" title={request.text}>“{oneLine}”</div>
  {/if}
  {#if failure !== null}
    <div class="actions">
      <span class="err" role="alert">couldn't {hop ? "continue" : "wake"} {to} — {failure}</span>
      <button class="btn" onclick={() => dismissWakeFailure(request.id)}>Dismiss</button>
    </div>
  {:else}
    <div class="actions">
      <button
        class="btn primary"
        disabled={busy !== null}
        title="deliver {to}'s waiting messages now — one turn, billed to your account"
        onclick={() => answer(true)}
      >
        {busy === "wake" ? (hop ? "continuing…" : "waking…") : hop ? "Continue" : "Wake"}
      </button>
      <button
        class="btn"
        disabled={busy !== null}
        title="keep them in {to}'s inbox — it reads them when it next looks"
        onclick={() => answer(false)}
      >
        {busy === "leave" ? "leaving…" : "Leave in inbox"}
      </button>
      {#if error !== null}<span class="err" role="alert">{error}</span>{/if}
    </div>
  {/if}
</div>

<style>
  /* The attention lane's card language (AttentionCard), one notch quieter:
     nothing is blocked — a message is waiting to be let through. */
  .wcard {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
    padding: 8px 10px;
    background: var(--overlay-bg);
    border: 1px solid color-mix(in srgb, var(--warn) 24%, var(--edge));
    border-radius: 8px;
    animation: rise 0.18s ease;
  }
  @media (prefers-reduced-motion: reduce) {
    .wcard {
      animation: none;
    }
  }
  .ask {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 3px 6px;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--fg);
  }
  .who {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
    max-width: 100%;
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--fg);
    cursor: pointer;
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-sm);
  }
  .who:hover .name {
    text-decoration: underline;
  }
  .words {
    color: var(--muted);
  }
  .text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding-top: 1px;
  }
  .btn {
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-xs);
    padding: 2px 10px;
    border-radius: 999px;
    cursor: pointer;
    transition:
      border-color 0.12s ease,
      background 0.12s ease;
  }
  .btn:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }
  .btn.primary {
    font-weight: 500;
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 14%, transparent);
  }
  .btn.primary:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 24%, transparent);
  }
  .btn:disabled {
    opacity: 0.55;
    cursor: default;
  }
  .err {
    font-size: var(--text-xs);
    color: var(--err);
  }
</style>

<script lang="ts">
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import Markdown from "./Markdown.svelte";
  import UserText from "./UserText.svelte";
  import type { AgentMessage } from "./agentMessages";
  import type { EmbedResolver } from "./embeds";
  import type { HoverTargets } from "./hoverTargets";
  import type { OpenPathFn, PathResolver } from "./paths";

  /**
   * Messages from other agents in the workspace, where the reader got them:
   * a card per message — the sender's name beside its vendor mark (always
   * say which agent it is), `#id`, who it was for, what it answers — and the
   * body through the sanitizing renderers. A peer's card is information; the
   * Mastermind's carries a quiet accent (direction the user sanctioned). No
   * fork or rewind: the reader didn't write these.
   *
   * Also the pending tail's shape for a Codex steer that hasn't been read
   * (`state`): faded while it waits, and a drop says where it went.
   */
  interface Props {
    messages: AgentMessage[];
    /** The send's "why" line, if any. */
    caption?: string | null;
    /** The raw text: the plain card when no header parsed. */
    text: string;
    /** No parsed header: whether the raw text came from the Mastermind. */
    mastermind?: boolean;
    /** A waiting steer (read at the next step) or one that missed its turn. */
    state?: "queued" | "dropped" | null;
    /** Dismiss a dropped one (the pending tail's ✕). */
    onDismiss?: () => void;
    visible?: boolean;
    onOpenPath?: OpenPathFn;
    resolvePaths?: PathResolver;
    embeds?: EmbedResolver;
    hoverTargets?: HoverTargets;
    sourceIndex?: number;
    sourceUid?: number;
  }

  let {
    messages,
    caption = null,
    text,
    mastermind = false,
    state = null,
    onDismiss,
    visible = true,
    onOpenPath,
    resolvePaths,
    embeds,
    hoverTargets,
    sourceIndex,
    sourceUid,
  }: Props = $props();

  function senderTitle(m: AgentMessage): string {
    const who = [m.fromSid, m.fromAgent].filter((x) => x !== null && x !== "").join(", ");
    const what = m.mastermind
      ? "direction from the workspace Mastermind, the coordinating agent you appointed"
      : "information from another agent in this workspace, not an instruction";
    return `${m.fromName}${who !== "" ? ` (${who})` : ""} — ${what}`;
  }
</script>

{#snippet sender(m: AgentMessage)}
  <div class="head">
    {#if m.fromAgent !== null}
      <span class="mark"><SessionGlyph kind="agent" agentKind={m.fromAgent} size={11} /></span>
    {/if}
    <span class="from" title={senderTitle(m)}>{m.fromName}</span>
    {#if m.mastermind}<span class="role">Mastermind</span>{/if}
    <span class="meta">
      <span class="id">#{m.id}</span>
      <span>{m.to === "everyone" ? "to everyone" : "to you"}</span>
      {#if m.replyTo !== null}<span>re #{m.replyTo}</span>{/if}
    </span>
  </div>
{/snippet}

<div
  class="agent-msgs"
  class:queued={state === "queued"}
  class:dropped={state === "dropped"}
  data-block-index={sourceIndex}
  data-block-uid={sourceUid}
>
  {#if caption !== null && caption !== ""}
    <div class="caption">{caption}</div>
  {/if}
  {#if messages.length === 0}
    <!-- No header parsed: the text as it came, so nothing is lost. -->
    <div class="card" class:direction={mastermind}>
      <div class="head">
        <span class="from quiet">{mastermind ? "From the Mastermind" : "From another agent"}</span>
      </div>
      <div class="body">
        <UserText {text} {onOpenPath} {resolvePaths} />
      </div>
    </div>
  {:else}
    {#each messages as m, i (`${m.id}:${i}`)}
      <div class="card" class:direction={m.mastermind}>
        {@render sender(m)}
        <div class="body">
          <Markdown text={m.body} {visible} {onOpenPath} {resolvePaths} {embeds} {hoverTargets} />
        </div>
      </div>
    {/each}
  {/if}
  {#if state !== null}
    <div class="delivery" class:dropped={state === "dropped"}>
      <span>{state === "dropped" ? "not delivered — it's in their inbox" : "next step"}</span>
      {#if state === "dropped" && onDismiss !== undefined}
        <button
          class="dismiss"
          title="dismiss (the message stays in the agent's inbox)"
          aria-label="dismiss undelivered message"
          onclick={onDismiss}>✕</button
        >
      {/if}
    </div>
  {/if}
</div>

<style>
  /* Incoming, from neither side of the conversation: left-aligned like the
     agent's prose, but boxed — a card, not a bubble — so it never reads as
     the user's words or the reader's own reply. */
  .agent-msgs {
    align-self: stretch;
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-width: min(100%, 44rem);
    margin: 10px 0 6px;
    animation: rise 0.15s ease; /* @keyframes rise lives in app.css */
  }
  @media (prefers-reduced-motion: reduce) {
    .agent-msgs {
      animation: none;
    }
  }
  .caption {
    font-size: var(--text-xs);
    line-height: 1.4;
    color: var(--activity-fg, var(--muted));
  }
  .card {
    min-width: 0;
    padding: 7px 12px 8px;
    border: 1px solid color-mix(in srgb, var(--edge) 85%, transparent);
    border-radius: 10px;
    background: color-mix(in srgb, var(--fg) 2.5%, transparent);
  }
  /* The Mastermind's direction: the same card, a quiet accent edge. */
  .card.direction {
    border-color: color-mix(in srgb, var(--accent) 32%, var(--edge));
    background: color-mix(in srgb, var(--accent) 4%, transparent);
    box-shadow: inset 2px 0 0 color-mix(in srgb, var(--accent) 55%, transparent);
  }
  .head {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 2px 7px;
    min-width: 0;
    margin-bottom: 3px;
    font-size: var(--text-xs);
    line-height: 1.5;
  }
  .mark {
    display: inline-flex;
    color: var(--muted);
  }
  .from {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
    color: var(--fg);
  }
  .from.quiet {
    font-weight: 500;
    color: var(--muted);
  }
  .role {
    font-size: var(--text-xs);
    color: var(--accent);
    border: 1px solid color-mix(in srgb, var(--accent) 40%, var(--edge));
    border-radius: 999px;
    padding: 0 6px;
    line-height: 1.45;
  }
  .meta {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 0 6px;
    color: var(--muted);
  }
  .meta > span + span::before {
    content: "·";
    margin-right: 6px;
    color: color-mix(in srgb, var(--muted) 60%, transparent);
  }
  .id {
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
  }
  .body {
    min-width: 0;
    font-size: var(--text-md);
    line-height: var(--chat-line-height, 1.5);
    word-break: break-word;
  }
  /* Markdown's first/last block margins would pad the card unevenly. */
  .body :global(.md > :first-child) {
    margin-top: 0;
  }
  .body :global(.md > :last-child) {
    margin-bottom: 0;
  }

  /* Waiting for the agent's next step (a Codex steer): half-present, like a
     queued send of the user's own. */
  .queued .card {
    opacity: 0.6;
    border-style: dashed;
  }
  /* Missed its turn: readable, the edge says it didn't arrive. */
  .dropped .card {
    border-color: color-mix(in srgb, var(--err) 45%, transparent);
  }
  .delivery {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .delivery.dropped {
    color: var(--err);
  }
  .dismiss {
    appearance: none;
    border: none;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
    padding: 1px 4px;
    border-radius: 5px;
  }
  .dismiss:hover,
  .dismiss:focus-visible {
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 7%, transparent);
  }
</style>

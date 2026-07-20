<script lang="ts">
  /**
   * One "needs you" lane entry: a session waiting on the human, answerable in
   * place where the data allows it. Chat sessions with a journaled
   * PermissionRequest render the real permission card inline (the same
   * component the chat view uses — answering here IS answering there); every
   * other attention state says honestly what it knows and offers the door.
   */
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import PermissionCard from "../chat/PermissionCard.svelte";
  import { agentKind, dotState, dotTitle, type Session } from "../workspace/sessions";
  import type { ChatStore } from "../chat/store.svelte";

  interface Props {
    session: Session;
    name: string;
    /** Warm chat store (acquired upstream for every attention chat session). */
    store: ChatStore | null;
    onOpen: () => void;
    /** Answer a pending permission over the session's live socket. */
    onDecide?: (requestId: string, optionId: string, destination?: string, feedback?: string) => void;
  }

  let { session, name, store, onOpen, onDecide }: Props = $props();

  const pending = $derived(store?.pending[0] ?? null);
  const questionCount = $derived(store?.questions.length ?? 0);

  /** The honest one-liner when there is nothing to answer inline. */
  const reason = $derived.by(() => {
    switch (session.agent_state) {
      case "needs_permission":
        return session.ui === "chat"
          ? "needs permission"
          : "needs permission — answer in the terminal";
      case "idle_prompt":
        return "waiting for your input";
      case "errored":
        return "agent error — see the session";
      default:
        return "needs you";
    }
  });
</script>

<div class="acard">
  <button class="top" onclick={onOpen} title="open the session">
    <SessionGlyph
      kind={session.kind}
      agentKind={session.agent_kind}
      state={dotState(session)}
      size={12}
      title={dotTitle(session)}
    />
    <span class="dot" title={dotTitle(session)}></span>
    <span class="name">{name}</span>
    <span class="chip">{agentKind(session)} · {session.ui === "chat" ? "chat" : "term"}</span>
    <span class="why">{pending !== null ? "needs permission" : reason}</span>
    <span class="open">open →</span>
  </button>

  {#if pending !== null && onDecide !== undefined}
    {#key pending.requestId}
      <div class="perm">
        <PermissionCard
          request={pending}
          onDecide={(optionId, destination, feedback) =>
            onDecide(pending.requestId, optionId, destination, feedback)}
        />
      </div>
    {/key}
  {:else if questionCount > 0}
    <button class="ask" onclick={onOpen}>
      has {questionCount === 1 ? "a question" : `${questionCount} questions`} for you — answer in
      the chat →
    </button>
  {/if}
</div>

<style>
  .acard {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
    padding: 8px 10px;
    background: var(--overlay-bg);
    border: 1px solid color-mix(in srgb, var(--warn) 30%, var(--edge));
    border-radius: 8px;
    animation: rise 0.18s ease;
  }
  @media (prefers-reduced-motion: reduce) {
    .acard {
      animation: none;
    }
  }

  .top {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    border: none;
    background: none;
    font: inherit;
    color: var(--fg);
    text-align: left;
    padding: 0;
    cursor: pointer;
  }

  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--warn);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--warn) 16%, transparent);
  }

  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-md);
  }

  .chip {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 999px;
    padding: 0 6px;
  }

  .why {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--warn);
    text-align: right;
  }

  .open {
    flex: none;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .top:hover .open {
    color: var(--fg);
  }

  /* The inline permission card is the chat component verbatim; give it a
     quiet inset so it reads as the session's ask, not dashboard chrome. */
  .perm {
    min-width: 0;
  }

  .ask {
    align-self: flex-start;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--muted);
    cursor: pointer;
    padding: 0;
  }
  .ask:hover {
    color: var(--fg);
  }
</style>

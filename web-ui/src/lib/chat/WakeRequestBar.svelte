<script lang="ts">
  /**
   * Another agent's message waiting for the user's OK to wake this idle
   * agent (agent communication's wake request), pinned above the composer
   * of the agent it would wake — where the user is looking — as well as in
   * the dashboard's Needs you. **Wake** delivers every waiting message as
   * one turn, billed to the user (**Continue** past a limit: a conversation
   * that went back and forth, or agents that kept waking each other while
   * the user was away — one Continue lets every held agent go on); **Leave**
   * keeps them in the inbox (the agent reads them when it next works, or
   * with the user's next prompt).
   * A refused answer stays as the bar with the daemon's words until
   * dismissed. The message is an agent's words: plain text, one line.
   */
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import { dismissWakeFailure, wake, type WakeRequest } from "../workspace/comms.svelte";

  interface Props {
    request: WakeRequest | null;
    /** A refused answer for this agent (the request itself is gone). */
    failure: { request: WakeRequest; message: string } | null;
    /** False while the retained owner is hidden (no entrance motion). */
    visible?: boolean;
  }

  let { request, failure, visible = true }: Props = $props();

  let busy = $state<"wake" | "leave" | null>(null);

  /** Past a limit the question is whether to let them keep going. */
  const cont = $derived(request?.reason === "hop_limit" || request?.reason === "unattended");
  const oneLine = $derived((request?.text ?? "").replace(/\s+/g, " ").trim());
  /** Why chimaera asks although the user lets agents wake each other. */
  const why = $derived(
    request?.reason === "hop_limit"
      ? { words: "back and forth", title: "these two agents have woken each other several times — continue?" }
      : request?.reason === "unattended"
        ? {
            words: "agents kept going",
            title: "agents here have woken each other many times since you last stepped in — Continue lets them keep going",
          }
        : null,
  );

  async function answer(yes: boolean): Promise<void> {
    if (request === null || busy !== null) return;
    busy = yes ? "wake" : "leave";
    try {
      await wake(request.id, yes);
    } catch {
      // The store keeps the refusal; `failure` shows it.
    } finally {
      busy = null;
    }
  }
</script>

{#if failure !== null && request === null}
  <div class="pinned-card wake-card" class:visible role="alert">
    <span class="text err"
      >couldn't {failure.request.reason === "hop_limit" || failure.request.reason === "unattended" ? "continue" : "wake"} — {failure.message}</span
    >
    <span class="actions">
      <button class="opt quiet" onclick={() => dismissWakeFailure(failure.request.id)}>Dismiss</button>
    </span>
  </div>
{:else if request !== null}
  <div class="pinned-card wake-card" class:visible role="group" aria-label="message from {request.from_name} waiting">
    <span class="from">
      {#if request.from_agent !== null}<SessionGlyph kind="agent" agentKind={request.from_agent} size={11} />{/if}
      <span class="name">{request.from_name}</span>
    </span>
    {#if why !== null}<span class="why" title={why.title}>{why.words}</span>{/if}
    <span class="text" title={request.text}>{oneLine}</span>
    <span class="actions">
      <button
        class="opt quiet"
        disabled={busy !== null}
        title="keep it in the inbox — it reads it when it next works"
        onclick={() => void answer(false)}>{busy === "leave" ? "Leaving…" : "Leave"}</button
      >
      <button
        class="opt primary"
        disabled={busy !== null}
        title="deliver it now — a turn billed to your account"
        onclick={() => void answer(true)}
        >{busy === "wake" ? (cont ? "Continuing…" : "Waking…") : cont ? "Continue" : "Wake"}</button
      >
    </span>
  </div>
{/if}

<style>
  /* Chrome (edge, radius, tint, entrance) is the shared .pinned-card recipe
     in app.css, tinted toward attention: something waits on the user. */
  .wake-card {
    --pinned-card-bg: color-mix(in srgb, var(--warn) 5%, var(--bg));
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    padding: 6px 8px 6px 12px;
    border-color: color-mix(in srgb, var(--warn) 28%, var(--edge));
    font-size: var(--text-sm);
  }
  .from {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    max-width: 40%;
    min-width: 0;
    color: var(--fg);
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }
  .why {
    flex: none;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .text {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
  }
  .text.err {
    color: var(--err);
  }
  .actions {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
</style>

<script lang="ts">
  /**
   * Plan approval (claude ExitPlanMode): the agent proposes its plan and asks
   * to leave plan mode. Renders the plan markdown itself plus the official
   * three answers — "Yes, and auto-accept edits" / "Yes, manually approve" /
   * "No, keep planning" — with an optional comment that rides the decision
   * (approvals: updatedInput.userFeedback/userComments; keep-planning: the
   * feedback-denial). Option ids/labels come from the driver verbatim.
   */
  import Markdown from "./Markdown.svelte";
  import type { ResolvePaths } from "./paths";
  import type { PendingPermission } from "./store.svelte";

  interface Props {
    request: PendingPermission;
    onDecide: (optionId: string, feedback?: string) => void;
    onOpenPath?: (path: string, kind: "file" | "dir") => void;
    resolvePaths?: ResolvePaths;
  }

  let { request, onDecide, onOpenPath, resolvePaths }: Props = $props();
  let comment = $state("");

  function decide(optionId: string) {
    const text = comment.trim();
    onDecide(optionId, text.length > 0 ? text : undefined);
  }

  let cardEl = $state<HTMLDivElement | null>(null);
  // Like the permission card: capture input on arrival, without forcing a
  // scroll (ChatView's own effect handles stick-to-bottom).
  $effect(() => {
    cardEl?.focus({ preventScroll: true });
  });

  /** Enter (card focused) = the first option — "Yes, and auto-accept edits",
   *  the TUI's default; Esc = keep planning. Enter inside the comment input
   *  is deliberately inert: a typed comment can accompany ANY of the three
   *  answers, so the decision stays an explicit button press. */
  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && e.target === e.currentTarget && request.options.length > 0) {
      e.preventDefault();
      decide(request.options[0].id);
    } else if (e.key === "Escape") {
      const keep = request.options.find((o) => o.kind.startsWith("reject"));
      if (keep) {
        e.preventDefault();
        decide(keep.id);
      }
    }
  }
</script>

<!-- Focusable container so Enter/Esc answer without mousing; the buttons
     below remain the accessible path. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="plan-approval" role="group" tabindex="0" bind:this={cardEl} onkeydown={onKeydown}>
  <div class="head">
    <span class="mark">✓</span>
    <span class="label">plan ready — approve to leave plan mode</span>
  </div>
  {#if request.plan !== null && request.plan.length > 0}
    <div class="plan-body">
      <Markdown text={request.plan} {onOpenPath} {resolvePaths} />
    </div>
  {/if}
  <input class="comment" bind:value={comment} placeholder="optional feedback on the plan…" />
  <div class="actions">
    {#each request.options as option, i (option.id)}
      <button
        class="opt"
        class:primary={i === 0 && option.kind.startsWith("allow")}
        class:quiet={option.kind.startsWith("reject")}
        onclick={() => decide(option.id)}
      >
        {option.label}
      </button>
    {/each}
  </div>
</div>

<style>
  .plan-approval {
    border: 1px solid color-mix(in srgb, var(--accent) 40%, var(--edge));
    background: color-mix(in srgb, var(--accent) 5%, transparent);
    border-radius: 8px;
    padding: 10px 12px;
    margin: 6px 0;
    outline: none;
    animation: rise 0.15s ease; /* @keyframes rise lives in app.css */
  }
  @media (prefers-reduced-motion: reduce) {
    .plan-approval {
      animation: none;
    }
  }
  .plan-approval:focus-visible {
    box-shadow: 0 0 0 2px var(--focus-ring);
  }
  .head {
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
    background: var(--accent);
    /* Fixed dark ink, matching the permission card's mark: a theme-tracking
       ink fails on the light side of a mid-tone disc. */
    color: rgba(0, 0, 0, 0.8);
    display: grid;
    place-items: center;
    font-size: 10px;
    font-weight: 700;
  }
  .label {
    flex: 1;
    min-width: 0;
    color: var(--fg);
  }
  .plan-body {
    margin: 8px 0 0;
    padding: 8px 10px;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
    border: 1px solid color-mix(in srgb, var(--edge) 60%, transparent);
    border-radius: 6px;
    font-size: var(--text-sm);
    max-height: 320px;
    overflow: auto;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }
  .comment {
    margin-top: 8px;
    width: 100%;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 4px 10px;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
  }
  .comment:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
  }
  /* Base .opt / .opt.primary / .opt.quiet live in app.css; the reject turns
     danger-red on hover, matching the permission card's affordance. */
  .opt.quiet:hover {
    color: var(--err);
    border-color: color-mix(in srgb, var(--err) 45%, var(--edge));
  }
</style>

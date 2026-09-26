<script lang="ts">
  /**
   * "Now" — who is running, as ONE line (decision 1: the rail has the
   * detail, the dashboard stops competing). Each agent is its name in mono
   * plus a short honest phrase from the same state vocabulary as the rail;
   * terminals fold into a count. "show cards" flips the roster setting.
   */
  import { inlineMarkdown } from "../shared/inlineMarkdown";
  import { isBusy, needsAttention, type Session } from "../workspace/sessions";

  interface Props {
    agents: Session[];
    shells: Session[];
    names: Map<string, string>;
    /** The session this window was in most recently (a quiet ↳ mark). */
    continueId: string | null;
    onOpenSession: (id: string) => void;
    onShowCards: () => void;
  }

  let { agents, shells, names, continueId, onOpenSession, onShowCards }: Props = $props();

  const MAX = 6;

  function phrase(s: Session): { text: string; tone: "warn" | "err" | "muted" | "fg"; markdown: boolean } {
    if (!s.alive) return { text: "exited", tone: "muted", markdown: false };
    if (needsAttention(s)) {
      return {
        text: s.agent_state === "errored" ? "errored" : "waiting on you",
        tone: s.agent_state === "errored" ? "err" : "warn",
        markdown: false,
      };
    }
    if (isBusy(s)) {
      const now = s.now_line ?? s.status_detail;
      return now ? { text: now, tone: "fg", markdown: true } : { text: "working", tone: "fg", markdown: false };
    }
    if (s.agent_state === "finished") return { text: "finished", tone: "muted", markdown: false };
    return { text: "idle", tone: "muted", markdown: false };
  }

  const shown = $derived(agents.slice(0, MAX));
  const rest = $derived(agents.length - shown.length);
  const busyShells = $derived(shells.filter(isBusy));
</script>

<div class="now">
  <span class="lbl">Now</span>
  {#each shown as s (s.id)}
    {@const p = phrase(s)}
    <button class="agent" onclick={() => onOpenSession(s.id)} title="open {names.get(s.id) ?? s.name}">
      <span class="name">{names.get(s.id) ?? s.name}</span>
      {#if p.markdown}
        <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
        <span class="what {p.tone}" title={p.text}>{@html inlineMarkdown(p.text)}</span>
      {:else}
        <span class="what {p.tone}">{p.text}</span>
      {/if}
      {#if s.id === continueId && agents.length > 1}
        <span class="last" title="last active — the session you were in most recently">↳</span>
      {/if}
    </button>
  {/each}
  {#if rest > 0}
    <span class="rest">+{rest} more</span>
  {/if}
  {#if shells.length > 0}
    <span class="shells">
      {shells.length} terminal{shells.length === 1 ? "" : "s"}
      {#if busyShells.length === 1}
        · <button class="shell" onclick={() => onOpenSession(busyShells[0].id)}
          >{names.get(busyShells[0].id) ?? busyShells[0].name}</button
        > running
      {:else if busyShells.length > 1}
        · {busyShells.length} running
      {/if}
    </span>
  {/if}
  <button class="link" onclick={onShowCards}>show cards</button>
</div>

<style>
  .now {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px 14px;
    font-size: var(--text-sm);
    color: var(--muted);
    padding-top: 12px;
    border-top: 1px solid var(--edge);
    min-width: 0;
  }
  .lbl {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .agent {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--muted);
    cursor: pointer;
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    max-width: 320px;
  }
  .agent:hover .name {
    text-decoration: underline;
  }
  .name {
    font-family: var(--mono);
    color: var(--fg);
    flex: none;
  }
  .what {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .what :global(code) {
    font-family: var(--mono);
    font-size: 0.92em;
  }
  .what.warn {
    color: var(--warn);
  }
  .what.err {
    color: var(--err);
  }
  .what.fg {
    color: var(--muted);
  }
  .last {
    color: var(--accent);
    flex: none;
  }
  .rest,
  .shells {
    white-space: nowrap;
  }
  .shell {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
    cursor: pointer;
  }
  .shell:hover {
    text-decoration: underline;
  }
  .link {
    margin-left: auto;
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--accent);
    cursor: pointer;
    white-space: nowrap;
  }
  .link:hover {
    text-decoration: underline;
  }
</style>

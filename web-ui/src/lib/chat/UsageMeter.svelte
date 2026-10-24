<script lang="ts">
  import { toolbarPopover } from "../shared/toolbarPopover";
  import UsageBars from "../shared/UsageBars.svelte";
  import Switch from "../shared/Switch.svelte";
  import { formatTokenCount } from "../shared/format";
  import { usageHeadline, usageTitle, type UsageRow } from "../shared/usage";

  /**
   * The chat header's usage: a faint gauge whose needle sits at the fullest
   * reading, and nothing else, until something nears its edge (80%) — then
   * that reading's name and number, warn-coloured, and err once a limit is
   * spent. A click opens the context window (with Compact) and every plan
   * window this chat knows, each with when it resets. Nothing shows for an
   * agent that reports neither (maintainer, 2026-10-15: quiet unless it
   * matters, the full picture one click away, the account's own view in
   * Agent settings).
   */
  interface Props {
    contextPct: number | null;
    contextTokens: { total: number; max: number } | null;
    limits: UsageRow[];
    limitReached: boolean;
    open: boolean;
    onToggle: () => void;
    onClose: () => void;
    /** Offered while the agent is idle. */
    onCompact?: () => void;
    /** Whether this chat compacts on its own; null where the agent has no
     *  per-chat switch (Codex always does near a full window). */
    autoCompact?: boolean | null;
    /** The environment variable holding it off: the switch reads Off and
     *  cannot be turned on here. */
    autoCompactDisabledBy?: string | null;
    onSetAutoCompact?: (enabled: boolean) => void;
    /** Where the agent compacts on its own, in tokens of the context window,
     *  when it applies a threshold someone set; null at its own default. */
    compactAt?: number | null;
    /** Whose threshold it is, for the one-line explanation on hover. */
    agentName?: string;
    /** Codex checks its threshold when a turn's answer lands. */
    compactsAfterTurn?: boolean;
    onOpenSettings?: () => void;
  }

  let {
    contextPct,
    contextTokens,
    limits,
    limitReached,
    open,
    onToggle,
    onClose,
    onCompact,
    autoCompact = null,
    autoCompactDisabledBy = null,
    onSetAutoCompact,
    compactAt = null,
    agentName = "The agent",
    compactsAfterTurn = false,
    onOpenSettings,
  }: Props = $props();

  const headline = $derived(usageHeadline(contextPct, limits, limitReached));
  const fullest = $derived(
    Math.max(contextPct ?? 0, ...limits.map((row) => row.utilization), limitReached ? 100 : 0),
  );
  /** The needle: 0% points left, 100% right, over the gauge's arc. */
  const needle = $derived.by(() => {
    const angle = Math.PI * (1 - Math.min(100, Math.max(0, fullest)) / 100);
    return { x: 8 + 4.4 * Math.cos(angle), y: 11 - 4.4 * Math.sin(angle) };
  });
  const summary = $derived(
    [
      contextPct !== null ? `Context ${Math.floor(contextPct)}%` : null,
      ...limits.map((row) => `${usageTitle(row.label)} ${Math.floor(row.utilization)}%`),
    ]
      .filter((part) => part !== null)
      .join(" · "),
  );
  /** The threshold as a point on the context bar: only while the agent
   *  compacts on its own (a chat switched off, or held off by the
   *  environment, draws none). */
  const compactAtPct = $derived.by(() => {
    if (compactAt === null || contextTokens === null || contextTokens.max <= 0) return null;
    if (autoCompact === false || autoCompactDisabledBy !== null) return null;
    const pct = (compactAt / contextTokens.max) * 100;
    return pct > 0 && pct < 100 ? pct : null;
  });
  const contextRow = $derived<UsageRow[]>(
    contextPct === null
      ? []
      : [{ label: "context", utilization: contextPct, resetsAt: null, mark: compactAtPct }],
  );
  const compactAtTitle = $derived(
    compactAtPct === null || compactAt === null
      ? ""
      : `${agentName} summarizes the conversation once it reaches ${formatTokenCount(compactAt, true)} tokens` +
          (compactsAfterTurn ? ", when a turn ends past it" : "") +
          ".",
  );
  let now = $state(Date.now());
  $effect(() => {
    if (open) now = Date.now();
  });
</script>

{#if contextPct !== null || limits.length > 0 || limitReached}
  <div class="menu-host usage-meter">
    <button
      class="usage-btn"
      class:warn={headline?.level === "warn"}
      class:full={headline?.level === "full"}
      aria-haspopup="dialog"
      aria-expanded={open}
      aria-label={`Usage: ${summary}${limitReached ? " · limit reached" : ""}`}
      title={summary}
      onclick={onToggle}
    >
      <svg class="gauge" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
        <path d="M2.6 11a5.4 5.4 0 0 1 10.8 0" class="arc" />
        <path d="M8 11L{needle.x.toFixed(2)} {needle.y.toFixed(2)}" class="needle" />
        <circle cx="8" cy="11" r="1.1" class="hub" />
      </svg>
      {#if headline !== null}
        <span class="what">{headline.level === "full" && limitReached ? `${headline.label} limit reached` : headline.label}</span>
        {#if !(headline.level === "full" && limitReached)}<span class="pct">{Math.floor(headline.pct)}%</span>{/if}
      {/if}
    </button>
    {#if open}
      <div class="overlay-surface usage-pop" role="dialog" aria-label="Usage" use:toolbarPopover={{ onClose }}>
        {#if contextRow.length > 0}
          <UsageBars rows={contextRow} {now} />
          <div class="context-line">
            {#if contextTokens !== null}
              <span>{formatTokenCount(contextTokens.total, true)} of {formatTokenCount(contextTokens.max, true)} tokens</span>
            {/if}
            {#if onCompact}
              <button class="link" onclick={onCompact} title="Summarize the conversation so far to free the context window">Compact</button>
            {/if}
          </div>
          {#if autoCompact !== null && onSetAutoCompact}
            <div
              class="auto-line"
              title={compactAtTitle ||
                "Summarize the conversation on its own when the context window fills. Off: it waits for Compact, and a full window stops the turn."}
            >
              <span
                >Auto-compact{#if compactAtPct !== null}<span class="at">{` at ${Math.round(compactAtPct)}%`}</span>{/if}</span
              >
              <Switch
                on={autoCompact && !autoCompactDisabledBy}
                disabled={!!autoCompactDisabledBy}
                label="Auto-compact this chat"
                onToggle={onSetAutoCompact}
              />
            </div>
            {#if autoCompactDisabledBy}
              <p class="held">set off by <code>{autoCompactDisabledBy}</code></p>
            {/if}
          {:else if compactAtPct !== null}
            <div class="auto-line" title={compactAtTitle}>
              <span>Auto-compact<span class="at">{` at ${Math.round(compactAtPct)}%`}</span></span>
            </div>
          {/if}
        {/if}
        {#if limits.length > 0}
          {#if contextRow.length > 0}<hr />{/if}
          <UsageBars rows={limits} {now} refused={limitReached} />
        {:else if limitReached}
          {#if contextRow.length > 0}<hr />{/if}
          <p class="note full">The plan's usage limit is reached.</p>
        {/if}
        {#if onOpenSettings}
          <button class="overlay-row settings" onclick={onOpenSettings}>
            {limits.length > 0 ? "All limits" : "Plan limits"}<span aria-hidden="true">›</span>
          </button>
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .usage-meter {
    display: inline-flex;
    flex: none;
    min-width: 0;
  }
  .usage-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: var(--pane-control-size);
    min-width: 0;
    padding: 0 5px;
    font: inherit;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    border: 1px solid transparent;
    border-radius: 6px;
    background: none;
    /* Faint at rest: present, never asking for attention. */
    color: color-mix(in srgb, var(--muted) 65%, transparent);
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }
  .usage-btn:hover,
  .usage-btn:focus-visible,
  .usage-btn[aria-expanded="true"] {
    color: var(--fg);
    background: var(--hover-tint);
  }
  .usage-btn:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: -2px;
  }
  .usage-btn.warn {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 40%, var(--edge));
    animation: rise 0.18s ease;
  }
  .usage-btn.full {
    color: var(--err);
    border-color: color-mix(in srgb, var(--err) 50%, var(--edge));
    animation: rise 0.18s ease;
  }
  .gauge {
    flex: none;
  }
  .gauge .arc,
  .gauge .needle {
    fill: none;
    stroke: currentColor;
    stroke-linecap: round;
  }
  .gauge .arc {
    stroke-width: 1.4;
    opacity: 0.55;
  }
  .gauge .needle {
    stroke-width: 1.5;
    transition: d 0.3s ease;
  }
  .gauge .hub {
    fill: currentColor;
  }
  .what {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  /* In a narrow pane the number alone says it; the popover names it. */
  @container pane-chrome (max-width: 620px) {
    .what {
      display: none;
    }
    .usage-btn.full .what {
      display: inline;
      max-width: 72px;
    }
  }
  .usage-pop {
    width: 280px;
    padding: 10px 12px 6px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .context-line {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: -3px;
    font-size: var(--text-xs);
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .context-line .link {
    margin-left: auto;
    padding: 1px 6px;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--fg);
    background: none;
    border: 1px solid var(--edge);
    border-radius: 5px;
    cursor: pointer;
  }
  .context-line .link:hover,
  .context-line .link:focus-visible {
    background: var(--hover-tint);
  }
  .auto-line {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-top: -2px;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .auto-line .at {
    font-variant-numeric: tabular-nums;
  }
  .held {
    margin: -4px 0 0;
    font-size: var(--text-xs);
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .held code {
    font-family: var(--mono);
    font-size: inherit;
  }
  hr {
    width: 100%;
    margin: 2px 0;
    border: 0;
    border-top: 1px solid var(--edge);
  }
  .note {
    margin: 0;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .note.full {
    color: var(--err);
  }
  .settings {
    display: flex;
    justify-content: space-between;
    margin: 0 -8px;
    width: calc(100% + 16px);
    color: var(--muted);
    font-size: var(--text-xs);
  }
  .settings:hover,
  .settings:focus-visible {
    color: var(--fg);
  }
  @media (prefers-reduced-motion: reduce) {
    .usage-btn.warn,
    .usage-btn.full {
      animation: none;
    }
    .gauge .needle {
      transition: none;
    }
  }
</style>

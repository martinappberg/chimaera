<script lang="ts">
  /**
   * The chat header strip: identity chip, the model / permission-mode / effort
   * pickers, ultracode + thinking toggles, and the live status chips (stop,
   * rate limit, context). It renders and toggles the shared `menu` state but
   * the picks themselves are the host's callbacks (they ride socket.send).
   */
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import EffortPopover from "./EffortPopover.svelte";
  import type { ChatStore } from "./store.svelte";

  interface ModelChoice {
    id: string;
    label: string;
    resolved?: string | null;
    description?: string | null;
  }

  interface Props {
    store: ChatStore;
    agentKind: string;
    agentName: string;
    /** The one-of-N open overlay; two-way so the host can open /mcp and the
     *  outside-dismiss action can close everything. */
    menu: "model" | "mode" | "effort" | "mcp" | null;
    modelChoices: ModelChoice[];
    modelLabel: string | null;
    modeLabel: string | null;
    hasEffort: boolean;
    effortChoices: string[];
    effortShown: string | null;
    effortHint: string;
    hasUltracode: boolean;
    hasThinking: boolean;
    thinking: boolean;
    onPickModel: (id: string) => void;
    onPickMode: (id: string) => void;
    onPickEffort: (id: string) => void;
    onToggleUltracode: () => void;
    onToggleThinking: () => void;
    onInterrupt: () => void;
  }

  let {
    store,
    agentKind,
    agentName,
    menu = $bindable(),
    modelChoices,
    modelLabel,
    modeLabel,
    hasEffort,
    effortChoices,
    effortShown,
    effortHint,
    hasUltracode,
    hasThinking,
    thinking,
    onPickModel,
    onPickMode,
    onPickEffort,
    onToggleUltracode,
    onToggleThinking,
    onInterrupt,
  }: Props = $props();
</script>

{#snippet caret()}
  <span class="caret">
    <svg viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
      <path
        d="M4 6l4 4 4-4"
        fill="none"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  </span>
{/snippet}

<header class="strip">
  <span class="agent-id" title="{agentName} chat session">
    <SessionGlyph kind="agent" {agentKind} size={11} />
    <span class="agent-name">{agentName}</span>
  </span>
  <div class="menu-host">
    <button
      class="chip pick"
      title={store.model === null ? "resolving model…" : "model — click to switch"}
      aria-haspopup="menu"
      aria-expanded={menu === "model"}
      onclick={() => (menu = menu === "model" ? null : "model")}
    >
      <!-- store.model stays null until the first init/ready payload resolves —
           show a neutral skeleton then, never a concrete (wrong) default name. -->
      {#if store.model === null}
        <span class="model-skel" aria-label="loading model"></span>
      {:else}
        {modelLabel ?? "model"}
      {/if}
      {@render caret()}
    </button>
    {#if menu === "model"}
      <div class="overlay-surface menu" role="menu" aria-label="model">
        {#if modelChoices.length === 0}
          <span class="menu-empty">no known models</span>
        {/if}
        {#each modelChoices as m (m.id)}
          <button
            class="overlay-row menu-row"
            class:current={m.id === store.model || m.resolved === store.model}
            role="menuitem"
            title={typeof m.description === "string" ? m.description : undefined}
            onclick={() => onPickModel(m.id)}
          >
            {m.label}
          </button>
        {/each}
      </div>
    {/if}
  </div>
  {#if store.modes.length > 0}
    <div class="menu-host">
      <button
        class="chip pick"
        title="permission mode — click to switch"
        aria-haspopup="menu"
        aria-expanded={menu === "mode"}
        onclick={() => (menu = menu === "mode" ? null : "mode")}
      >
        {modeLabel ?? "mode"}
        {@render caret()}
      </button>
      {#if menu === "mode"}
        <div class="overlay-surface menu" role="menu" aria-label="permission mode">
          {#each store.modes as m (m.id)}
            <button
              class="overlay-row menu-row"
              class:current={m.id === store.currentMode}
              role="menuitem"
              onclick={() => onPickMode(m.id)}
            >
              {m.label}
            </button>
          {/each}
        </div>
      {/if}
    </div>
  {/if}
  {#if hasEffort}
    <div class="menu-host">
      <button
        class="chip pick"
        title={effortHint}
        aria-haspopup="menu"
        aria-expanded={menu === "effort"}
        onclick={() => (menu = menu === "effort" ? null : "effort")}
      >
        {effortShown ?? "effort"}
        {@render caret()}
      </button>
      {#if menu === "effort"}
        <EffortPopover choices={effortChoices} shown={effortShown} onPick={onPickEffort} />
      {/if}
    </div>
  {/if}
  {#if hasUltracode}
    <button
      class="chip pick"
      class:on={store.ultracode}
      title="ultracode — xhigh effort + standing workflow orchestration, this session only"
      aria-pressed={store.ultracode}
      onclick={onToggleUltracode}
    >
      ultracode{store.ultracode ? " on" : " off"}
    </button>
  {/if}
  {#if hasThinking}
    <button
      class="chip pick"
      class:on={thinking === true}
      title="extended thinking — applies from your next message"
      aria-pressed={thinking === true}
      onclick={onToggleThinking}
    >
      thinking{thinking ? " on" : " off"}
    </button>
  {/if}
  <span class="spacer"></span>
  {#if store.running}
    <button class="stop" onclick={onInterrupt} title="interrupt the agent (Esc)">stop</button>
  {/if}
  {#if store.rateLimit !== null && (store.rateLimit.limitReached || store.rateLimit.utilization >= 80)}
    <span
      class="ratelimit"
      class:hit={store.rateLimit.limitReached}
      title={store.rateLimit.resetsAt !== null
        ? `resets ${new Date(Number(store.rateLimit.resetsAt) * 1000).toLocaleString()}`
        : "account rate limit"}
    >
      {store.rateLimit.label ?? "usage limit"}
      {store.rateLimit.limitReached ? "reached" : `${Math.floor(store.rateLimit.utilization)}%`}
    </span>
  {/if}
  {#if store.contextPct !== null}
    <span
      class="ctx"
      class:full={store.contextPct >= 80}
      title={store.contextTokens !== null
        ? `context window: ${store.contextTokens.total.toLocaleString()} / ${store.contextTokens.max.toLocaleString()} tokens`
        : "context window used"}
    >
      {Math.round(store.contextPct)}% ctx
    </span>
  {/if}
</header>

<style>
  .strip {
    display: flex;
    align-items: center;
    flex-wrap: wrap; /* narrow panes get a clean second chip row, not clipping */
    gap: 4px 6px;
    padding: 4px 10px;
    border-bottom: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
    flex: none;
  }
  .menu-host {
    position: relative;
  }
  .agent-id {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--fg);
    font-family: var(--mono);
    flex: none;
    padding-right: 4px;
    border-right: 1px solid var(--edge);
    margin-right: 2px;
  }
  .agent-name {
    white-space: nowrap;
  }
  .chip {
    border: 1px solid var(--edge);
    border-radius: 999px;
    padding: 0 8px;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 18px;
    /* Fixed-height pill: the label must clip, never wrap out of it. */
    white-space: nowrap;
    min-width: 0;
    overflow: hidden;
  }
  .chip.pick {
    background: none;
    color: var(--muted);
    font: inherit;
    font-family: var(--mono);
    cursor: pointer;
    transition:
      color 0.12s ease,
      border-color 0.12s ease;
  }
  .chip.pick:hover {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--accent) 40%, var(--edge));
  }
  /* Shared "toggle is on" treatment for the ultracode + thinking chips: an
     accent tint so an active toggle reads at a glance, not just from its label. */
  .chip.on {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 10%, transparent);
  }
  .caret {
    display: inline-flex;
    opacity: 0.7;
  }
  /* Neutral loading placeholder for the model chip: a short muted bar that
     breathes, so the header reads "resolving" rather than flashing a wrong
     default. Static (no pulse) under reduced-motion — it's just a placeholder. */
  .model-skel {
    display: inline-block;
    width: 42px;
    height: 8px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--fg) 22%, transparent);
    animation: skel-pulse 1.4s ease-in-out infinite;
  }
  @keyframes skel-pulse {
    0%,
    100% {
      opacity: 0.5;
    }
    50% {
      opacity: 0.9;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .model-skel {
      animation: none;
    }
  }
  /* .overlay-surface / .overlay-row live in app.css; .menu / .menu-row add the
     dropdown anchor and the menu-item specifics. */
  .menu {
    top: 100%;
    left: 0;
    margin-top: 4px;
    min-width: 180px;
    z-index: 20;
  }
  .menu-row {
    display: block;
  }
  .menu-row.current {
    color: var(--accent);
  }
  .menu-empty {
    display: block;
    padding: 6px 12px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .spacer {
    flex: 1;
  }
  .stop {
    font: inherit;
    font-size: var(--text-xs);
    border: 1px solid color-mix(in srgb, var(--err) 50%, var(--edge));
    color: var(--err);
    background: none;
    border-radius: 5px;
    padding: 0 10px;
    line-height: 16px;
    cursor: pointer;
    transition: background-color 0.12s ease;
  }
  .stop:hover {
    background: color-mix(in srgb, var(--err) 10%, transparent);
  }
  .ctx {
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }
  .ctx.full {
    color: var(--warn);
  }
  .ratelimit {
    font-variant-numeric: tabular-nums;
    color: var(--warn);
    border: 1px solid color-mix(in srgb, var(--warn) 45%, var(--edge));
    border-radius: 999px;
    padding: 0 8px;
    height: 18px;
    display: inline-flex;
    align-items: center;
    animation: rise 0.18s ease; /* @keyframes rise lives in app.css */
  }
  .ratelimit.hit {
    color: var(--err);
    border-color: color-mix(in srgb, var(--err) 55%, var(--edge));
  }
  @media (prefers-reduced-motion: reduce) {
    .ratelimit {
      animation: none;
    }
  }
</style>

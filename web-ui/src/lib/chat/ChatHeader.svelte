<script lang="ts">
  /**
   * The chat header strip: identity chip, the model / permission-mode / effort
   * pickers, ultracode + thinking toggles, and the live status chips (stop,
   * rate limit, context). It renders and toggles the shared `menu` state but
   * the picks themselves are the host's callbacks (they ride socket.send).
   */
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import EffortPopover from "./EffortPopover.svelte";
  import { copyText } from "../shared/clipboard";
  import { openInSystemBrowser } from "../shared/urlOpen";
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
    menu: "model" | "mode" | "effort" | "mcp" | "remote" | null;
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
    /** Turn the agent's Remote Control bridge on/off (claude: the
     *  `remote_control` control; codex answers with a pointer to its daemon). */
    onSetRemoteControl: (enabled: boolean) => void;
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
    onSetRemoteControl,
  }: Props = $props();

  // Remote Control: the chip shows where the bridge stands; the popover
  // carries the one action plus the session link. Offered for claude when
  // the CLI says so; a codex row appears only once its daemon reports a
  // live state (its bridge is not switchable from here).
  const rc = $derived(store.remoteControl);
  const rcShown = $derived(store.remoteControlAvailable || rc !== null);
  const rcState = $derived<"off" | "connecting" | "connected" | "error">(rc?.state ?? "off");
  const rcLabel = $derived(
    rcState === "connected"
      ? "remote on"
      : rcState === "connecting"
        ? "remote…"
        : rcState === "error"
          ? "remote ✕"
          : "remote off",
  );
  const rcTitle = $derived(
    rcState === "connected"
      ? `Remote Control on${rc?.name ? ` — ${rc.name}` : ""}: pick this session up in the Claude app or at claude.ai/code`
      : rcState === "connecting"
        ? "Remote Control connecting…"
        : rcState === "error"
          ? `Remote Control: ${rc?.detail ?? "could not connect"}`
          : "Remote Control off — take this session with you on your other devices",
  );
  let rcCopied = $state(false);
  let rcCopiedTimer: ReturnType<typeof setTimeout> | null = null;
  function copyRemoteLink() {
    const url = rc?.sessionUrl;
    if (!url) return;
    // Same contract as the other copy affordances: "Copied" only when a
    // write happened, one timer at a time, torn down with the component.
    void copyText(url).then((ok) => {
      if (!ok) return;
      rcCopied = true;
      if (rcCopiedTimer !== null) clearTimeout(rcCopiedTimer);
      rcCopiedTimer = setTimeout(() => {
        rcCopied = false;
        rcCopiedTimer = null;
      }, 1400);
    });
  }
  $effect(() => () => {
    if (rcCopiedTimer !== null) clearTimeout(rcCopiedTimer);
  });
  function openRemote() {
    const url = rc?.sessionUrl;
    if (url) openInSystemBrowser(url);
  }
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
      title={modelLabel === null ? "resolving model…" : "model — click to switch"}
      aria-haspopup="menu"
      aria-expanded={menu === "model"}
      onclick={() => (menu = menu === "model" ? null : "model")}
    >
      <!-- Skeleton only in the brief window before the model catalog loads
           (modelLabel null). Once it's loaded, modelLabel is the session's real
           model — or the default a fresh chat will use — never a wrong flash. -->
      {#if modelLabel === null}
        <span class="model-skel" aria-label="loading model"></span>
      {:else}
        {modelLabel}
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
  {#if rcShown}
    <div class="menu-host">
      <button
        class="chip pick rc"
        class:on={rcState === "connected"}
        class:busy={rcState === "connecting"}
        class:err={rcState === "error"}
        title={rcTitle}
        aria-haspopup="menu"
        aria-expanded={menu === "remote"}
        onclick={() => (menu = menu === "remote" ? null : "remote")}
      >
        <span class="rc-dot" aria-hidden="true"></span>
        {rcLabel}
        {@render caret()}
      </button>
      {#if menu === "remote"}
        <div class="overlay-surface menu rc-menu" role="menu" aria-label="remote control">
          <div class="rc-head">
            <span class="rc-dot big" class:on={rcState === "connected"} class:busy={rcState === "connecting"} class:err={rcState === "error"} aria-hidden="true"></span>
            <span class="rc-title">Remote Control</span>
            <span class="rc-state">
              {rcState === "connected"
                ? "connected"
                : rcState === "connecting"
                  ? "connecting…"
                  : rcState === "error"
                    ? "failed"
                    : "off"}
            </span>
          </div>
          <p class="rc-blurb">
            {#if rcState === "error"}
              {rc?.detail ?? "The agent could not open its bridge."}
            {:else if rcState === "off"}
              The session keeps running here; your phone or claude.ai/code becomes the remote.
            {:else}
              Pick this session up in the Claude mobile app, or open it on claude.ai/code.
              {#if rc?.name}Registered as <b>{rc.name}</b>.{/if}
            {/if}
          </p>
          {#if agentKind === "codex"}
            <p class="rc-blurb rc-fine">
              Codex's bridge belongs to its app-server daemon: <code>codex remote-control start</code>, then
              <code>codex remote-control pair</code> on this host.
            </p>
          {:else if rcState === "connected" || rcState === "connecting"}
            {#if rc?.sessionUrl}
              <button class="overlay-row menu-row rc-row" role="menuitem" onclick={openRemote}>
                Open on claude.ai/code <span class="rc-ext" aria-hidden="true">↗</span>
              </button>
              <button class="overlay-row menu-row rc-row" role="menuitem" onclick={copyRemoteLink}>
                {rcCopied ? "Copied" : "Copy session link"}
              </button>
            {/if}
            <button class="overlay-row menu-row rc-row rc-off" role="menuitem" onclick={() => { onSetRemoteControl(false); menu = null; }}>
              Turn off Remote Control
            </button>
          {:else}
            <button class="overlay-row menu-row rc-row rc-on" role="menuitem" onclick={() => { onSetRemoteControl(true); menu = null; }}>
              {rcState === "error" ? "Try again" : "Turn on Remote Control"}
            </button>
            <p class="rc-blurb rc-fine">Opens a secure connection to claude.ai. Also <code>/remote-control</code>.</p>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
  <span class="spacer"></span>
  {#if store.running || store.compacting}
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
    height: calc(var(--text-xs) + 6px);
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
  /* Remote Control chip: the same pill as its siblings, plus a state dot —
     muted when off, breathing while connecting, accent when live, warn on a
     refusal. The dot carries the state at a glance; the label spells it. */
  .chip.rc {
    gap: 5px;
  }
  .chip.rc.err {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 50%, var(--edge));
  }
  .rc-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: color-mix(in srgb, var(--muted) 55%, transparent);
    flex: none;
    transition: background-color 0.15s ease;
  }
  .rc-dot.big {
    width: 8px;
    height: 8px;
  }
  .chip.rc.on .rc-dot,
  .rc-dot.on {
    background: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 22%, transparent);
  }
  .chip.rc.busy .rc-dot,
  .rc-dot.busy {
    background: var(--accent);
    animation: pulse 1.4s ease-in-out infinite; /* shared keyframe in app.css */
  }
  .chip.rc.err .rc-dot,
  .rc-dot.err {
    background: var(--warn);
  }
  /* Infinite "presence" animations pause while the app is hidden (the
     html.app-hidden contract; see app.css). */
  :global(html.app-hidden) .chip.rc.busy .rc-dot,
  :global(html.app-hidden) .rc-dot.busy {
    animation-play-state: paused;
  }
  @media (prefers-reduced-motion: reduce) {
    .chip.rc.busy .rc-dot,
    .rc-dot.busy {
      animation: none;
      opacity: 0.8;
    }
  }
  /* Two classes: the generic .menu rule (declared later) would otherwise
     win the min-width tie. */
  .menu.rc-menu {
    min-width: 300px;
    max-width: 340px;
    padding-bottom: 4px;
  }
  .rc-head {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 8px 12px 4px;
  }
  .rc-title {
    color: var(--fg);
    font-weight: 600;
    white-space: nowrap;
  }
  .rc-state {
    margin-left: auto;
    font-family: var(--mono);
    color: var(--muted);
  }
  .rc-blurb {
    margin: 0;
    padding: 2px 12px 8px;
    font-size: var(--text-sm);
    line-height: 1.4;
    color: var(--muted);
  }
  .rc-blurb b {
    color: var(--fg);
    font-weight: 500;
  }
  .rc-fine {
    font-size: var(--text-xs);
    padding-top: 0;
  }
  .rc-fine code {
    font-family: var(--mono);
    color: var(--fg);
  }
  .rc-row {
    display: flex;
    align-items: center;
    gap: 6px;
    white-space: nowrap;
  }
  .rc-row.rc-on {
    color: var(--accent);
  }
  .rc-row.rc-off {
    color: var(--muted);
  }
  .rc-ext {
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
    line-height: 1.35;
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
    height: calc(var(--text-xs) + 6px);
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

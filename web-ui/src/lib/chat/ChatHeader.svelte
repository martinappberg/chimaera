<script lang="ts">
  /**
   * The chat header strip: identity chip, the model / permission-mode / effort
   * pickers, ultracode + thinking toggles, and the live status chips (stop,
   * rate limit, context). It renders and toggles the shared `menu` state but
   * the picks themselves are the host's callbacks (they ride socket.send).
   */
  import { toolbarPopover } from "../shared/toolbarPopover";
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
    menu: "model" | "mode" | "effort" | "mcp" | "remote" | "options" | null;
    canPickModel: boolean;
    canPickMode: boolean;
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
    canPickModel,
    canPickMode,
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

  // Remote Control: Chat options shows where the bridge stands; the popover
  // carries the one action plus the session link. Offered for claude when
  // the CLI says so; a codex row appears only once its daemon reports a
  // live state (its bridge is not switchable from here).
  const rc = $derived(store.remoteControl);
  const rcShown = $derived(store.remoteControlAvailable || rc !== null);
  const rcState = $derived<"off" | "connecting" | "connected" | "error">(rc?.state ?? "off");
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

<div class="chat-header">
<header class="strip" class:no-mode={!canPickMode || store.modes.length === 0} class:no-effort={!hasEffort}>
  <span class="agent-id" title="{agentName} chat session">
    <SessionGlyph kind="agent" {agentKind} size={11} />
    <span class="agent-name">{agentName}</span>
  </span>

  <div class="menu-host primary-picker model-picker">
    <button
      class="chip pick"
      disabled={!canPickModel}
      title={canPickModel ? (modelLabel === null ? "Resolving model…" : `Model: ${modelLabel}`) : "Model selected by the agent"}
      aria-label={`Model: ${modelLabel ?? "agent default"}`}
      aria-haspopup="menu"
      aria-expanded={menu === "model"}
      onclick={() => (menu = menu === "model" ? null : "model")}
    >
      <!-- A catalog is not an active model. Keep startup neutral until the
           session or an acknowledged selection names the model. -->
      {#if modelLabel === null && !store.initialized && !store.exited && !store.fatalError}
        <span class="model-skel" aria-label="loading model"></span>
      {:else}
        <span class="pick-label">{modelLabel ?? "agent default"}</span>
      {/if}
      {#if canPickModel}{@render caret()}{/if}
    </button>
    {#if canPickModel && menu === "model"}
      <div class="overlay-surface menu" use:toolbarPopover={{ onClose: () => (menu = null) }} role="menu" aria-label="model">
        {#if modelChoices.length === 0}
          <span class="menu-empty">no known models</span>
        {/if}
        {#each modelChoices as m (m.id)}
          <button
            class="overlay-row menu-row"
            class:current={m.id === (store.pendingModel ?? store.model) || m.resolved === (store.pendingModel ?? store.model)}
            role="menuitemradio"
            aria-checked={m.id === (store.pendingModel ?? store.model) || m.resolved === (store.pendingModel ?? store.model)}
            title={typeof m.description === "string" ? m.description : undefined}
            onclick={() => onPickModel(m.id)}
          >
            {m.label}
          </button>
        {/each}
      </div>
    {/if}
  </div>
  {#if canPickMode && store.modes.length > 0}
    <div class="menu-host primary-picker mode-picker">
      <button
        class="chip pick"
        title={`Permission mode: ${modeLabel ?? "mode"}`}
        aria-label={`Permission mode: ${modeLabel ?? "mode"}`}
        aria-haspopup="menu"
        aria-expanded={menu === "mode"}
        onclick={() => (menu = menu === "mode" ? null : "mode")}
      >
        <span class="pick-label">{modeLabel ?? "mode"}</span>
        {@render caret()}
      </button>
      {#if menu === "mode"}
        <div class="overlay-surface menu" use:toolbarPopover={{ onClose: () => (menu = null) }} role="menu" aria-label="permission mode">
          {#each store.modes as m (m.id)}
            <button
              class="overlay-row menu-row"
              class:current={m.id === store.currentMode}
              role="menuitemradio"
              aria-checked={m.id === store.currentMode}
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
    <div class="menu-host primary-picker effort-picker">
      <button
        class="chip pick"
        title={effortHint}
        aria-label={`Reasoning effort: ${effortShown ?? "default"}`}
        aria-haspopup="menu"
        aria-expanded={menu === "effort"}
        onclick={() => (menu = menu === "effort" ? null : "effort")}
      >
        <span class="pick-label">{effortShown ?? "effort"}</span>
        {@render caret()}
      </button>
      {#if menu === "effort"}
        <EffortPopover choices={effortChoices} shown={effortShown} onPick={onPickEffort} onClose={() => (menu = null)} />
      {/if}
    </div>
  {/if}
  <div class="menu-host extras">
    <button class="chip more" title="Chat options" aria-label="Chat options" aria-haspopup="menu"
      aria-expanded={menu === "options" || menu === "remote"}
      onclick={() => (menu = menu === "options" ? null : "options")}>
      <svg viewBox="0 0 16 16" width="14" height="14" fill="currentColor" aria-hidden="true"><circle cx="3" cy="8" r="1.2" /><circle cx="8" cy="8" r="1.2" /><circle cx="13" cy="8" r="1.2" /></svg>
    </button>
    {#if menu === "options"}
      <div class="overlay-surface menu options-menu" role="menu" aria-label="Chat options" use:toolbarPopover={{ onClose: () => (menu = null) }}>
        <div class="options-heading">{agentName} · session options</div>
        {#if hasUltracode}
          <button class="overlay-row menu-row" role="menuitemcheckbox" aria-checked={store.ultracode}
            title="xhigh effort + standing workflow orchestration, this session only" onclick={onToggleUltracode}>
            ultracode <span>{store.ultracode ? "on" : "off"}</span>
          </button>
        {/if}
        {#if hasThinking}
          <button class="overlay-row menu-row" role="menuitemcheckbox" aria-checked={thinking}
            title="Extended thinking — applies from your next message" onclick={onToggleThinking}>
            thinking <span>{thinking ? "on" : "off"}</span>
          </button>
        {/if}
        {#if rcShown}
          <button class="overlay-row menu-row" role="menuitem" title={rcTitle} onclick={() => (menu = "remote")}>
            Remote Control <span>{rcState}</span>
          </button>
        {/if}
        <div class="options-heading">
          {store.contextPct === null ? "Context usage not yet available" : `${Math.round(store.contextPct)}% of context used`}
          {#if store.rateLimit !== null && (store.rateLimit.limitReached || store.rateLimit.utilization >= 80)}
            <br />{store.rateLimit.label ?? "Usage limit"}: {store.rateLimit.limitReached ? "reached" : `${Math.floor(store.rateLimit.utilization)}%`}
          {/if}
        </div>
      </div>
    {/if}
      {#if menu === "remote"}
        <div class="overlay-surface menu rc-menu" use:toolbarPopover={{ onClose: () => (menu = null) }} role="menu" aria-label="remote control">
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
  <span class="spacer"></span>
  <div class="session-status">
  {#if store.running || store.compacting}
    <button class="stop" onclick={onInterrupt} aria-label="Stop" title="interrupt the agent (Esc)"><span class="stop-mark" aria-hidden="true"></span><span class="stop-label">Stop</span></button>
  {/if}
  {#if store.rateLimit !== null && (store.rateLimit.limitReached || store.rateLimit.utilization >= 80)}
    <span
      class="ratelimit"
      class:hit={store.rateLimit.limitReached}
      title={`${store.rateLimit.label ?? "Usage limit"}: ${store.rateLimit.limitReached ? "reached" : `${Math.floor(store.rateLimit.utilization)}%`}${store.rateLimit.resetsAt !== null ? ` · resets ${new Date(Number(store.rateLimit.resetsAt) * 1000).toLocaleString()}` : ""}`}
    >
      <span class="rate-label">{store.rateLimit.label ?? "Usage limit"}</span>
      <span class="rate-label-short">{(store.rateLimit.label ?? "Usage").replace(/\s+limit$/i, "")}</span>
      <span>{store.rateLimit.limitReached ? "reached" : `${Math.floor(store.rateLimit.utilization)}%`}</span>
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
  </div>
</header>
</div>

<style>
  .chat-header { container: pane-chrome / inline-size; flex: none; min-width: 0; }
  .strip {
    display: flex;
    align-items: center;
    min-width: 0;
    height: var(--pane-toolbar-height);
    gap: 5px;
    padding: 0 8px;
    white-space: nowrap;
    border-bottom: 1px solid var(--edge);
    background: color-mix(in srgb, var(--bg) 65%, var(--term-bg));
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
    border: 1px solid transparent;
    border-radius: 6px;
    padding: 0 6px;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: var(--pane-control-size);
    /* Fixed-height pill: the label must clip, never wrap out of it. */
    white-space: nowrap;
    min-width: 0;
    overflow: hidden;
  }
  .chip.pick {
    background: none;
    color: var(--fg);
    font: inherit;
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }
  .chip.pick:hover:not(:disabled), .chip.pick[aria-expanded="true"], .more:hover {
    color: var(--fg);
    background: var(--pane-control-hover);
  }
  .chip.pick:disabled { cursor: default; color: var(--muted); }
  .mode-picker .chip, .effort-picker .chip { color: var(--muted); }
  .caret {
    flex: none;
    display: inline-flex;
    opacity: 0.5;
  }
  /* Remote Control keeps its state dot inside its details popover. */
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
  .rc-dot.on {
    background: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 22%, transparent);
  }
  .rc-dot.busy {
    background: var(--accent);
    animation: pulse 1.4s ease-in-out infinite; /* shared keyframe in app.css */
  }
  .rc-dot.err {
    background: var(--warn);
  }
  /* Infinite "presence" animations pause while the app is hidden (the
     html.app-hidden contract; see app.css). */
  :global(html.app-hidden) .rc-dot.busy {
    animation-play-state: paused;
  }
  @media (prefers-reduced-motion: reduce) {
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
    white-space: normal;
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
  .spacer { flex: 1; }
  .session-status { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .rate-label-short { display: none; }
  .primary-picker { flex: 0 1 auto; min-width: 0; }
  .primary-picker .chip { max-width: 100%; }
  .pick-label { overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .more { width: var(--pane-control-size); justify-content: center; padding: 0; background: none; color: var(--muted); cursor: pointer; }
  .extras { flex: none; }
  .options-menu { min-width: 240px; }
  .options-menu .menu-row { display: flex; gap: 16px; justify-content: space-between; }
  .options-menu .menu-row span { color: var(--muted); }
  .options-heading { padding: 8px 12px; font-size: var(--text-xs); color: var(--muted); white-space: normal; }
  .chip:focus-visible, .stop:focus-visible, .menu-row:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: -2px; }
  .stop, .ctx, .ratelimit { flex: none; white-space: nowrap; }
  .stop { height: var(--pane-control-size); }
  @container pane-chrome (max-width: 620px) {
    .agent-name, .ctx { display: none; }
    .ctx.full { display: inline; }
    .agent-id { border: 0; margin: 0; padding: 0; }
    .strip .ratelimit { min-width: 0; flex-shrink: 1; padding: 0 6px; }
    .rate-label { display: none; }
    .rate-label-short { display: inline; min-width: 0; max-width: 52px; overflow: hidden; text-overflow: ellipsis; }
    .stop { width: var(--pane-control-size); padding: 0; }
    .stop-label { display: none; }
  }
  /* Small chat panes have two deliberate rows: session/status, then selectors.
     Their height stays fixed as a turn starts, stops, or reports a limit. */
  @container pane-chrome (max-width: 480px) {
    .strip { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) 76px 24px; grid-template-rows: 24px 24px; gap: 4px 5px; height: calc(var(--pane-toolbar-height) * 2); padding: 4px 8px; }
    .agent-id { grid-row: 1; grid-column: 1; min-width: 0; }
    .agent-name { display: inline; overflow: hidden; text-overflow: ellipsis; }
    .model-picker { grid-row: 2; grid-column: 1; }
    .mode-picker { grid-row: 2; grid-column: 2; }
    .effort-picker { grid-row: 2; grid-column: 3; }
    .extras { grid-row: 2; grid-column: 4; }
    .primary-picker .chip { width: 100%; justify-content: space-between; }
    .session-status { grid-row: 1; grid-column: 2 / 5; justify-self: end; max-width: 100%; }
    .spacer { display: none; }
    .strip.no-mode .model-picker,
    .strip.no-effort .model-picker { grid-column: 1 / 3; }
    .strip.no-effort .mode-picker { grid-column: 3; }
    .strip.no-mode.no-effort .model-picker { grid-column: 1 / 4; }
  }
  .stop {
    font: inherit;
    font-size: var(--text-xs);
    border: 1px solid var(--edge);
    color: var(--fg);
    background: var(--term-bg);
    border-radius: 6px;
    padding: 0 9px;
    line-height: 1.35;
    cursor: pointer;
    transition: background-color 0.12s ease;
  }
  .stop { display: inline-flex; align-items: center; justify-content: center; gap: 5px; }
  .stop-mark { width: 6px; height: 6px; border-radius: 1px; background: currentColor; }
  .stop:hover {
    background: var(--row-hover);
  }
  .ctx {
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }
  .ctx.full {
    color: var(--warn);
  }
  .ratelimit {
    gap: 4px;
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

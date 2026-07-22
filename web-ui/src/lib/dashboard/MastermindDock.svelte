<script lang="ts">
  /**
   * The Mastermind dock — the one management surface of the workspace's
   * privileged agent (dashboard plan §7). Three honest states: no binding →
   * the setup card (agent + ask/auto mode + start); a bound, live session →
   * identity header + the embedded chat (a plain ChatView on the chat pool);
   * a binding whose session is gone → say so and offer the reset. Reactive-
   * only by design: nothing here ever triggers a Mastermind turn — it speaks
   * when the user types into its composer, never before.
   */
  import BrandMark from "../shared/BrandMark.svelte";
  import ChatView from "../chat/ChatView.svelte";
  import { acquireChat, releaseChat } from "../chat/chatPool";
  import type { ChatStore } from "../chat/store.svelte";
  import { dismiss } from "../shared/dismiss";
  import { resolvedTheme } from "../settings/store.svelte";
  import { ApiError } from "../net/api";
  import { deleteMastermind, putMastermind, type Session } from "../workspace/sessions";
  import type { LayoutCtrl } from "../layout/dnd";

  interface Props {
    /** The binding from the workspaces wire; null = unconfigured. */
    cfg: { session_id: string; mode: "ask" | "auto"; agent?: string } | null;
    /** The live roster row for cfg.session_id (null while the map lacks it). */
    session: Session | null;
    wsId: string;
    paneId: string;
    ctrl: LayoutCtrl;
    /** Re-sync the workspaces list after a PUT/DELETE (the binding lives there). */
    refresh: () => Promise<void>;
    /** Collapse the dock back to the edge pill / close the overlay. */
    onCollapse: () => void;
    /** The dock currently fills the whole dashboard surface. */
    expanded: boolean;
    /** Toggle between the sidebar width and the full surface. */
    onToggleExpand: () => void;
    /** False while the retained dashboard pane is hidden. */
    visible?: boolean;
  }

  let {
    cfg,
    session,
    wsId,
    paneId,
    ctrl,
    refresh,
    onCollapse,
    expanded,
    onToggleExpand,
    visible = true,
  }: Props = $props();

  /** Setup-card mode choice; ask-first is the default (plan §6). */
  let mode = $state<"ask" | "auto">("ask");
  /** Setup-card agent choice — both chat-driver agents enforce the mode
   *  through their own harness (claude: settings pre-allows; codex: the
   *  driver answering its MCP tool-call elicitations from the recorded
   *  mode). */
  let agent = $state<"claude" | "codex">("claude");
  /** A PUT/DELETE is in flight — buttons disable, the chat gap shows busy. */
  let pending = $state(false);
  /** The server's own words for a refused/failed call, shown verbatim. */
  let error = $state<string | null>(null);
  /** The PUT's returned session row: bridges the beat between the binding
   *  refresh landing and the events snapshot that carries the new row (the
   *  "gone" card must not flash while the roster catches up). */
  let justCreated = $state<Session | null>(null);
  let menuOpen = $state(false);
  let confirm = $state<null | { kind: "mode"; to: "ask" | "auto" } | { kind: "retire" }>(null);

  /** The session behind the binding, only while genuinely alive: a present-
   *  but-dead row is honestly "gone" (errored Masterminds keep no ghost chat). */
  const live = $derived.by(() => {
    if (cfg === null) return null;
    if (session !== null) return session.alive ? session : null;
    return justCreated !== null && justCreated.id === cfg.session_id ? justCreated : null;
  });

  const modeLabel = (m: "ask" | "auto") => (m === "ask" ? "ask first" : "auto");
  const MODE_HELP: Record<"ask" | "auto", string> = {
    ask: "acting on the workspace asks you first — reads never ask",
    auto: "acts without asking; every act is audited",
  };
  /** The bound vendor, from the binding itself (additive wire field) with
   *  the roster row as the pre-upgrade fallback — null when neither knows,
   *  which disables the mode switch rather than guessing. */
  const boundAgent = $derived(
    cfg === null ? null : (cfg.agent ?? session?.agent_kind ?? justCreated?.agent_kind ?? null),
  );

  // A second refcounted hold on the SAME pool entry the embedded ChatView
  // uses — read-only, for the native-mode cross-check below.
  //
  // Key the effect on the id STRING, not the `live` object: every /ws/events
  // snapshot hands us a fresh session-object identity, so depending on `live`
  // would tear down + re-acquire the same pool entry once per second. The
  // intermediate mmId derived collapses those snapshots to a stable string,
  // so the acquire/release fires only when the bound session id truly changes.
  const mmId = $derived(
    cfg !== null && live !== null && live.ui === "chat" ? live.id : null,
  );
  let mmStore = $state<ChatStore | null>(null);
  $effect(() => {
    const id = mmId;
    if (id === null) {
      mmStore = null;
      return;
    }
    mmStore = acquireChat(id).store;
    return () => {
      releaseChat(id);
      mmStore = null;
    };
  });

  /** Claude's native permission modes that DON'T raise a prompt for a
   *  non-allowlisted MCP act — the set that makes our ask-first gate moot.
   *  Framed as the opposite of the asking modes (default / acceptEdits /
   *  plan still prompt for a non-edit MCP act) so a new non-asking mode is
   *  caught by adding it here, and the two edit/plan modes never false-fire.
   *  claude's vocabulary: default, acceptEdits, plan, auto, dontAsk,
   *  bypassPermissions (claude_modes()). */
  const CLAUDE_NONASKING_MODES = ["auto", "dontAsk", "bypassPermissions"];

  /** The honest cross-check between the TWO mode machines on this surface:
   *  our binding gates acts by not pre-allowing them — which only bites
   *  while claude's own permission mode actually asks. If the user flips
   *  claude's native mode to one that doesn't ask (auto / "Don't ask" /
   *  bypass — its own picker in the chat header, or shift+tab), ask-first is
   *  silently moot — say so instead of wearing a badge that no longer means
   *  what it says. Claude-only: codex's gate is the driver answering
   *  elicitations, which no native mode bypasses. */
  const nativeModeCaveat = $derived.by(() => {
    if (cfg === null || cfg.mode !== "ask" || boundAgent !== "claude") return null;
    const m = mmStore?.currentMode ?? null;
    if (m === null || !CLAUDE_NONASKING_MODES.includes(m)) return null;
    return mmStore?.modes.find((x) => x.id === m)?.label ?? m;
  });

  /** PUT the binding (setup start AND mode switch — a mode change is a
   *  re-PUT; the daemon restarts the session with the new gating). A mode
   *  switch keeps the bound agent (never silently rotates a codex
   *  Mastermind into a claude one — when the vendor is unknowable the
   *  switch refuses instead of defaulting); the setup card uses the picker. */
  async function appoint(m: "ask" | "auto"): Promise<void> {
    if (pending) return;
    if (cfg !== null && boundAgent === null) {
      error = "can't switch mode: the bound agent is unknown — retire and re-appoint instead";
      confirm = null;
      return;
    }
    pending = true;
    error = null;
    confirm = null;
    const a = cfg !== null ? (boundAgent ?? "claude") : agent;
    try {
      // Theme rides along like POST /sessions so the agent boots matched.
      justCreated = await putMastermind(wsId, { agent: a, mode: m, theme: resolvedTheme() });
      await refresh();
    } catch (e) {
      error = e instanceof ApiError ? e.message : String(e);
    } finally {
      pending = false;
    }
  }

  /** DELETE the binding. A 404 means it is already gone — that IS the goal. */
  async function retire(): Promise<void> {
    if (pending) return;
    pending = true;
    error = null;
    confirm = null;
    try {
      await deleteMastermind(wsId);
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 404)) {
        error = e instanceof ApiError ? e.message : String(e);
        pending = false;
        return;
      }
    }
    justCreated = null;
    await refresh();
    pending = false;
  }

  // focused = the last pointerdown landed inside the dock — the pane idiom
  // (click-to-focus), never hover: ChatView's composer grabs keyboard focus
  // when `focused` flips true, and a hover must not steal it from a terminal.
  let rootEl = $state<HTMLElement | null>(null);
  let focusWithin = $state(false);
  $effect(() => {
    const onDown = (e: PointerEvent) => {
      focusWithin = rootEl !== null && e.target instanceof Node && rootEl.contains(e.target);
    };
    window.addEventListener("pointerdown", onDown, true);
    return () => window.removeEventListener("pointerdown", onDown, true);
  });
</script>

<div class="dock" bind:this={rootEl}>
  <header class="head">
    {#if cfg !== null}
      <BrandMark size={13} title="Mastermind" />
      <span class="title">Mastermind</span>
      <span class="chip">{boundAgent ?? "…"}</span>
      <!-- The badge IS the control: this is the Mastermind's act gate (ours,
           not the agent's own permission mode) — click to switch it. -->
      <button
        class="chip modechip"
        title="how workspace acts are gated: {MODE_HELP[cfg.mode]} — click to switch"
        onclick={() => {
          confirm = { kind: "mode", to: cfg.mode === "ask" ? "auto" : "ask" };
        }}
      >
        acts: {modeLabel(cfg.mode)}
      </button>
      <span class="sp"></span>
      <!-- Default node.contains inside-test: the button + its menu stay open.
           (Never the .menu-host class — that selector belongs to ChatView's
           own dismiss and would pin a pane chat's open menu on our clicks.) -->
      <div
        class="menu-anchor"
        use:dismiss={{ enabled: menuOpen, onDismiss: () => (menuOpen = false) }}
      >
        <button
          class="hbtn"
          title="mastermind actions"
          aria-label="mastermind actions"
          onclick={() => (menuOpen = !menuOpen)}>⋯</button
        >
        {#if menuOpen}
          <div class="menu overlay-surface" role="menu">
            <button
              class="overlay-row"
              role="menuitem"
              onclick={() => {
                menuOpen = false;
                confirm = { kind: "mode", to: cfg.mode === "ask" ? "auto" : "ask" };
              }}
            >
              switch to {modeLabel(cfg.mode === "ask" ? "auto" : "ask")}
            </button>
            <button
              class="overlay-row danger"
              role="menuitem"
              onclick={() => {
                menuOpen = false;
                confirm = { kind: "retire" };
              }}
            >
              retire the Mastermind
            </button>
          </div>
        {/if}
      </div>
    {:else}
      <span class="sp"></span>
    {/if}
    <button
      class="hbtn"
      title={expanded ? "restore the dock width" : "expand the dock to the whole surface"}
      aria-label={expanded ? "restore the dock width" : "expand the dock"}
      onclick={onToggleExpand}>{expanded ? "⤡" : "⤢"}</button
    >
    <button class="hbtn" title="collapse the dock" aria-label="collapse the dock" onclick={onCollapse}
      >»</button
    >
  </header>

  {#if nativeModeCaveat !== null}
    <!-- Two mode machines, one honest line: claude's own permission mode
         currently outranks our ask-first gate. -->
    <div class="warnline">
      claude's own permission mode is “{nativeModeCaveat}” — while it's on, claude may act
      without asking, so <b>ask first</b> only bites once it's back to a mode that asks.
    </div>
  {/if}

  {#if confirm !== null}
    {@const c = confirm}
    <div class="confirm" class:danger={c.kind === "retire"}>
      <span class="ctext">
        {c.kind === "retire"
          ? "retire the Mastermind? its session ends."
          : `switch to ${modeLabel(c.to)}? this restarts the session.`}
      </span>
      <button
        class="mini"
        disabled={pending}
        onclick={() => (c.kind === "retire" ? retire() : appoint(c.to))}
      >
        {c.kind === "retire" ? "retire" : "switch"}
      </button>
      <button class="mini quiet" onclick={() => (confirm = null)}>cancel</button>
    </div>
  {/if}

  {#if cfg !== null && error !== null}
    <div class="err">{error}</div>
  {/if}

  {#if cfg === null}
    <!-- The setup card: what a Mastermind IS, in plain words, then the two
         choices that matter. It exists only after the user starts it. -->
    <div class="setup">
      <BrandMark size={26} draw title="chimaera" />
      <h3>Mastermind</h3>
      <p class="help">
        One agent that knows every inch of this workspace: it sees every session, answers your
        questions, and delegates work to other agents. It never does the work itself — and it bills
        as your own account.
      </p>

      <div class="field">agent</div>
      <label class="choice">
        <input
          type="radio"
          name="mm-agent"
          value="claude"
          bind:group={agent}
          disabled={pending}
        />
        <span class="cbody">
          <span class="cname">claude</span>
          <span class="csub">Claude Code, as your own account</span>
        </span>
      </label>
      <label class="choice">
        <input type="radio" name="mm-agent" value="codex" bind:group={agent} disabled={pending} />
        <span class="cbody">
          <span class="cname">codex</span>
          <span class="csub">Codex, as your own account</span>
        </span>
      </label>

      <div class="field">mode</div>
      <label class="choice">
        <input type="radio" name="mm-mode" value="ask" bind:group={mode} disabled={pending} />
        <span class="cbody">
          <span class="cname">ask first</span>
          <span class="csub">{MODE_HELP.ask}</span>
        </span>
      </label>
      <label class="choice">
        <input type="radio" name="mm-mode" value="auto" bind:group={mode} disabled={pending} />
        <span class="cbody">
          <span class="cname">auto</span>
          <span class="csub">{MODE_HELP.auto}</span>
        </span>
      </label>

      <button class="cta" disabled={pending} onclick={() => appoint(mode)}>
        {pending ? "starting…" : "start the Mastermind"}
      </button>
      {#if error !== null}
        <div class="err">{error}</div>
      {/if}
    </div>
  {:else if live !== null && live.ui !== "chat"}
    <!-- The daemon degrades a chat whose handshake fails into a PTY under
         the same id — and a flagged row is hidden from the rail/roster, so
         the dock must be its door or the session is unreachable. -->
    <div class="gone">
      <BrandMark size={20} title="chimaera" />
      <p>
        the Mastermind degraded to a terminal (its chat handshake failed) —
        open it to see why, or reset and start over.
      </p>
      <button
        class="cta quiet"
        onclick={() => {
          if (live !== null) ctrl.revealWorktreeSession(live.id, wsId);
        }}>open the terminal</button
      >
      <button class="cta quiet" disabled={pending} onclick={retire}>reset</button>
    </div>
  {:else if live !== null}
    <!-- The embedded chat: the same ChatView the panes use, on the same chat
         pool, scoped by the wrapper so it behaves at dock width. -->
    <div class="dock-chat">
      {#key live.id}
        <ChatView
          session={live}
          focused={focusWithin}
          {visible}
          onOpenFile={(p) => ctrl.openFileFrom(paneId, p, false)}
          onOpenPath={(p, k) => ctrl.openPathFrom(paneId, p, k, false)}
        />
      {/key}
    </div>
  {:else if pending}
    <div class="busy">
      <BrandMark size={20} busy title="chimaera" />
      <span>restarting…</span>
    </div>
  {:else}
    <!-- Bound but the session is gone: say so, offer the way back. -->
    <div class="gone">
      <BrandMark size={20} title="chimaera" />
      <p>the Mastermind session is gone — set it up again.</p>
      <button class="cta quiet" disabled={pending} onclick={retire}>reset</button>
    </div>
  {/if}
</div>

<style>
  .dock {
    position: relative;
    height: 100%;
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
    background: var(--bg);
  }

  .head {
    flex: none;
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    padding: 7px 8px 7px 12px;
    border-bottom: 1px solid var(--edge);
  }
  .title {
    font-size: var(--text-sm);
    font-weight: 600;
    letter-spacing: 0.01em;
    white-space: nowrap;
  }
  .chip {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 999px;
    padding: 0 6px;
    white-space: nowrap;
  }
  /* The act-gate badge doubles as its own switch. */
  button.modechip {
    appearance: none;
    background: none;
    font-family: var(--mono);
    line-height: inherit;
    cursor: pointer;
    transition: border-color 0.12s ease;
  }
  button.modechip:hover {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }

  /* The native-mode caveat: a quiet warn line, the stall-pill tone. */
  .warnline {
    flex: none;
    font-size: var(--text-xs);
    line-height: 1.45;
    color: var(--warn);
    padding: 6px 12px;
    border-bottom: 1px solid color-mix(in srgb, var(--warn) 30%, var(--edge));
  }
  .warnline b {
    font-weight: 600;
  }
  .sp {
    flex: 1;
    min-width: 0;
  }
  .hbtn {
    flex: none;
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-md);
    line-height: 1;
    color: var(--muted);
    cursor: pointer;
    padding: 2px 5px;
    border-radius: 4px;
  }
  .hbtn:hover {
    color: var(--fg);
    background: var(--row-hover);
  }

  .menu-anchor {
    position: relative;
    flex: none;
  }
  .menu {
    top: 100%;
    right: 0;
    margin-top: 4px;
    min-width: 180px;
    z-index: 8;
    display: flex;
    flex-direction: column;
  }
  .overlay-row.danger:hover {
    color: var(--err);
  }

  /* Inline confirm strip — the rail's kill-confirm idiom, not a dialog. */
  .confirm {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    font-size: var(--text-sm);
    color: var(--muted);
    border-bottom: 1px solid var(--edge);
  }
  .confirm.danger {
    border-bottom-color: color-mix(in srgb, var(--err) 35%, var(--edge));
  }
  .ctext {
    flex: 1;
    min-width: 0;
  }
  .mini {
    flex: none;
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-xs);
    padding: 1px 8px;
    border-radius: 999px;
    cursor: pointer;
  }
  .confirm.danger .mini:not(.quiet) {
    color: var(--err);
    border-color: color-mix(in srgb, var(--err) 45%, var(--edge));
  }
  .mini.quiet {
    border-color: transparent;
    color: var(--muted);
  }
  .mini.quiet:hover {
    color: var(--fg);
  }
  .mini:disabled {
    opacity: 0.5;
    cursor: default;
  }

  /* Quiet danger: the server's words, verbatim, no shouting. */
  .err {
    flex: none;
    font-size: var(--text-sm);
    color: var(--err);
    padding: 6px 12px;
    border-bottom: 1px solid color-mix(in srgb, var(--err) 25%, var(--edge));
  }
  .setup .err {
    border: none;
    padding: 0;
    text-align: center;
  }

  /* --- the setup card -------------------------------------------------------- */
  .setup {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 26px 18px 22px;
  }
  .setup h3 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
    letter-spacing: 0.01em;
  }
  .help {
    margin: 0 0 6px;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
    text-align: center;
    max-width: 300px;
  }
  .field {
    align-self: stretch;
    font-size: var(--text-xs);
    color: var(--muted);
    letter-spacing: 0.04em;
    text-transform: lowercase;
    padding: 6px 2px 0;
  }
  .choice {
    align-self: stretch;
    display: flex;
    align-items: flex-start;
    gap: 9px;
    padding: 7px 10px;
    border: 1px solid var(--edge);
    border-radius: 7px;
    cursor: pointer;
    transition: border-color 0.12s ease;
  }
  .choice:hover {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }
  .choice:has(input:checked) {
    border-color: color-mix(in srgb, var(--accent) 60%, var(--edge));
  }
  .choice input {
    flex: none;
    margin: 2px 0 0;
    accent-color: var(--accent);
  }
  .cbody {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .cname {
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
  }
  .csub {
    font-size: var(--text-xs);
    color: var(--muted);
    line-height: 1.4;
  }

  .cta {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-md);
    padding: 6px 16px;
    border-radius: 6px;
    cursor: pointer;
    margin-top: 8px;
    transition: border-color 0.12s ease;
  }
  .cta:hover:not(:disabled) {
    border-color: var(--accent);
  }
  .cta:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .cta.quiet {
    color: var(--muted);
  }
  .cta.quiet:hover:not(:disabled) {
    color: var(--fg);
  }

  /* --- the embedded chat ------------------------------------------------------ */
  /* ChatView owns its own height:100% column; the wrapper just hands it the
     remaining dock height and forbids horizontal creep at ~360px. Its reading
     measure (52rem) never binds this narrow, so no ChatView change is needed. */
  .dock-chat {
    flex: 1;
    min-height: 0;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .dock-chat > :global(.chat) {
    flex: 1;
    min-height: 0;
  }

  /* --- busy / gone -------------------------------------------------------------- */
  .busy,
  .gone {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    padding: 20px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .gone p {
    margin: 0;
    text-align: center;
    line-height: 1.5;
    max-width: 260px;
  }
</style>

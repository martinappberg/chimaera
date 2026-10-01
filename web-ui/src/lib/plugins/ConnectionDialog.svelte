<script lang="ts">
  import { onMount } from "svelte";
  import { modalFocus } from "../shared/modalFocus";
  import { focusOnMount } from "../shared/focusOnMount";
  import { pageVisible } from "../shared/visibility";
  import { openInSystemBrowser } from "../shared/urlOpen";
  import { CLAUDE_CONNECTIONS_URL, connectionAuthAction, connectionAuthStatus, loginConnection, type ConnectionAuth } from "./connections";
  import type { AgentId } from "./store";

  let { wsId, agent, name, visible, onClose, onConnected }: {
    wsId: string; agent: AgentId; name: string; visible: boolean;
    onClose: () => void; onConnected: () => void;
  } = $props();
  let attempt = $state<ConnectionAuth | null>(null);
  let error = $state<string | null>(null);
  let callback = $state("");
  let busy = $state(false);
  let alive = true;
  let announced = false;
  const dialogId = $props.id();
  const hosted = $derived(agent === "claude" && name.startsWith("claude.ai "));
  const attemptId = $derived(attempt?.id);
  const terminal = $derived(attempt !== null && ["succeeded", "failed", "cancelled"].includes(attempt.state));
  const provider = $derived(agent === "claude" ? "Claude Code" : "Codex");
  const destination = $derived.by(() => {
    try { return attempt?.authorization_url ? new URL(attempt.authorization_url).hostname : ""; }
    catch { return ""; }
  });

  async function start(): Promise<void> {
    error = null; attempt = null; callback = ""; busy = true;
    try {
      const result = await loginConnection(wsId, agent, name);
      if (alive) attempt = result;
      else void connectionAuthAction(wsId, result.id, "cancel").catch(() => {});
    } catch (e) { if (alive) error = e instanceof Error ? e.message : String(e); }
    finally { if (alive) busy = false; }
  }

  async function refresh(id: string): Promise<void> {
    try {
      const result = await connectionAuthStatus(wsId, id);
      if (!alive || attempt?.id !== id) return;
      attempt = result; error = null;
      if (result.state === "succeeded" && !announced) { announced = true; onConnected(); }
    } catch (e) { if (alive) error = e instanceof Error ? e.message : String(e); }
  }

  onMount(() => {
    void start();
    return () => {
      alive = false;
      if (attempt && !terminal) void connectionAuthAction(wsId, attempt.id, "cancel").catch(() => {});
    };
  });

  // Poll only this small in-memory job snapshot. A provider check is explicit;
  // hidden panes never repeatedly start MCP inventory processes.
  $effect(() => {
    const id = attemptId;
    if (!id || terminal || !visible || !$pageVisible) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll(): Promise<void> {
      await refresh(id!);
      if (!stopped) timer = setTimeout(() => void poll(), 1000);
    }
    timer = setTimeout(() => void poll(), 100);
    return () => { stopped = true; clearTimeout(timer); };
  });

  async function act(action: "check" | "callback"): Promise<void> {
    if (!attempt || busy) return;
    busy = true; error = null;
    try {
      await connectionAuthAction(wsId, attempt.id, action, action === "callback" ? callback.trim() : undefined);
      callback = "";
      await refresh(attempt.id);
    } catch (e) { if (alive) error = e instanceof Error ? e.message : String(e); }
    finally { if (alive) busy = false; }
  }

  async function close(): Promise<void> {
    if (busy && attempt !== null) return;
    if (attempt && !terminal) {
      busy = true;
      try { await connectionAuthAction(wsId, attempt.id, "cancel"); }
      catch (e) { error = e instanceof Error ? e.message : String(e); busy = false; return; }
    }
    onClose();
  }
</script>

<div class="backdrop" role="presentation" onkeydown={(event) => {
  if (event.key === "Escape") { event.stopPropagation(); void close(); }
  if (event.key === "Enter" && event.repeat) event.preventDefault();
}}>
  <div class="dialog" role="dialog" aria-modal="true" aria-label={`Connect ${name}`} tabindex="-1" use:modalFocus>
    <header>
      <div class="eyebrow">CONNECTION · {provider}</div>
      <h2>{attempt?.state === "succeeded" ? "Connected" : "Connect"} {name.replace(/^claude\.ai /, "")}</h2>
      <p>{hosted ? "Claude manages this connector's setup and authorization in your account." : "Your agent handles authorization and keeps the credentials on this host."}</p>
    </header>
    <div class="progress" role="status" aria-live="polite" class:success={attempt?.state === "succeeded"}>
      <span class="dot"></span>
      {#if attempt?.state === "succeeded"}The agent confirmed this connection.
      {:else if attempt?.state === "failed"}{hosted ? "Connection setup didn’t finish." : "Sign-in didn’t finish."}
      {:else if attempt?.state === "cancelled"}{hosted ? "Connection setup was cancelled." : "Sign-in was cancelled."}
      {:else if error && !attempt}Couldn't prepare connection.
      {:else if attempt?.state === "verifying"}Checking your connection…
      {:else if attempt?.authorization_url}{hosted ? "Review setup in Claude." : "Continue in your browser."}
      {:else}{hosted ? "Checking this connector…" : "Preparing a secure sign-in link…"}{/if}
    </div>
    {#if attempt?.authorization_url && !terminal}
      <div class="browser-step">
        <button class="opt primary" onclick={() => openInSystemBrowser(hosted ? CLAUDE_CONNECTIONS_URL : attempt!.authorization_url!)}>{hosted ? "Open Claude connector settings ↗" : "Open authorization page ↗"}</button>
        <span class="destination">{destination}</span>
      </div>
      {#if attempt.state === "awaiting_callback" || attempt.state === "verifying" && !hosted}
        <form onsubmit={(event) => { event.preventDefault(); void act("callback"); }}>
          <label for={`${dialogId}-callback`}>Return from your browser</label>
          <p>After authorizing, copy the full callback address from your browser and paste it here. A localhost page may not load; its address still works.</p>
          <input id={`${dialogId}-callback`} type="text" inputmode="url" autocomplete="off" spellcheck="false" placeholder="Paste the callback URL" bind:value={callback} maxlength="8192" disabled={busy || attempt.state !== "awaiting_callback"} />
          <button class="opt primary" type="submit" disabled={busy || attempt.state !== "awaiting_callback" || !callback.trim()}>Finish sign-in</button>
        </form>
      {:else if attempt.state === "awaiting_browser" || attempt.state === "verifying" && hosted}
        <p>Select this connector in Claude, complete its setup or sign-in, then return here. Claude shows connection errors and configuration options in those settings.</p>
        <button class="opt primary" disabled={busy || attempt.state !== "awaiting_browser"} onclick={() => void act("check")}>Check connection</button>
        <p>Chimaera can check whether the connector is connected, but can't see errors shown in your browser.</p>
      {/if}
    {/if}
    {#if attempt?.message}<p class:failure={attempt.state === "failed"}>{attempt.message}</p>{/if}
    {#if error}<p class="failure" role="alert">{error}</p>{/if}
    {#if attempt?.state === "succeeded"}<p>Existing agent sessions may need to reconnect or reopen to use it.</p>{/if}
    <footer>
      {#if hosted && terminal && attempt?.state !== "succeeded"}
        <button class="opt quiet" onclick={() => openInSystemBrowser(CLAUDE_CONNECTIONS_URL)}>Connector settings ↗</button>
      {/if}
      {#if attempt?.state !== "succeeded"}
        <button class="opt quiet" use:focusOnMount disabled={busy && attempt !== null} onclick={() => void close()}>{terminal || error && !attempt ? "Close" : "Cancel"}</button>
      {/if}
      {#if attempt?.state === "failed" || attempt?.state === "cancelled" || error && !attempt}
        <button class="opt primary" disabled={busy} onclick={() => void start()}>Try again</button>
      {:else if attempt?.state === "succeeded"}
        <button class="opt primary" use:focusOnMount onclick={onClose}>Done</button>
      {/if}
    </footer>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; z-index: 110; display: grid; place-items: center; padding: 24px; background: var(--scrim); backdrop-filter: blur(2px); }
  .dialog { display: flex; flex-direction: column; gap: 18px; width: min(480px, 100%); max-height: calc(100vh - 48px); overflow: auto; padding: 24px; border: 1px solid var(--edge); border-radius: 14px; background: var(--bg); box-shadow: 0 16px 48px rgba(0,0,0,.35); }
  header { display: flex; flex-direction: column; gap: 10px; }
  .eyebrow { font-size: var(--text-xs); letter-spacing: .08em; color: var(--muted); }
  h2 { margin: 0; font-size: var(--text-lg); font-weight: 600; overflow-wrap: anywhere; }
  p { margin: 0; font-size: var(--text-sm); line-height: 1.55; color: var(--muted); overflow-wrap: anywhere; }
  .progress { display: flex; align-items: center; gap: 10px; font-size: var(--text-sm); padding: 12px; background: color-mix(in srgb, var(--fg) 5%, var(--bg)); border-radius: 8px; }
  .dot { width: 7px; height: 7px; border-radius: 50%; flex: none; background: var(--muted); }
  .success .dot { background: var(--accent); }
  .browser-step { display: flex; flex-direction: column; align-items: flex-start; gap: 8px; }
  .destination { font-size: var(--text-xs); color: var(--muted); overflow-wrap: anywhere; }
  .opt { padding: 7px 12px; }
  form { display: flex; flex-direction: column; align-items: stretch; gap: 10px; }
  label { font-size: var(--text-sm); font-weight: 600; }
  input { width: 100%; box-sizing: border-box; font: inherit; font-size: var(--text-sm); color: var(--fg); background: var(--bg); border: 1px solid var(--edge); border-radius: 6px; padding: 9px 10px; }
  input:focus { outline: 2px solid var(--focus-ring); outline-offset: 1px; }
  .failure { color: var(--err); }
  footer { display: flex; justify-content: flex-end; gap: 8px; padding-top: 6px; }
  @media (max-width: 520px) { .backdrop { padding: 12px; } .dialog { padding: 18px; } }
</style>

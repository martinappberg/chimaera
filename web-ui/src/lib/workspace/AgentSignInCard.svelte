<script lang="ts">
  // The one sign-in flow, wherever an agent turns out to be signed out: a
  // Settings → Agents card, a chat that could not start. The daemon runs the
  // CLI's own login; this card shows the page it printed (and a device code),
  // takes a pasted code back, and reports what the CLI's status command said
  // — never a success of its own.
  import { onMount } from "svelte";
  import { pageVisible } from "../shared/visibility";
  import { openInSystemBrowser } from "../shared/urlOpen";
  import { copyText } from "../shared/clipboard";
  import { resolvedTheme } from "../settings/store.svelte";
  import { ApiError } from "../net/api";
  import type { AgentInfo } from "./launcher";
  import { cancelSignIn, getSignIn, sendSignInCode, signInRunning, startSignIn, type SignInProgress, type SignInStatus } from "./agentSignIn";

  interface Props {
    agent: AgentInfo;
    /** A sign-in runs in a workspace (the CLI's cwd, the terminal flow's
     *  home); without one the card only reports. */
    workspaceId: string | null;
    /** Start the login at once (the user already clicked Sign in elsewhere). */
    autostart?: boolean;
    /** Framed as a card of its own (a chat's empty state) or inline under a
     *  row that already names the agent (Settings). */
    framed?: boolean;
    /** The CLI confirmed the login. */
    onSignedIn?: () => void;
    /** The user is done here without signing in. */
    onDismiss?: () => void;
  }
  let { agent, workspaceId, autostart = false, framed = false, onSignedIn, onDismiss }: Props = $props();

  let status = $state<SignInStatus | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let code = $state("");
  let copied = $state<"url" | "code" | null>(null);
  let requestId: string | null = null;
  let checking = false;
  let disposed = false;
  let announced: string | null = null;
  /** The attempt this card started: a finished attempt from elsewhere (an
   *  earlier card, another window) is history, not this card's state. */
  let ownAttempt = $state<string | null>(null);

  const operation = $derived<SignInProgress | null>(
    status?.operation && (signInRunning(status.operation) || status.operation.id === ownAttempt) ? status.operation : null,
  );
  const running = $derived(signInRunning(operation));
  const how = $derived(status?.how ?? agent.signIn?.how ?? "browser");
  const pageHost = $derived.by(() => {
    try { return operation?.url ? new URL(operation.url).hostname : null; } catch { return null; }
  });

  async function check(): Promise<void> {
    if (checking || disposed) return;
    checking = true;
    try {
      const next = await getSignIn(agent.id);
      if (disposed) return;
      status = next;
      if (next.operation?.id === requestId) { ownAttempt = requestId; requestId = null; }
      if (next.operation?.phase === "succeeded" && announced !== next.operation.id) {
        announced = next.operation.id;
        onSignedIn?.();
      }
    } catch (e) {
      if (!disposed) error = e instanceof Error ? e.message : "Couldn't check the sign-in.";
    } finally { checking = false; }
  }
  onMount(() => {
    void check().then(() => { if (autostart && !running) void start(); });
    return () => { disposed = true; };
  });
  // Only a sign-in in progress needs the daemon again, and only while someone
  // can see it: a hidden window stops asking.
  $effect(() => {
    if (!$pageVisible || !running) return;
    const timer = setInterval(() => void check(), 1500);
    return () => clearInterval(timer);
  });

  async function start(): Promise<void> {
    if (busy || running || workspaceId === null) return;
    busy = true; error = null; code = "";
    requestId ??= crypto.randomUUID();
    try {
      const next = await startSignIn(agent.id, workspaceId, requestId, resolvedTheme());
      if (disposed) return;
      ownAttempt = next.id;
      status = status ? { ...status, operation: next } : null;
      if (!status) await check();
      requestId = null;
    } catch (e) {
      if (!disposed) {
        error = e instanceof Error ? e.message : "Couldn't start the sign-in.";
        // A refused request is settled; a lost one keeps its id so a retry joins it.
        if (e instanceof ApiError) requestId = null;
      }
    } finally { busy = false; }
  }
  async function cancel(): Promise<void> {
    if (!operation || busy) return;
    busy = true;
    try { status = status ? { ...status, operation: await cancelSignIn(agent.id, operation.id) } : status; }
    catch (e) { error = e instanceof Error ? e.message : "Couldn't stop the sign-in."; }
    finally { busy = false; }
  }
  async function submitCode(): Promise<void> {
    const trimmed = code.trim();
    if (!operation || busy || trimmed === "") return;
    busy = true; error = null;
    try {
      status = status ? { ...status, operation: await sendSignInCode(agent.id, operation.id, trimmed) } : status;
      code = "";
    } catch (e) { error = e instanceof Error ? e.message : "Couldn't send the code."; }
    finally { busy = false; }
  }
  async function copy(what: "url" | "code"): Promise<void> {
    const text = what === "url" ? operation?.url : operation?.code;
    if (!text) return;
    if (await copyText(text)) {
      copied = what;
      setTimeout(() => { if (copied === what) copied = null; }, 1200);
    }
  }
</script>

<div class="signin" class:framed role="group" aria-label="Sign in to {agent.name}">
  {#if operation?.phase === "succeeded"}
    <p class="line ok" role="status">Signed in to {agent.name}.</p>
  {:else if running}
    <p class="line" role="status">
      {#if operation?.url}
        {how === "terminal" ? operation.message : `Finish signing in at ${pageHost ?? "the page"}.`}
      {:else}
        {operation?.message ?? "Starting the sign-in…"}
      {/if}
    </p>
    {#if operation?.url}
      <div class="actions">
        <button class="btn primary" onclick={() => openInSystemBrowser(operation!.url!)}>Open ↗</button>
        <button class="link" onclick={() => void copy("url")}>{copied === "url" ? "Copied" : "Copy link"}</button>
      </div>
      {#if operation.code}
        <!-- A device-code flow: the page asks for this. Large and copyable;
             the user types it on another device as often as here. -->
        <button class="code" title="Copy the code" onclick={() => void copy("code")}>
          <span class="digits">{operation.code}</span>
          <span class="hint">{copied === "code" ? "copied" : "enter this code on the page"}</span>
        </button>
      {/if}
      {#if operation.accepts_code}
        <!-- The page shows a code to bring back (Claude). -->
        <form class="paste" onsubmit={(e) => { e.preventDefault(); void submitCode(); }}>
          <input bind:value={code} placeholder="Paste the code the page shows" spellcheck="false" autocomplete="off" disabled={busy} aria-label="Code from the sign-in page" />
          <button class="btn" type="submit" disabled={busy || code.trim() === ""}>Continue</button>
        </form>
      {/if}
    {/if}
    <div class="actions">
      <button class="link quiet" disabled={busy || operation?.phase === "cancelling"} onclick={() => void cancel()}>{operation?.phase === "cancelling" ? "Stopping…" : "Cancel"}</button>
    </div>
  {:else}
    <p class="line" class:failure={operation?.phase === "failed"} role="status">
      {#if operation?.phase === "failed" || operation?.phase === "cancelled"}
        {operation.message}
      {:else if how === "terminal"}
        {agent.name} signs in from its own terminal. Chimaera opens one and notices when the login lands.
      {:else}
        {agent.name} isn't signed in on this host.
      {/if}
    </p>
    <div class="actions">
      <button class="btn primary" disabled={busy || workspaceId === null || !status} onclick={() => void start()}>
        {busy ? "Starting…" : operation ? "Try again" : how === "terminal" ? "Sign in in a terminal" : "Sign in"}
      </button>
      {#if onDismiss}<button class="link quiet" onclick={onDismiss}>Not now</button>{/if}
    </div>
  {/if}
  {#if error}<p class="line failure" role="alert">{error}</p>{/if}
</div>

<style>
  .signin { display: flex; flex-direction: column; gap: 10px; font-size: var(--text-sm); color: var(--fg); min-width: 0; }
  .signin.framed { width: min(420px, 100%); margin: 0 auto; padding: 18px 20px; border: 1px solid var(--edge); border-radius: 12px; background: color-mix(in srgb, var(--fg) 2%, transparent); text-align: left; }
  .line { margin: 0; line-height: 1.5; color: var(--muted); overflow-wrap: anywhere; }
  .line.ok { color: var(--fg); }
  .line.failure { color: var(--err); }
  .actions { display: flex; align-items: center; gap: 14px; flex-wrap: wrap; }
  button { font: inherit; font-size: var(--text-xs); cursor: pointer; }
  button:disabled { opacity: .5; cursor: default; }
  .btn { border: 1px solid var(--edge); border-radius: 6px; padding: 6px 12px; background: var(--term-bg); color: var(--fg); }
  .btn:hover:not(:disabled) { background: color-mix(in srgb, var(--accent) 8%, transparent); }
  .primary { border-color: color-mix(in srgb, var(--accent) 45%, var(--edge)); color: var(--accent); }
  .link { border: 0; padding: 3px 0; background: none; color: var(--accent); }
  .link.quiet { color: var(--muted); }
  .link:hover:not(:disabled) { text-decoration: underline; }
  .code { display: flex; flex-direction: column; align-items: flex-start; gap: 2px; width: fit-content; padding: 8px 14px; border: 1px dashed color-mix(in srgb, var(--fg) 25%, transparent); border-radius: 8px; background: none; color: var(--fg); text-align: left; }
  .code:hover { border-color: var(--accent); }
  .digits { font: 600 20px/1.2 var(--mono); letter-spacing: .14em; }
  .hint { font-size: var(--text-xs); color: var(--muted); }
  .paste { display: flex; gap: 8px; align-items: center; }
  .paste input { flex: 1; min-width: 0; box-sizing: border-box; border: 1px solid var(--edge); background: var(--term-bg); color: var(--fg); border-radius: 6px; padding: 6px 9px; font: var(--text-sm) var(--mono); }
  .paste input:focus { outline: 2px solid var(--focus-ring); outline-offset: 1px; }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 1px; }
</style>

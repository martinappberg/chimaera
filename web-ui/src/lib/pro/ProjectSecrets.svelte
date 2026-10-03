<script lang="ts">
  import { untrack } from "svelte";
  import ConfirmDialog from "../shared/ConfirmDialog.svelte";
  import { pageVisible } from "../shared/visibility";
  import { isBrowserGateway } from "../net/base";
  import { isNativeShell, navigateHome, onProChanged, proCloudProjects } from "../net/native";
  import { fetchHomeProjects } from "./accountHome";
  import { confirmsSecretAttempt, secretAttempt, secretNameAllowed, secretValueAllowed, type SecretAttempt, type SecretCommand, type SecretPolicy, type SecretProject } from "./projectSecrets";
  import { observeSecretContext, retainSecretAttempt, secretReconciliation, settleSecretAttempt } from "./projectSecretsMemory";
  import { readSecretOperation, readSecretPage, secretFailure, sendSecretCommand, type SecretFailureCode } from "./projectSecretsTransport";

  let { visible = true }: { visible?: boolean } = $props();
  let open = $state(false);
  let context = $state<string | null>(null);
  let policy = $state<SecretPolicy | null>(null);
  let projects = $state<SecretProject[]>([]);
  let labels = $state<Record<string, string>>({});
  let selected = $state("");
  let name = $state("");
  let value = $state("");
  let loading = $state(false);
  let sending = $state(false);
  let checking = $state(false);
  let fresh = $state(false);
  let failure = $state<SecretFailureCode | null>(null);
  let notice = $state("");
  let confirmation = $state<{ action: "apply" | "cancel" | "remove"; name?: string; row: SecretProject; context: string } | null>(null);
  let generation = 0;
  const native = isNativeShell();
  const row = $derived(projects.find(project => project.workspace_id === selected) ?? null);
  const attempts = $derived($secretReconciliation.context === context ? $secretReconciliation.attempts : []);
  const unresolved = $derived(attempts.find(attempt => attempt.workspace_id === selected) ?? null);
  const editable = $derived(fresh && context !== null && policy !== null && row?.state === "ready" && !sending && unresolved === null);
  const validEntry = $derived(policy !== null && secretNameAllowed(policy, name) && secretValueAllowed(value));
  function named(id: string): boolean { return Object.hasOwn(labels, id) && typeof labels[id] === "string"; }
  function title(id: string): string { return named(id) ? labels[id] : "Project name unavailable"; }
  const failureLine = $derived(failure === "unsupported" ? "Project secrets need an updated account connection. Check again after it has updated."
    : failure === "account_home_required" ? "Manage project secrets from your account Home."
    : failure === "sign_in_required" || failure === "context_changed" ? "Your account changed. Check again before making another decision."
    : failure === "state_changed" ? "This project's access changed. Check the current names and queued batch before trying again."
    : failure === "limit_reached" ? "There are too many unfinished requests. Check their original results before starting another."
    : failure === "operation_unavailable" || failure === "unconfirmed" ? "The result isn't confirmed. Check the original request; its value will never be sent again automatically."
    : failure !== null ? "Project secrets couldn't be checked just now. Try checking again." : "");

  async function projectNames(signal: AbortSignal): Promise<Record<string, string>> {
    const list = native ? await proCloudProjects() : (await fetchHomeProjects(signal)).projects;
    const result: Record<string, string> = Object.create(null);
    for (const project of list.slice(0, 256)) {
      if (project.name && project.name.trim() && new TextEncoder().encode(project.name).length <= 512 && !/[\u0000-\u001f\u007f]/.test(project.name)) result[project.workspace_id] = project.name;
    }
    return result;
  }
  async function load(signal?: AbortSignal): Promise<void> {
    const mine = ++generation;
    loading = true; fresh = false;
    const controller = signal ?? new AbortController().signal;
    try {
      const first = await readSecretPage(null, controller);
      const names = projectNames(controller).catch(() => ({}));
      const collected = [...first.catalog.projects];
      let after = first.catalog.next;
      for (let count = 1; after !== null; count++) {
        if (count >= 4) throw new Error("project_secrets_unsupported");
        const page = await readSecretPage(after, controller);
        if (page.context !== first.context || JSON.stringify(page.catalog.name_policy) !== JSON.stringify(first.catalog.name_policy)) throw new Error("project_secrets_context_changed");
        collected.push(...page.catalog.projects);
        if (collected.length > 128) throw new Error("project_secrets_unsupported");
        after = page.catalog.next;
      }
      const named = await names;
      if (controller.aborted || mine !== generation) return;
      if (context !== first.context) { value = ""; name = ""; confirmation = null; notice = ""; }
      observeSecretContext(first.context);
      context = first.context; policy = first.catalog.name_policy; projects = collected; labels = named;
      if (!collected.some(project => project.workspace_id === selected)) selected = collected[0]?.workspace_id ?? "";
      failure = null; fresh = true;
    } catch (reason) {
      if (!controller.aborted && mine === generation) { failure = secretFailure(reason).code; value = ""; confirmation = null; }
    } finally { if (mine === generation) loading = false; }
  }
  async function reconcile(attempt: SecretAttempt, signal?: AbortSignal): Promise<void> {
    if (checking || context !== attempt.context) return;
    checking = true;
    try {
      const result = await readSecretOperation(attempt.context, attempt.operation_id, signal);
      if (signal?.aborted || context !== attempt.context) return;
      if (!confirmsSecretAttempt(result, attempt)) throw new Error("project_secrets_unconfirmed");
      notice = result.receipt.outcome === "applying" ? "Stopping this project to apply access…"
        : result.receipt.outcome === "queued" ? "Queued until this project is safely idle."
        : result.receipt.outcome === "canceled" ? "Queued changes canceled. Applied access is unchanged." : "Project access updated. Resume the project when you’re ready.";
      failure = null;
      if (result.receipt.outcome !== "applying") settleSecretAttempt(attempt.context, attempt.operation_id);
      await load(signal);
    } catch (reason) { if (!signal?.aborted && context === attempt.context) failure = secretFailure(reason).code; }
    finally { checking = false; }
  }
  async function send(action: SecretCommand["action"], target: SecretProject, expectedContext: string, selectedName?: string): Promise<void> {
    if (!fresh || sending || context !== expectedContext || policy === null || row?.workspace_id !== target.workspace_id
      || row.revision !== target.revision || row.pending?.operation_id !== target.pending?.operation_id) { confirmation = null; value = ""; failure = "state_changed"; return; }
    const base = { version: 1 as const, operation_id: crypto.randomUUID(), workspace_id: target.workspace_id, expected_revision: target.revision, expected_pending: target.pending?.operation_id ?? null };
    const command: SecretCommand = action === "set" ? { ...base, action, name, value }
      : action === "remove" ? { ...base, action, name: selectedName ?? "" } : { ...base, action };
    const attempt = secretAttempt(command, expectedContext, target, policy);
    value = ""; confirmation = null;
    if (attempt === null) { failure = "invalid_request"; return; }
    if (!retainSecretAttempt(attempt)) { failure = "limit_reached"; return; }
    sending = true; failure = null; notice = "";
    try {
      const result = await sendSecretCommand(expectedContext, command);
      if (context !== expectedContext) return;
      if (!confirmsSecretAttempt(result, attempt)) throw new Error("project_secrets_unconfirmed");
      notice = result.receipt.outcome === "applying" ? "Stopping this project to apply access…"
        : result.receipt.outcome === "queued" ? "Queued until this project is safely idle." : result.receipt.outcome === "canceled" ? "Queued changes canceled. Applied access is unchanged." : "Project access updated. Resume the project when you’re ready.";
      if (result.receipt.outcome !== "applying") settleSecretAttempt(expectedContext, attempt.operation_id);
      name = "";
      await load();
    } catch (reason) {
      if (context === expectedContext) {
        failure = secretFailure(reason).code;
        // A fixed pre-effect refusal is conclusive; uncertain outcomes retain
        // the original redacted ID for passive reconciliation, never a resend.
        if (["invalid_request", "state_changed", "unsupported", "account_home_required"].includes(failure)) settleSecretAttempt(expectedContext, attempt.operation_id);
        fresh = false;
      }
    } finally { sending = false; }
  }
  $effect(() => {
    if (!visible || !open || !$pageVisible) { untrack(() => { value = ""; confirmation = null; fresh = false; }); return; }
    const controller = new AbortController();
    untrack(() => void load(controller.signal));
    const timer = setInterval(() => untrack(() => {
      if (sending || loading || checking) return;
      const attempt = $secretReconciliation.context === context
        ? $secretReconciliation.attempts.find(item => item.workspace_id === selected) ?? $secretReconciliation.attempts[0] : null;
      if (attempt) void reconcile(attempt, controller.signal);
      else void load(controller.signal);
    }), 30_000);
    return () => { generation++; controller.abort(); clearInterval(timer); loading = false; };
  });
  $effect(() => {
    if (!native) return;
    let dead = false;
    let dispose: (() => void) | undefined;
    void onProChanged(() => { generation++; fresh = false; value = ""; confirmation = null; context = null; policy = null; projects = []; labels = {}; if (visible && open && document.visibilityState === "visible") void load(); }).then(off => { if (dead) off(); else dispose = off; });
    return () => { dead = true; dispose?.(); };
  });
</script>

<details class="secrets" ontoggle={event => { open = event.currentTarget.open; }}>
  <summary>Project secrets</summary>
  <div class="content">
    <p class="muted">Choose which project can use each secret. Existing values are never shown. Adding or replacing a value waits until the project is safely idle.</p>
    {#if failureLine}<p role="status" class="message">{failureLine}</p>{/if}
    {#if failure === "account_home_required"}
      {#if native}<button onclick={() => void navigateHome(null)}>Open account Home</button>{:else if isBrowserGateway()}<a href="/">Open account Home</a>{/if}
    {:else}
      <button class="quiet" disabled={loading || sending} onclick={() => void load()}>{loading ? "Checking…" : "Check again"}</button>
    {/if}
    {#if context && policy && projects.length > 0}
      <label class="field">Project<select bind:value={selected} onchange={() => { value = ""; name = ""; confirmation = null; notice = ""; }}>{#each projects as project (project.workspace_id)}<option value={project.workspace_id}>{title(project.workspace_id)}</option>{/each}</select></label>
      {#if row}
        {#if !named(row.workspace_id)}<p class="muted identifier">Project reference: <code>{row.workspace_id}</code></p>{/if}
        {#if row.state !== "ready"}<p role="status">{row.state === "applying" ? "This project's access is being updated." : "This project can't accept access changes just now."}</p>{/if}
        <h3>Applied access</h3>
        {#if row.applied_names.length === 0}<p class="muted">No custom secrets applied.</p>{/if}
        {#each row.applied_names as applied (applied)}<div class="name-row"><code>{applied}</code><button class="quiet" disabled={!editable} onclick={() => { if (row && context) confirmation = { action: "remove", name: applied, row, context }; }}>Remove access</button></div>{/each}
        {#if row.pending}<div class="queued"><h3>Queued until idle</h3><p class="names">{row.pending.names.join(", ")}</p><p class="muted">These changes stay queued until they can be applied safely.</p><div class="actions"><button disabled={!editable} onclick={() => { if (row && context) confirmation = { action: "apply", row, context }; }}>Apply now…</button><button class="quiet" disabled={!editable} onclick={() => { if (row && context) confirmation = { action: "cancel", row, context }; }}>Cancel queued batch…</button></div></div>{/if}
        {#if unresolved}<div class="queued" role="status"><p>Checking the original request for {unresolved.names.join(", ")}. The value will not be resent.</p><button disabled={checking || sending} onclick={() => void reconcile(unresolved)}>{checking ? "Checking…" : "Check original request"}</button></div>{/if}
        <form onsubmit={event => { event.preventDefault(); if (row && context && editable && validEntry) void send("set", row, context); }}>
          <h3>Add or replace a secret</h3>
          <label class="field">Name<input autocomplete="off" autocapitalize="characters" spellcheck="false" maxlength="128" bind:value={name} disabled={!editable} placeholder="SERVICE_TOKEN" /></label>
          <label class="field">New value<input type="password" autocomplete="off" spellcheck="false" maxlength="8192" bind:value={value} disabled={!editable} /></label>
          {#if name && policy && !secretNameAllowed(policy, name)}<p class="muted">Use uppercase letters, digits and underscores. Provider and runtime names are reserved.</p>{/if}
          {#if value && !secretValueAllowed(value)}<p class="message">Enter a nonempty value of at most 8 KiB, without NUL characters.</p>{/if}
          {#if row.pending}<p class="muted">This update carries the shown queued batch: {row.pending.names.join(", ")}.</p>{/if}
          <button type="submit" disabled={!editable || !validEntry}>{sending ? "Sending…" : "Queue until idle"}</button>
        </form>
      {/if}
    {:else if fresh}<p class="muted">No selected projects are available for secret access.</p>{/if}
    {#if notice}<p role="status" class="message">{notice}</p>{/if}
  </div>
</details>
{#if confirmation}
  {@const choice = confirmation}
  <ConfirmDialog title={choice.action === "apply" ? "Apply queued secrets now?" : choice.action === "cancel" ? "Cancel this queued batch?" : "Remove secret access?"}
    body={choice.action === "apply" ? `Stop ${title(choice.row.workspace_id)} to apply every name in the shown batch. It stays stopped until you explicitly resume it. Other projects keep running.`
      : choice.action === "cancel" ? "Cancel every name in this queued batch. Applied access stays unchanged."
      : `Removing access immediately stops ${title(choice.row.workspace_id)}. ${choice.row.pending ? "Its entire shown queued batch is also canceled." : "Other projects keep running."}`}
    detail={choice.action === "remove" ? `${choice.name}${choice.row.pending ? `\nQueued batch canceled: ${choice.row.pending.names.join(", ")}` : ""}` : choice.row.pending?.names.join("\n") ?? ""}
    confirmLabel={choice.action === "apply" ? "Stop and apply now" : choice.action === "cancel" ? "Cancel batch" : "Stop and remove access"} danger={choice.action !== "cancel"}
    onCancel={() => { confirmation = null; }} onConfirm={() => void send(choice.action, choice.row, choice.context, choice.name)} />
{/if}

<style>
  .secrets { margin: 14px 0; border: 1px solid var(--edge); border-radius: 8px; color: var(--fg); }
  summary { padding: 13px 16px; cursor: pointer; font-weight: 550; font-size: var(--text-sm); }
  .content { padding: 0 16px 16px; display: grid; gap: 12px; min-width: 0; }
  p { margin: 0; font-size: var(--text-sm); line-height: 1.6; overflow-wrap: anywhere; }
  .muted { color: var(--muted); }
  .message { color: var(--fg); }
  h3 { margin: 4px 0 0; font-size: var(--text-sm); font-weight: 550; }
  .field { display: grid; gap: 6px; font-size: var(--text-sm); }
  select, input { box-sizing: border-box; width: 100%; min-width: 0; padding: 8px 10px; border: 1px solid var(--edge); border-radius: 5px; background: var(--bg); color: var(--fg); font: inherit; }
  form { display: grid; gap: 12px; border-top: 1px solid var(--edge); padding-top: 12px; }
  button, a { justify-self: start; padding: 8px 11px; border: 1px solid var(--edge); border-radius: 5px; background: color-mix(in srgb, var(--fg) 5%, var(--bg)); color: var(--fg); font: inherit; font-size: var(--text-sm); cursor: pointer; text-decoration: none; }
  button.quiet { background: transparent; }
  button:disabled, input:disabled { opacity: .5; cursor: default; }
  button:focus-visible, a:focus-visible, select:focus-visible, input:focus-visible, summary:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  .queued { padding: 12px; border: 1px solid var(--edge); border-radius: 6px; display: grid; gap: 8px; }
  .actions, .name-row { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
  .name-row { justify-content: space-between; }
  code, .names { overflow-wrap: anywhere; word-break: break-word; }
  .identifier { font-size: var(--text-xs); }
  @media (pointer: coarse) { button, select, input, a { min-height: 40px; } }
</style>

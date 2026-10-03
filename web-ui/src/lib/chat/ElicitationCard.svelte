<script lang="ts">
  import { browserRequestUrl, formErrors, formValues, loopbackUrl, type ElicitationAction, type PendingElicitation } from "./elicitation";
  import ElicitationFields from "./ElicitationFields.svelte";
  import { openInSystemBrowser } from "../shared/urlOpen";
  import { copyText } from "../shared/clipboard";

  let { request, onRespond, visible = true }: {
    request: PendingElicitation;
    onRespond: (action: ElicitationAction, content: Record<string, unknown> | null) => boolean;
    visible?: boolean;
  } = $props();
  let inputs = $state<Record<string, unknown>>(Object.create(null));
  let errors = $state<Record<string, string>>(Object.create(null));
  let feedback = $state("");
  const form = $derived(request.elicitation);
  const url = $derived(browserRequestUrl(form.url));
  const loopback = $derived(loopbackUrl(url));
  function set(name: string, next: unknown) {
    inputs[name] = next;
    delete errors[name];
  }
  function submit(event: SubmitEvent) {
    event.preventDefault();
    if (form.unsupported) return;
    const values = formValues(form.fields, inputs);
    errors = formErrors(form.fields, values);
    if (Object.keys(errors).length) return;
    onRespond("accept", form.mode === "url" ? null : values);
  }
  async function copyUrl() {
    if (url === null) return;
    try { feedback = await copyText(url.href) ? "Link copied." : "Could not copy the link."; }
    catch { feedback = "Could not copy the link."; }
  }
</script>

<form class="elicitation" aria-label={`Request from ${request.server}`} onsubmit={submit} inert={!visible}>
  <div class="heading"><span class="server">{request.server}</span><span class="badge">{form.mode === "url" ? "browser request" : "MCP form"}</span></div>
  <p class="message">{request.message}</p>
  {#if form.unsupported}
    <p class="problem">{form.unsupported}</p>
  {:else if form.mode === "url"}
    {#if url !== null}
      <div class="browser-actions">
        <button class="open" type="button" onclick={() => openInSystemBrowser(url!.href)}>Open {url.host}</button>
        <button type="button" onclick={copyUrl}>Copy link</button>
      </div>
      <p class="help">Complete the requested step in your browser, then return here. Opening the page does not confirm that sign-in succeeded.</p>
      {#if loopback}<p class="help">This address points to the computer running the agent. For a remote workspace, use its forwarded port or open the link on that host.</p>{/if}
    {:else}
      <p class="problem">The server supplied an invalid web address.</p>
    {/if}
  {:else}
    <ElicitationFields fields={form.fields} {inputs} {errors} setValue={set} />
    <p class="help">Your answers go to this MCP server. Chimaera records the decision without storing these answers in the conversation.</p>
  {/if}
  {#if feedback}<p class="help" role="status">{feedback}</p>{/if}
  <div class="actions">
    <button class="primary" type="submit" disabled={!!form.unsupported || (form.mode === "url" && url === null)}>{form.mode === "url" ? "Continue" : "Submit"}</button>
    <button type="button" onclick={() => onRespond("decline", null)}>Decline</button>
    <button type="button" onclick={() => onRespond("cancel", null)}>Cancel</button>
  </div>
</form>

<style>
  .elicitation { border: 1px solid var(--edge); border-left: 3px solid var(--accent); border-radius: 8px; padding: 16px; margin: 12px 0; background: var(--panel); color: var(--fg); }
  .heading, .actions, .browser-actions { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; }
  .server { font-weight: 600; overflow-wrap: anywhere; }
  .badge { font-size: var(--text-xs); color: var(--muted); }
  .message { white-space: pre-wrap; overflow-wrap: anywhere; margin: 10px 0; }
  .help { color: var(--muted); font-size: var(--text-xs); line-height: 1.5; overflow-wrap: anywhere; }
  .problem { color: var(--err); font-size: var(--text-sm); }
  button, .open { font: inherit; font-size: var(--text-sm); color: var(--fg); background: transparent; border: 1px solid var(--edge); border-radius: 5px; padding: 6px 12px; cursor: pointer; text-decoration: none; }
  .primary, .open { border-color: var(--accent); color: var(--accent); }
  button:disabled { opacity: .5; cursor: default; }
  .actions { margin-top: 14px; }
</style>

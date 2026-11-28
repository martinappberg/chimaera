<script lang="ts">
  /**
   * Where a file meets the plugins active in its workspace
   * (docs/design/plugin-platform-plan.md §5): a file a plugin claims (`[[files]]`)
   * opens in that plugin's file view, with **Text** one click away; two
   * plugins claiming it offer the choice, remembered per workspace and file
   * kind. The bar above also carries the claiming plugins' status chips
   * (`slot = "status"`) and every matching file action (`[[actions]]`).
   * A file no active plugin claims but an installed one switched off here
   * would gets a one-click Turn on (Not now is remembered per workspace).
   * A file action may open one of its plugin's file views on a file,
   * claimed or not (`fileViewRequest`): that view joins the switch for this
   * opening only, and is not remembered.
   * With none of that, the file shows exactly as it always did.
   */
  import type { Snippet } from "svelte";
  import { setWorkspacePluginOn, workspacePlugins } from "../store";
  import {
    actionsFor,
    claimsFor,
    declinedOffers,
    declineOffer,
    fileActionTarget,
    fileViewRequest,
    FILE_VIEW_REQUEST_MS,
    offersFor,
    openPluginView,
    openWith,
    postFileAction,
    rememberOpenWith,
    viewClaim,
    viewsIn,
    workspaceRelative,
  } from "../platform";
  import { openPath, pathPaneFrom } from "../../shared/openPath";
  import PluginScreen from "./PluginScreen.svelte";

  interface Props {
    path: string;
    wsRoot: string | null;
    children: Snippet;
  }

  let { path, wsRoot, children }: Props = $props();

  const wsId = $derived($workspacePlugins?.workspace_id ?? null);
  const plugins = $derived($workspacePlugins?.plugins ?? []);
  const rel = $derived(workspaceRelative(wsRoot, path));
  const claims = $derived(rel !== null ? claimsFor(plugins, rel) : []);
  const actions = $derived(rel !== null ? actionsFor(plugins, rel) : []);
  let declined = $state<string[]>([]);
  $effect(() => {
    declined = wsId !== null ? declinedOffers(wsId) : [];
  });
  const offer = $derived(
    rel !== null && claims.length === 0
      ? (offersFor(plugins, rel).find((o) => !declined.includes(o.plugin.id)) ?? null)
      : null,
  );
  const chips = $derived(
    viewsIn(plugins, "status").filter((s) => claims.some((c) => c.plugin.id === s.plugin.id)),
  );

  /** A file view a file action asked for on this file (see the header),
   *  shown while `showAsked` (until another choice is picked). */
  let asked = $state<{ path: string; plugin: string; view: string } | null>(null);
  let showAsked = $state(false);
  let askedSeen = 0;
  $effect(() => {
    const req = $fileViewRequest;
    if (req === null || req.path !== path || req.at <= askedSeen) return;
    askedSeen = req.at;
    if (Date.now() - req.at > FILE_VIEW_REQUEST_MS) return;
    asked = { path, plugin: req.plugin, view: req.view };
    showAsked = true;
  });
  const askedClaim = $derived(asked !== null && asked.path === path ? viewClaim(plugins, asked.plugin, asked.view) : null);
  /** The switch's choices: the claimants, and the asked-for view if it is not one. */
  const askedExtra = $derived(
    askedClaim !== null && !claims.some((c) => c.plugin.id === askedClaim.plugin.id && c.view.id === askedClaim.view.id)
      ? askedClaim
      : null,
  );

  /** A plugin id, `"text"`, or null (the first claimant). */
  let picked = $state<string | null>(null);
  $effect(() => {
    const first = claims[0];
    picked = wsId !== null && first !== undefined ? openWith(wsId, first.kind.label) : null;
  });
  const claim = $derived(
    showAsked && askedClaim !== null
      ? askedClaim
      : picked === "text"
        ? null
        : (claims.find((c) => c.plugin.id === picked) ?? claims[0] ?? null),
  );

  function pick(choice: string): void {
    showAsked = false;
    picked = choice;
    const first = claims[0];
    if (wsId !== null && first !== undefined) {
      rememberOpenWith(wsId, first.kind.label, choice === first.plugin.id ? null : choice);
    }
  }

  let gateEl = $state<HTMLElement | null>(null);
  let running = $state<string | null>(null);
  let message = $state<{ text: string; bad: boolean } | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => () => clearTimeout(timer));

  let turning = $state(false);

  async function turnOn(pid: string): Promise<void> {
    if (turning) return;
    turning = true;
    try {
      await setWorkspacePluginOn(pid, true);
    } catch (e) {
      message = { text: e instanceof Error ? e.message : String(e), bad: true };
    } finally {
      turning = false;
    }
  }

  function notNow(pid: string): void {
    if (wsId === null) return;
    declineOffer(wsId, pid);
    declined = [...declined, pid];
  }

  async function runAction(pid: string, action: string): Promise<void> {
    if (wsId === null || rel === null || running !== null) return;
    running = `${pid}/${action}`;
    try {
      const answer = await postFileAction(wsId, pid, action, rel);
      if (answer.message !== null) message = { text: answer.message, bad: false };
      const target = answer.open === null ? null : fileActionTarget(plugins, pid, answer.open, rel);
      if (target?.kind === "tab") openPluginView(pid, target.view);
      else if (target?.kind === "file" && wsRoot !== null) {
        const file = `${wsRoot.replace(/\/+$/, "")}/${target.file}`;
        fileViewRequest.set({ path: file, plugin: pid, view: target.view, at: Date.now() });
        if (file !== path) openPath(file, "file", { fromPane: pathPaneFrom(gateEl ?? null) });
      }
    } catch (e) {
      message = { text: e instanceof Error ? e.message : String(e), bad: true };
    } finally {
      running = null;
      clearTimeout(timer);
      timer = setTimeout(() => (message = null), 6000);
    }
  }
</script>

{#if wsId === null || rel === null || (claims.length === 0 && askedClaim === null && actions.length === 0 && offer === null)}
  {@render children()}
{:else}
  <div class="gate" bind:this={gateEl}>
    <div class="bar">
      {#each chips as c (`${c.plugin.id}/${c.view.id}`)}
        <div class="chip" title={c.view.title}>
          <PluginScreen ws={wsId} {wsRoot} plugin={c.plugin.id} view={c.view.id} file={rel} compact />
        </div>
      {/each}
      {#if offer !== null}
        <span class="offer">
          <strong>{offer.plugin.name}</strong> can open this file. It's off in this workspace.
        </span>
        <button class="opt small primary" disabled={turning} onclick={() => void turnOn(offer.plugin.id)}
          >{turning ? "Turning on…" : "Turn on"}</button
        >
        <button class="opt small quiet" disabled={turning} onclick={() => notNow(offer.plugin.id)}>Not now</button>
      {/if}
      {#if message !== null}
        <span class="message" class:bad={message.bad} role="status">{message.text}</span>
      {/if}
      <span class="spacer"></span>
      {#each actions as a (`${a.plugin.id}/${a.action.action}`)}
        <button
          class="opt small quiet"
          title="{a.action.label} ({a.plugin.name})"
          disabled={running !== null}
          onclick={() => void runAction(a.plugin.id, a.action.action)}>{a.action.label}</button
        >
      {/each}
      {#if claims.length > 0 || askedExtra !== null}
        <div class="switch" role="tablist" aria-label="open with">
          {#each claims as c (c.plugin.id)}
            <button
              class="seg"
              class:on={claim?.plugin.id === c.plugin.id}
              role="tab"
              aria-selected={claim?.plugin.id === c.plugin.id}
              title="Open with {c.plugin.name}"
              onclick={() => pick(c.plugin.id)}>{claims.length > 1 ? c.plugin.name : c.kind.label}</button
            >
          {/each}
          {#if askedExtra !== null}
            <button
              class="seg"
              class:on={claim === askedExtra}
              role="tab"
              aria-selected={claim === askedExtra}
              title="Open with {askedExtra.plugin.name}"
              onclick={() => (showAsked = true)}>{askedExtra.view.title}</button
            >
          {/if}
          <button class="seg" class:on={claim === null} role="tab" aria-selected={claim === null} onclick={() => pick("text")}
            >Text</button
          >
        </div>
      {/if}
    </div>
    <div class="body">
      {#if claim !== null}
        <div class="screen-scroll">
          {#key `${claim.plugin.id}/${claim.view.id}/${rel}`}
            <PluginScreen ws={wsId} {wsRoot} plugin={claim.plugin.id} view={claim.view.id} file={rel} fill />
          {/key}
        </div>
      {:else}
        {@render children()}
      {/if}
    </div>
  </div>
{/if}

<style>
  .gate {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .bar {
    flex: none;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border-bottom: 1px solid var(--edge);
    min-height: 30px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
  }
  .spacer {
    flex: 1;
  }
  .message,
  .offer {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  /* The bar's buttons sit at the switch's size. */
  .opt.small {
    font-size: var(--text-xs);
    padding: 2px 10px;
  }
  .offer strong {
    color: var(--fg);
    font-weight: 600;
  }
  .message.bad {
    color: var(--err);
  }
  .switch {
    display: inline-flex;
    border: 1px solid var(--edge);
    border-radius: 6px;
    overflow: hidden;
  }
  .seg {
    appearance: none;
    border: none;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    padding: 2px 10px;
    cursor: pointer;
  }
  .seg + .seg {
    border-left: 1px solid var(--edge);
  }
  .seg.on {
    background: var(--row-active);
    color: var(--fg);
  }
  .body {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .body > :global(*) {
    flex: 1;
    min-height: 0;
  }
  .screen-scroll {
    overflow: auto;
    background: var(--bg);
  }
</style>

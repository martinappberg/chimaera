<script lang="ts">
  import CloudSetup from "./CloudSetup.svelte";
  import BrandMark from "../shared/BrandMark.svelte";
  import PlanBadge from "../shared/PlanBadge.svelte";
  import { onMount, untrack } from "svelte";
  import MirrorSettings from "./MirrorSettings.svelte";
  import { asyncDisposer } from "../shared/asyncDisposer";
  import { pageVisible } from "../shared/visibility";
  import {
    onProChanged, proStatus, proSignIn, proSignOut, proSignOutEverywhere,
    proHosts, proSetHostKept, proDevices,
    type ProStatus, type ProHost, type ProDevice,
  } from "../net/native";

  let { visible = true }: { visible?: boolean } = $props();
  let status = $state<ProStatus | null>(null);
  let hosts = $state<ProHost[]>([]);
  let devices = $state<ProDevice[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let busy = $state<string | null>(null);
  let revision = $state(0);
  let generation = 0;
  let alive = true;

  async function load(): Promise<void> {
    const request = ++generation;
    try {
      const next = await proStatus();
      if (!alive || request !== generation) return;
      status = next;
      if (next.signed_in) {
        const [nextHosts, nextDevices] = await Promise.all([proHosts(), proDevices()]);
        if (!alive || request !== generation) return;
        hosts = nextHosts;
        devices = nextDevices;
      } else {
        hosts = [];
        devices = [];
      }
      error = null;
    } catch (reason) {
      if (alive && request === generation) error = reason instanceof Error ? reason.message : String(reason);
    } finally {
      if (alive && request === generation) loading = false;
    }
  }

  // Events refresh visible settings only. Returning to the tab catches up;
  // there is no account polling while a settings tab is parked.
  $effect(() => {
    revision;
    if (visible && $pageVisible) untrack(() => void load());
  });
  onMount(() => {
    const dispose = asyncDisposer(onProChanged(() => { revision += 1; }));
    return () => { alive = false; generation += 1; dispose(); };
  });

  async function act(name: string, operation: () => Promise<void>): Promise<void> {
    if (busy !== null) return;
    busy = name;
    error = null;
    try { await operation(); await load(); }
    catch (reason) { error = reason instanceof Error ? reason.message : String(reason); }
    finally { busy = null; }
  }

  function setKept(host: ProHost, checkbox: HTMLInputElement): void {
    const kept = checkbox.checked;
    // The keeper is authoritative. Preserve the confirmed check mark until
    // the command succeeds, including when a fresh sign-in is required.
    checkbox.checked = host.kept;
    void act(`host:${host.alias}`, () => proSetHostKept(host.alias, kept));
  }

  function lastSeen(value: string): string {
    const date = new Date(value);
    return Number.isNaN(date.valueOf()) ? "Last seen unavailable" : `Last seen ${date.toLocaleString()}`;
  }
</script>

<section class="pro" aria-label="Chimaera Pro">
  <h2 class="cat">Chimaera Pro</h2>
  {#if loading && status === null}
    <p class="state" role="status">Loading account…</p>
  {:else if status !== null && !status.available}
    <p class="state">Chimaera Pro isn't available in this build.</p>
  {:else if status !== null && !status.signed_in}
    <p class="intro">Sign in to keep your remote machines connected through Chimaera Pro.</p>
    <div class="actions">
      <button class="btn primary" disabled={busy !== null} onclick={() => void act("sign-in", proSignIn)}>
        {busy === "sign-in" ? "Finish signing in in your browser…" : "Sign in"}
      </button>
    </div>
  {:else if status?.signed_in}
    <div class="account" class:subscribed={status.plan === "pro" || status.plan === "max"}>
      <BrandMark size={32} />
      <div class="account-identity">
        <div class="account-brand">
          <span class="wordmark">chimaera</span>
          <PlanBadge plan={status.plan === "pro" || status.plan === "max" ? status.plan : null} />
          {#if status.plan !== "pro" && status.plan !== "max"}<span class="plan">No active plan</span>{/if}
        </div>
        <span class="email">{status.email}</span>
      </div>
    </div>

    <div class="group">
      <h3>Hosts kept connected</h3>
      {#if hosts.length === 0}
        <p class="hint">Add a remote host on Home to keep it connected here.</p>
      {:else}
        <ul class="rows">
          {#each hosts as host (host.alias)}
            <li class="row">
              <div class="detail">
                <span class="name">{host.alias}</span>
                <span class="hint" role="status">
                  {host.status === "prompting" ? "Waiting for authentication" : host.status === "connecting" ? "Connecting…" : host.status === "connected" ? "Connected" : "Offline"}
                  {#if host.kind === "device"} · Device{/if}
                </span>
              </div>
              {#if host.kind === "ssh"}
                <label class="keep">
                  <input type="checkbox" checked={host.kept} disabled={busy !== null}
                    onchange={(event) => setKept(host, event.currentTarget)} />
                  <span>Keep connected</span>
                </label>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <div class="group">
      <h3>Devices</h3>
      <ul class="rows">
        {#each devices as device (device.id)}
          <li class="row">
            <div class="detail">
              <span class="name">{device.name}</span>
              <span class="hint">{lastSeen(device.last_seen)}</span>
            </div>
            {#if device.this}<span class="current">This device</span>{/if}
          </li>
        {/each}
      </ul>
    </div>

    <CloudSetup {visible} />

    <MirrorSettings {visible} />

    <div class="actions">
      <button class="btn" disabled={busy !== null} onclick={() => void act("sign-out", proSignOut)}>
        {busy === "sign-out" ? "Signing out…" : "Sign out"}
      </button>
      <button class="btn" disabled={busy !== null} onclick={() => void act("sign-out-everywhere", proSignOutEverywhere)}>
        {busy === "sign-out-everywhere" ? "Signing out everywhere…" : "Sign out everywhere"}
      </button>
    </div>
    <p class="hint signout-hint">Signing out everywhere also closes the SSH logins kept by Pro.</p>
  {/if}
  {#if error !== null || status?.error}
    <div class="error" role="alert">
      <span>{error ?? status?.error}</span>
      <button class="btn" disabled={busy !== null} onclick={() => void load()}>Retry</button>
    </div>
  {/if}
</section>

<style>
  .pro { display: flex; flex-direction: column; padding-bottom: 14px; }
  .cat { margin: 18px 0 4px; padding: 0 14px; font-size: var(--text-xs); font-weight: 600; letter-spacing: .1em; text-transform: uppercase; color: var(--muted); }
  .intro, .state { margin: 0; padding: 8px 14px; font-size: var(--text-sm); line-height: 1.5; color: var(--muted); max-width: 64ch; }
  .account { display: flex; gap: 12px; align-items: center; margin: 10px 14px; padding: 16px; border: 1px solid var(--edge); border-radius: 10px; }
  .account.subscribed { border-color: color-mix(in srgb, var(--accent) 24%, var(--edge)); background: linear-gradient(115deg, color-mix(in srgb, var(--accent) 7%, transparent), transparent 80%); }
  .account-identity { display: flex; flex-direction: column; gap: 5px; min-width: 0; }
  .account-brand { display: flex; flex-wrap: wrap; align-items: center; gap: 9px; }
  .wordmark { font-size: var(--text-lg); font-weight: 600; letter-spacing: .01em; }
  .email { font-size: var(--text-sm); color: var(--muted); overflow-wrap: anywhere; }
  .plan, .current { font-size: var(--text-xs); color: var(--muted); flex: none; }
  .group { padding: 10px 14px; }
  h3 { margin: 0 0 8px; font-size: var(--text-sm); font-weight: 600; }
  .rows { list-style: none; margin: 0; padding: 0; border: 1px solid var(--edge); border-radius: 8px; overflow: hidden; }
  .row { display: flex; align-items: center; flex-wrap: wrap; gap: 10px 16px; padding: 12px; }
  .row + .row { border-top: 1px solid var(--edge); }
  .detail { flex: 1; display: flex; flex-direction: column; gap: 3px; min-width: 120px; overflow-wrap: anywhere; }
  .name { font-size: var(--text-md); }
  .hint { margin: 0; font-size: var(--text-sm); color: var(--muted); line-height: 1.4; }
  .keep { display: flex; gap: 7px; align-items: center; font-size: var(--text-sm); cursor: pointer; }
  .keep input { accent-color: var(--accent); width: 16px; height: 16px; margin: 0; }
  .actions { display: flex; flex-wrap: wrap; gap: 8px; padding: 10px 14px; }
  .btn { appearance: none; border: 1px solid var(--edge); border-radius: 6px; padding: 5px 10px; background: var(--term-bg); color: var(--fg); font: inherit; font-size: var(--text-sm); cursor: pointer; }
  .btn:hover:not(:disabled) { background: var(--row-hover); }
  .btn:focus-visible, .keep input:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  .btn:disabled { opacity: .5; cursor: default; }
  .primary { color: var(--accent); border-color: color-mix(in srgb, var(--accent) 45%, var(--edge)); }
  .signout-hint { padding: 0 14px; }
  .error { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; margin: 8px 14px; padding: 8px 10px; border-radius: 6px; color: var(--warn); background: color-mix(in srgb, var(--warn) 10%, transparent); font-size: var(--text-sm); overflow-wrap: anywhere; }
  .error span { flex: 1; min-width: 120px; }
</style>

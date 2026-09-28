<script lang="ts">
  import type { ProDevice } from "../net/native";
  import { groupDevices, lastSeen } from "./devices";
  let { devices, busy = false, onrevoke }: { devices: ProDevice[]; busy?: boolean; onrevoke: (device: ProDevice) => Promise<void> } = $props();
  const groups = $derived(groupDevices(devices));
  let pending = $state<ProDevice | null>(null);
  $effect(() => { if (pending && !devices.some(value => value.id === pending?.id)) pending = null; });
  async function revoke(): Promise<void> {
    const target = pending;
    if (!target || busy) return;
    await onrevoke(target);
    if (pending?.id === target.id) pending = null;
  }
</script>

{#snippet signInRow(signIn: ProDevice)}
  <div class="row"><div><span>{signIn.name}</span><span class="muted detail">{lastSeen(signIn.last_seen)}</span></div>{#if !signIn.this}<button class="text-button" disabled={busy} onclick={() => pending = signIn}>Sign out</button>{/if}</div>
{/snippet}

<div class="devices">
  {#each groups.devices as device (device.id)}
    <div class="device"><div class="row"><div><strong>{device.name}</strong><span class="muted detail">{device.current ? "This device" : lastSeen(device.lastSeen)}</span></div>{#if !device.current && device.signIns.length === 1}<button class="text-button" disabled={busy} onclick={() => pending = device.signIns[0]}>Sign out</button>{/if}</div>
      {#if device.signIns.length > 1}<details><summary>Sign-ins on this device</summary>{#each device.signIns as signIn (signIn.id)}{@render signInRow(signIn)}{/each}</details>{/if}
    </div>
  {/each}
  {#if groups.currentSignIn}<div class="row"><div><strong>This sign-in</strong><span class="muted detail">{lastSeen(groups.currentSignIn.last_seen)}</span></div></div>{/if}
  {#if groups.otherSignIns.length}<details class="other"><summary>Other sign-ins <span class="count">{groups.otherSignIns.length}</span></summary><p class="muted">These may include earlier sign-ins on this same computer. Remove any you no longer use.</p>{#each groups.otherSignIns as signIn (signIn.id)}{@render signInRow(signIn)}{/each}</details>{/if}
  {#if pending}<div class="confirmation" role="group" aria-label="Confirm sign-out"><strong>Sign out “{pending.name}”?</strong><p class="muted">This removes that sign-in’s access to your account. Your current sign-in stays connected.</p><div class="actions"><button disabled={busy} onclick={() => void revoke()}>{busy ? "Signing out…" : "Sign out"}</button><button class="secondary" disabled={busy} onclick={() => pending = null}>Cancel</button></div></div>{/if}
</div>

<style>
  .devices { display: grid; gap: 12px; }
  .row { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 12px 0; }
  .row > div { min-width: 0; overflow-wrap: anywhere; }
  strong { font-weight: 550; }
  .muted { color: var(--muted); }
  .detail { display: block; margin-top: 4px; font-size: var(--text-sm); }
  p { line-height: 1.6; font-size: var(--text-sm); }
  details { border-top: 1px solid var(--edge); padding-top: 14px; }
  summary { cursor: pointer; font-size: var(--text-sm); }
  .count { margin-left: 5px; color: var(--muted); }
  button { flex-shrink: 0; padding: 9px 14px; background: var(--fg); color: var(--bg); border: 1px solid transparent; border-radius: 7px; font: inherit; font-size: var(--text-sm); cursor: pointer; }
  button:disabled { opacity: .5; cursor: default; }
  .text-button { padding: 4px 0; background: transparent; color: var(--fg); text-decoration: underline; text-decoration-color: var(--edge); text-underline-offset: 4px; }
  .secondary { color: var(--fg); background: transparent; border-color: var(--edge); }
  .confirmation { overflow-wrap: anywhere; padding: 18px; border: 1px solid var(--edge); border-radius: 9px; }
  .actions { display: flex; flex-wrap: wrap; gap: 10px; }
  button:focus-visible, summary:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
</style>

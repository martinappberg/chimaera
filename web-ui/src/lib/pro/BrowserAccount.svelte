<script lang="ts">
  /**
   * Settings → Chimaera Pro on the web's Home (the account's own page): the
   * plan, usage and sign-out, and the one link to the billing page. This is
   * where the desktop app keeps them too; nothing billing-related shows on
   * Home. Read passively while shown; never a checkout or a price here.
   */
  import { untrack } from "svelte";
  import AccountUsage from "./AccountUsage.svelte";
  import BrandMark from "../shared/BrandMark.svelte";
  import PlanBadge from "../shared/PlanBadge.svelte";
  import { pageVisible } from "../shared/visibility";
  import type { ProStatus } from "../net/native";
  import { BILLING_PATH, fetchBrowserAccount, signOutBrowser } from "./accountHome";
  import { forgetCatalogs } from "./catalogMemory";
  import { grantedPlan, paymentDue, returningUntil } from "./status";
  import { returningLine } from "./presentation";

  let { visible = true }: { visible?: boolean } = $props();
  let status = $state<ProStatus | null>(null);
  let failed = $state(false);
  let signingOut = $state(false);
  let signOutFailed = $state(false);

  $effect(() => {
    if (!visible || !$pageVisible) return;
    const controller = new AbortController();
    untrack(() => {
      void fetchBrowserAccount(controller.signal).then(
        (next) => { status = next; failed = false; },
        () => { if (!controller.signal.aborted) failed = true; },
      );
    });
    return () => controller.abort();
  });

  const plan = $derived(grantedPlan(status));
  const paid = $derived(plan === "pro" || plan === "max");
  const due = $derived(paymentDue(status));
  const returning = $derived(returningLine(returningUntil(status)));

  async function signOut(): Promise<void> {
    signingOut = true;
    signOutFailed = false;
    try {
      await signOutBrowser();
      // The next account signed in here starts without this one's agent rows.
      forgetCatalogs();
      location.assign("/");
    } catch {
      signOutFailed = true;
      signingOut = false;
    }
  }
</script>

<div class="account">
  <div class="identity">
    <BrandMark size={30} />
    <div class="copy">
      <div class="title">
        <strong>{paid ? `Your Chimaera ${plan === "max" ? "Max" : "Pro"}` : "Chimaera Pro"}</strong>
        <PlanBadge plan={paid && (plan === "pro" || plan === "max") ? plan : null} ended={returning !== null} />
      </div>
      {#if status?.email}<span class="email">Signed in as {status.email}</span>{/if}
    </div>
  </div>
  {#if status === null}
    <p class="note" role="status">{failed ? "Your account couldn't load just now. It loads again when you come back to this page." : "Checking your plan…"}</p>
  {:else if due}
    <p class="note warn">Your last payment didn't go through. Update your payment details in billing to keep your plan.</p>
  {:else if returning !== null}
    <p class="note">{returning}</p>
  {:else if paid}
    <p class="note">Your plan, cloud agents and project sync.</p>
  {:else}
    <p class="note">Optional: agents keep working in the cloud while you're away, and your work opens on another device.</p>
  {/if}
  {#if paid && status !== null}
    <AccountUsage usage={status.usage} limits={status.limits} />
  {/if}
  <div class="actions">
    <a class="primary" href={BILLING_PATH}>{paid || due ? "Manage plan and billing" : "See plans"}</a>
    <button class="quiet" disabled={signingOut} onclick={() => void signOut()}>{signingOut ? "Signing out…" : "Sign out"}</button>
  </div>
  {#if signOutFailed}<p class="note warn" role="alert">Signing out didn't finish. Try again in a moment.</p>{/if}
  <p class="hint">Your devices, other sign-ins and Sign out everywhere are on the same page as billing.</p>
</div>

<style>
  .account { margin: 10px 14px 18px; padding: 18px; border: 1px solid var(--edge); border-radius: 8px; background: color-mix(in srgb, var(--fg) 2%, transparent); color: var(--fg); }
  .identity { display: grid; grid-template-columns: 30px minmax(0, 1fr); align-items: center; gap: 12px; }
  .copy { display: grid; gap: 4px; min-width: 0; }
  .title { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .title strong { font-size: var(--text-md); font-weight: 550; }
  .email { color: var(--muted); font-size: var(--text-sm); overflow-wrap: anywhere; }
  .note { margin: 14px 0 0; color: var(--muted); font-size: var(--text-sm); line-height: 1.6; }
  .note.warn { color: var(--warn); }
  .actions { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; margin-top: 18px; }
  .primary { display: inline-flex; align-items: center; padding: 8px 13px; border-radius: 6px; background: var(--fg); color: var(--bg); font-size: var(--text-sm); text-decoration: none; }
  .primary:hover { opacity: .9; }
  .quiet { padding: 8px 11px; border: 1px solid var(--edge); border-radius: 6px; background: transparent; color: var(--fg); font: inherit; font-size: var(--text-sm); cursor: pointer; }
  .quiet:hover { background: var(--row-hover); }
  .quiet:disabled { opacity: .5; cursor: default; }
  .primary:focus-visible, .quiet:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 2px; }
  .hint { margin: 14px 0 0; color: var(--muted); font-size: var(--text-xs); line-height: 1.6; }
  @media (pointer: coarse) { .primary, .quiet { min-height: 40px; box-sizing: border-box; } }
</style>

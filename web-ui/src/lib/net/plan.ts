import { derived, readable } from "svelte/store";
import { getHostLabel } from "./api";
import { asyncDisposer } from "../shared/asyncDisposer";
import { isAccountHome, isBrowserGateway, workbenchPath } from "./base";
import { isNativeShell, onProChanged, proStatus, type ProStatus } from "./native";
import { accountFailure, grantedPlan, paymentDue } from "../pro/status";

export type PaidPlan = "pro" | "max" | null;

/** `unavailable` = this window can never offer Pro (a build without an account
 * endpoint, or an ordinary browser); it is static for the process. */
export type AccountPlan = "loading" | "unknown" | "unavailable" | "free" | "pro" | "max";

function knownPlan(value: unknown): AccountPlan {
  if (value === "pro" || value === "max") return value;
  return value === "none" ? "free" : "unknown";
}

function nativePlan(status: ProStatus & { initializing?: boolean }): AccountPlan {
  // Checked first: an endpoint-less build is never initializing, and its
  // answer must not read as a neutral "unknown" that still shows Pro chrome.
  if (!status.available) return "unavailable";
  if (status.initializing || status.sign_in) return "loading";
  // A connection warning is informational; only a real failure is uncertain.
  if (accountFailure(status) !== null) return "unknown";
  if (!status.signed_in) return "free";
  // An ended plan is not a paid badge even while the account still names it.
  const plan = knownPlan(grantedPlan(status));
  // Overdue payment on an account without an active plan is neither a paid
  // badge nor a "Get Pro" offer; the Pro page explains it.
  return plan === "free" && paymentDue(status) ? "unknown" : plan;
}

interface AccountState {
  plan: AccountPlan;
  /** Whether Pro is offered at all: null until the first answer. */
  offered: boolean | null;
  /** This computer's account answered that it is signed out. False while
   *  unknown, and always in a browser view (its account gateway serves only
   *  a signed-in account). */
  signedOut: boolean;
}

/** Signed out for certain: available, settled (not starting up or mid
 *  sign-in) and not signed in. */
function nativeSignedOut(status: ProStatus & { initializing?: boolean }): boolean {
  return status.available && !status.initializing && !status.sign_in && !status.signed_in;
}

/** Account branding only; transport availability never implies entitlement.
 * Each window owns one subscription lifecycle and keeps no account data on disk.
 */
const account = readable<AccountState>({ plan: "loading", offered: null, signedOut: false }, (setState) => {
  // Availability is a build property, so it survives every later refresh
  // (including the ones that clear the plan back to "loading").
  let offered: boolean | null = null;
  const set = (plan: AccountPlan, signedOut = false): void => setState({ plan, offered, signedOut });
  set("loading");
  if (typeof document === "undefined") return;
  const native = isNativeShell();
  // The account's own Home (`/`) answers the same HEAD as a project view's index.
  const gateway = !native && (isBrowserGateway() || isAccountHome());
  if (!native && !gateway) { offered = false; set("unavailable"); return; }
  // A native window showing another host's daemon (an SSH remote, another
  // computer, the cloud) has no account bridge: its `pro_*` calls are refused.
  if (native && getHostLabel() !== "local") { offered = false; set("unavailable"); return; }
  // An account gateway exists only for Pro; there is nothing to wait for.
  if (gateway) { offered = true; set("loading"); }

  let alive = true;
  let generation = 0;
  let request: AbortController | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;

  function invalidate(clear = true): void {
    generation += 1;
    request?.abort();
    request = null;
    if (timer !== null) clearTimeout(timer);
    timer = null;
    if (clear) set("loading");
  }

  async function refresh(clear = true): Promise<void> {
    if (!alive) return;
    invalidate(clear);
    if (document.visibilityState !== "visible") return;
    const revision = generation;
    const controller = new AbortController();
    request = controller;
    let cancel!: () => void;
    const cancelled = new Promise<never>((_, reject) => {
      cancel = () => reject(new Error("Account branding request ended"));
      controller.signal.addEventListener("abort", cancel, { once: true });
    });
    const deadline = setTimeout(() => controller.abort(), 10_000);
    try {
      const lookup: Promise<{ plan: AccountPlan; available: boolean; signedOut?: boolean }> = native
        ? proStatus().then((status) => ({ plan: nativePlan(status), available: status.available, signedOut: nativeSignedOut(status) }))
        : fetch(workbenchPath(), {
          method: "HEAD",
          credentials: "same-origin",
          cache: "no-store",
          redirect: "error",
          signal: controller.signal,
        }).then((response) => ({
          plan: response.ok ? knownPlan(response.headers.get("X-Chimaera-Plan")) : "unknown" as const,
          available: true,
        }));
      const result = await Promise.race([lookup, cancelled]);
      if (alive && revision === generation) {
        offered = result.available;
        set(result.plan, result.signedOut === true);
      }
    } catch {
      if (alive && revision === generation) set("unknown");
    } finally {
      clearTimeout(deadline);
      controller.signal.removeEventListener("abort", cancel);
      if (alive && revision === generation) {
        request = null;
        if (gateway && document.visibilityState === "visible") {
          // A routine successful refresh must not make cosmetic branding blink.
          timer = setTimeout(() => { void refresh(false); }, 60_000);
        }
      }
    }
  }

  // Keep confirmed branding mounted while a background read is pending.
  // Fresh sign-out/error responses still clear it; this store grants no access.
  const visibility = (): void => { void refresh(false); };
  document.addEventListener("visibilitychange", visibility);
  // Listen before the first read so a concurrent sign-out cannot be missed.
  // asyncDisposer also handles the last subscriber leaving during registration.
  const dispose = native
    ? asyncDisposer(onProChanged(() => { void refresh(false); }).then(
      (unlisten) => { void refresh(); return unlisten; },
      () => { void refresh(); return () => {}; },
    ))
    : () => {};
  if (gateway) void refresh();

  return () => {
    alive = false;
    invalidate();
    dispose();
    document.removeEventListener("visibilitychange", visibility);
  };
});

export const accountPlan = derived(account, (state): AccountPlan => state.plan);

/** This computer's account is signed out: a conversation the cloud still
 *  holds stays there until the person signs in again. */
export const accountSignedOut = derived(account, (state): boolean => state.signedOut);

/** Gates every Pro entry point (Home, Settings). `null` while the first answer
 * is pending, so an endpoint-less build never flashes Pro chrome; once known it
 * holds through later refreshes. */
export const proOffered = derived(account, (state): boolean | null => state.offered);

/** Existing badge consumers share the account read; unknown is never a paid badge. */
export const paidPlan = derived(accountPlan, (plan): PaidPlan =>
  plan === "pro" || plan === "max" ? plan : null,
);

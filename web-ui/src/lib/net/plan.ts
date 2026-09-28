import { derived, readable } from "svelte/store";
import { asyncDisposer } from "../shared/asyncDisposer";
import { isBrowserGateway, workbenchPath } from "./base";
import { isNativeShell, onProChanged, proStatus, type ProStatus } from "./native";

export type PaidPlan = "pro" | "max" | null;

export type AccountPlan = "loading" | "unknown" | "free" | "pro" | "max";

function knownPlan(value: unknown): AccountPlan {
  if (value === "pro" || value === "max") return value;
  return value === "none" ? "free" : "unknown";
}

function nativePlan(status: ProStatus & { initializing?: boolean }): AccountPlan {
  if (status.initializing || status.sign_in) return "loading";
  if (!status.available || status.error !== null) return "unknown";
  return status.signed_in ? knownPlan(status.plan) : "free";
}

/** Account branding only; transport availability never implies entitlement.
 * Each window owns one subscription lifecycle and keeps no account data on disk.
 */
export const accountPlan = readable<AccountPlan>("loading", (set) => {
  set("loading");
  if (typeof document === "undefined") return;
  const native = isNativeShell();
  const gateway = !native && isBrowserGateway();
  if (!native && !gateway) { set("unknown"); return; }

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
      const lookup = native
        ? proStatus().then(nativePlan)
        : fetch(workbenchPath(), {
          method: "HEAD",
          credentials: "same-origin",
          cache: "no-store",
          redirect: "error",
          signal: controller.signal,
        }).then((response) => response.ok ? knownPlan(response.headers.get("X-Chimaera-Plan")) : "unknown" as const);
      const plan = await Promise.race([lookup, cancelled]);
      if (alive && revision === generation) set(plan);
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

/** Existing badge consumers share the account read; unknown is never a paid badge. */
export const paidPlan = derived(accountPlan, (plan): PaidPlan =>
  plan === "pro" || plan === "max" ? plan : null,
);

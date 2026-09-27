import { readable } from "svelte/store";
import { asyncDisposer } from "../shared/asyncDisposer";
import { isBrowserGateway, workbenchPath } from "./base";
import { isNativeShell, onProChanged, proStatus } from "./native";

export type PaidPlan = "pro" | "max" | null;

function paid(value: unknown): PaidPlan {
  return value === "pro" || value === "max" ? value : null;
}

/** Account branding only; transport availability never implies entitlement.
 * Each window owns one subscription lifecycle and keeps no account data on disk.
 */
export const paidPlan = readable<PaidPlan>(null, (set) => {
  set(null);
  if (typeof document === "undefined") return;
  const native = isNativeShell();
  const gateway = !native && isBrowserGateway();
  if (!native && !gateway) return;

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
    if (clear) set(null);
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
        ? proStatus().then((status) => status.signed_in ? paid(status.plan) : null)
        : fetch(workbenchPath(), {
          method: "HEAD",
          credentials: "same-origin",
          cache: "no-store",
          redirect: "error",
          signal: controller.signal,
        }).then((response) => response.ok ? paid(response.headers.get("X-Chimaera-Plan")) : null);
      const plan = await Promise.race([lookup, cancelled]);
      if (alive && revision === generation) set(plan);
    } catch {
      if (alive && revision === generation) set(null);
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

  const visibility = (): void => { void refresh(); };
  document.addEventListener("visibilitychange", visibility);
  // Listen before the first read so a concurrent sign-out cannot be missed.
  // asyncDisposer also handles the last subscriber leaving during registration.
  const dispose = native
    ? asyncDisposer(onProChanged(() => { void refresh(); }).then(
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

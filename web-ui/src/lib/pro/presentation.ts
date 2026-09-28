export type PaidPlan = "pro" | "max";
export type BillingInterval = "month" | "year";
export interface PlanChoice { plan: PaidPlan; interval: BillingInterval }
export interface PurchaseIntent extends PlanChoice { stage: "sign_in" | "checkout"; created: number }

export function paid(plan: unknown): plan is PaidPlan { return plan === "pro" || plan === "max"; }
export function readIntent(value: string | null, now = Date.now()): PurchaseIntent | null {
  try {
    const v = JSON.parse(value ?? "null");
    if (!v || !paid(v.plan) || !["month", "year"].includes(v.interval) || !["sign_in", "checkout"].includes(v.stage)
      || !Number.isFinite(v.created) || v.created > now || now - v.created > 24 * 60 * 60 * 1000) return null;
    return { plan: v.plan, interval: v.interval, stage: v.stage, created: v.created };
  } catch { return null; }
}
export function cloudCopy(state: string, reason: string | null): { title: string; detail: string } {
  if (reason === "provisioning_disabled") return { title: "Cloud service isn't available yet", detail: "Cloud preparation is not enabled for this service. Your local work continues as usual." };
  if (reason === "beta_invite_required") return { title: "Cloud access is by invitation", detail: "This preview needs an invitation before cloud preparation can begin." };
  if (reason === "hours_exhausted") return { title: "Cloud hours used for this month", detail: "Work continues on your computer. Your cloud allowance resets next month." };
  if (reason === "storage_exhausted") return { title: "Cloud storage is full", detail: "Your local projects remain available. Review project mirrors or your plan to make room." };
  if (reason === "spend_limit_reached") return { title: "Cloud use is paused", detail: "Your account's cloud spending limit has been reached. Your local work is unaffected." };
  switch (state) {
    case "ready": return { title: "Your cloud is ready", detail: "Your computer stays the first place work runs. Cloud handoff happens automatically when it's needed." };
    case "sleeping": return { title: "Your cloud is resting", detail: "It wakes automatically when work needs it. No cloud hours are used while it sleeps." };
    case "preparing": return { title: "Preparing your cloud", detail: "Chimaera is getting your cloud ready. You can keep working here." };
    case "no_plan": return { title: "Cloud is included with Pro", detail: "Choose a plan when you're ready. Local work and ordinary SSH stay available." };
    case "limited": return { title: "Cloud use is paused", detail: "Your account has reached a cloud limit. Your local work is unaffected." };
    default: return { title: "Cloud is temporarily unavailable", detail: "We couldn't confirm cloud readiness. Try checking again in a moment; your local work is available." };
  }
}
export function friendlyError(reason: unknown, fallback: string): string {
  const text = reason instanceof Error ? reason.message : String(reason);
  if (/expired|sign.in required|sign in first|authorization revoked/i.test(text)) return "Your sign-in has expired. Sign in again to continue.";
  if (/Could not open.*browser/i.test(text)) return "Your browser couldn't open. Please try again.";
  if (/finishing/i.test(text)) return "Sign-in is finishing. Please wait a moment.";
  if (/409|use_billing_portal/.test(text)) return "Your account already has a plan. Refresh your account, then choose Manage billing.";
  return fallback;
}

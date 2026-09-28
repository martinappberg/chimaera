import type { CloudProvisioningStatus, MirrorStatus, ProAuthScreenHint } from "../net/native";

export type PaidPlan = "pro" | "max";
export type BillingInterval = "month" | "year";
export interface PlanChoice { plan: PaidPlan; interval: BillingInterval }
export interface PurchaseIntent extends PlanChoice { stage: "sign_in"; created: number; screenHint?: ProAuthScreenHint }

export function paid(plan: unknown): plan is PaidPlan { return plan === "pro" || plan === "max"; }
export function readIntent(value: string | null, now = Date.now()): PurchaseIntent | null {
  try {
    const v = JSON.parse(value ?? "null");
    if (!v || !paid(v.plan) || !["month", "year"].includes(v.interval) || v.stage !== "sign_in"
      || (v.screenHint !== undefined && v.screenHint !== "sign-in" && v.screenHint !== "sign-up")
      || !Number.isFinite(v.created) || v.created > now || now - v.created > 24 * 60 * 60 * 1000) return null;
    return { plan: v.plan, interval: v.interval, stage: v.stage, created: v.created, ...(v.screenHint ? { screenHint: v.screenHint } : {}) };
  } catch { return null; }
}
export function cloudCopy(state: string, reason: string | null, phase?: CloudProvisioningStatus["phase"]): { title: string; detail: string } {
  if (reason === "provisioning_disabled") return { title: "Cloud preparation is paused", detail: "This service is not currently preparing cloud machines. Your local projects remain available." };
  if (reason === "beta_invite_required") return { title: "Cloud access is by invitation", detail: "This preview needs an invitation before cloud preparation can begin." };
  if (reason === "hours_exhausted") return { title: "Cloud hours used for this month", detail: "Work continues on your computer. Your cloud allowance resets next month." };
  if (reason === "storage_exhausted") return { title: "Cloud storage is full", detail: "Your local projects remain available. Review project mirrors or your plan to make room." };
  if (reason === "spend_limit_reached") return { title: "Cloud use is paused", detail: "Your account's cloud spending limit has been reached. Your local work is unaffected." };
  switch (state) {
    case "ready": return { title: "Your cloud is ready", detail: "Your computer stays the first place work runs. Cloud handoff happens automatically when it's needed." };
    case "sleeping": return { title: "Your cloud is resting", detail: "It wakes automatically when work needs it." };
    case "preparing":
      if (phase === "keeper") return { title: "Preparing your cloud connection", detail: "Chimaera is setting up the private connection between your account and your machines." };
      if (phase === "worker") return { title: "Starting your cloud machine", detail: "Your account connection is ready. Chimaera is preparing the machine where your agents will run." };
      if (phase === "connecting") return { title: "Connecting your cloud workbench", detail: "Your machine has started. Chimaera is waiting for it to confirm that it is ready." };
      return { title: "Preparing your cloud", detail: "Chimaera is getting your cloud ready. You can keep working here." };
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


/** Faster startup checks are finite, sequential in CloudSetup, and never wake compute. */
export function cloudPollDelay(preparing: boolean, elapsedMs: number): number {
  return preparing && elapsedMs < 5 * 60_000 ? 5000 : 30_000;
}

export interface CloudProjectStatus {
  title: string;
  detail: string;
  state: "active" | "attention" | "quiet";
}

/** A recorded copy is historical evidence, never a claim that a sync is running. */
export function cloudProjectStatus(projects: MirrorStatus | null, workspaceId?: string): CloudProjectStatus | null {
  if (!projects) return null;
  const rows = projects.workspaces.filter(p => !p.never_mirror && (!workspaceId || p.workspace_id === workspaceId));
  if (!rows.length) return null;
  if (!projects.configured) return { title: "Project connection pending", detail: "Existing copies are retained. Mirroring has not connected on this machine yet.", state: "attention" };
  if (rows.some(p => p.ownership?.state === "hydrating")) return { title: "Restoring your project", detail: "Files and conversations are being restored on this machine.", state: "active" };
  if (rows.some(p => p.ownership?.state === "transferring")) return { title: "Moving your project", detail: "Chimaera is saving the project and handing its work to the other machine.", state: "active" };
  const settingUp = rows.filter(p => p.ownership?.state === "setting_up");
  if (settingUp.length) return { title: "Project setup needs attention", detail: settingUp.some(p => p.blocked_providers?.length) ? "This project is waiting for an agent connection below." : "The project is paused until its cloud setup succeeds.", state: "attention" };
  if (rows.some(p => p.ownership?.state === "awaiting_verification")) return { title: "Checking project ownership", detail: "Chimaera is confirming where these projects can run.", state: "active" };
  if (rows.some(p => p.ownership?.state === "privacy_disabled")) return { title: "Project copying is disabled", detail: "Review this project's privacy setting in Project mirrors.", state: "attention" };
  if (rows.some(p => p.mirror?.error || p.privacy_pending)) return { title: "A project needs attention", detail: "Open Project mirrors below for details. Existing copies are retained.", state: "attention" };
  const copied = rows.filter(p => p.mirror?.last_mirrored_at != null).length;
  if (copied) return { title: "Cloud copies saved", detail: `${copied} ${copied === 1 ? "project has" : "projects have"} a completed cloud copy.${copied < rows.length ? " Other projects have not reported a completed copy yet." : ""}`, state: "quiet" };
  return { title: "Waiting for a project copy", detail: "Chimaera is watching your registered projects. No completed cloud copy has been reported yet.", state: "quiet" };
}

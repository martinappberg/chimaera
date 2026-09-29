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
export function cloudCopy(state: string, reason: string | null, _phase?: CloudProvisioningStatus["phase"]): { title: string; detail: string } {
  if (reason === "provisioning_disabled") return { title: "Cloud access is temporarily unavailable", detail: "Cloud work isn’t available yet. Work on this computer continues as usual." };
  if (reason === "beta_invite_required") return { title: "Cloud access is by invitation", detail: "This preview needs an invitation before you can use cloud work." };
  if (reason === "hours_exhausted") return { title: "Cloud allowance used for this month", detail: "Work continues on your computer. Your cloud allowance resets next month." };
  if (reason === "storage_exhausted") return { title: "Cloud copying needs more room", detail: "Your local projects remain available. The latest cloud copy couldn’t fit within your allowance. Review Usage and plan details." };
  if (reason === "spend_limit_reached") return { title: "Cloud use is paused", detail: "Your account's cloud spending limit has been reached. Your local work is unaffected." };
  switch (state) {
    // Idle compute is an implementation detail, not a different level of access.
    case "ready": case "sleeping": return { title: "Available when you need it", detail: "Connected agents can keep working while you’re away. Your projects and conversations come with you across devices." };
    case "preparing": return { title: "Getting things ready", detail: "Chimaera is preparing access to your projects and agent connections. You can keep working here." };
    case "no_plan": return { title: "Cloud is included with Pro", detail: "Choose a plan when you're ready. Local work and ordinary SSH stay available." };
    case "limited": return { title: "Cloud use is paused", detail: "Your account has reached a cloud limit. Your local work is unaffected." };
    default: return { title: "Cloud is temporarily unavailable", detail: "We couldn’t check cloud access. Try again in a moment. Your local work is available." };
  }
}
export function friendlyError(reason: unknown, fallback: string): string {
  const text = reason instanceof Error ? reason.message : String(reason);
  if (text === "service_unsupported") return "Your Pro service doesn’t support this version of Chimaera yet. Cloud features resume on their own once it does; your local work, connections and account are unaffected.";
  if (text === "account_restore_locked") return "Chimaera couldn’t read your saved sign-in. Unlock your computer’s credential store, then choose Check again.";
  if (text === "account_restore_unavailable") return "Chimaera couldn’t confirm your saved sign-in yet. Check your connection, then choose Check again. Your local work remains available.";
  if (text === "account_credentials_unsaved") return "You’re signed in, but Chimaera couldn’t save your session securely. Check your computer’s credential store and free disk space, then choose Check again. You may need to sign in again after restarting the app.";
  if (/expired|sign.in required|sign in first|authorization revoked/i.test(text)) return "Your sign-in has expired. Sign in again to continue.";
  if (/Could not open.*browser/i.test(text)) return "Your browser couldn't open. Please try again.";
  if (/finishing/i.test(text)) return "Sign-in is finishing. Please wait a moment.";
  if (alreadySubscribed(reason)) return "Your account already has a plan.";
  return fallback;
}

/** Checkout refused because the account already has a plan; the page re-reads
 * the account itself instead of asking the user to refresh. */
export function alreadySubscribed(reason: unknown): boolean {
  const text = reason instanceof Error ? reason.message : String(reason);
  return /409|use_billing_portal/.test(text);
}

/** Service diagnostics are neither UI copy nor a promise of automatic recovery. */
/** Newer daemons send a stable `error_code` beside the text; prefer it. */
const COPY_ERROR_CODES: Record<string, string> = {
  credential_in_history: "A file in this project’s Git history looks like a credential, so the project isn’t copied. Remove it from the history to turn copying on.",
  git_too_old: "Git on this computer is too old for cloud copies. Update Git and the copies resume on their own.",
  conversation_not_saved: "A conversation couldn’t be included in the latest project copy. Your work remains on this device.",
  root_setup_required: "Project setup didn’t finish. Your saved work is intact, but this project can’t continue in the cloud yet.",
  cache_recovery_needed: "The cloud copy needs a repair. Chimaera rebuilds it from the last saved copy on its own.",
  previous_processes_running: "Waiting for this project’s earlier agents to finish before it continues.",
  ownership_changed: "This project moved. Chimaera is catching up with where it runs now.",
  ownership_unverified: "Checking where this project is running…",
  account_changed: "Your account changed. Open the project again.",
  pending: "Copying…",
  checkpoint_pending: "Saving the latest copy…",
};
export function projectCopyError(reason: string, code?: string | null): string {
  if (code && COPY_ERROR_CODES[code]) return COPY_ERROR_CODES[code];
  switch (reason) {
    case "workspace exceeds mirror storage quota":
    case "workspace and conversations exceed mirror storage quota":
      return "This project exceeds your current cloud allowance. Your local work is still available. Review Usage and plan details for your account’s allowance.";
    case "session archive exceeds mirror file limit":
      return "A conversation is too large to include in this cloud copy. It remains available on this device.";
    case "Claude transcript is unavailable": case "Codex rollout is unavailable": case "native conversation is not ready to export":
      return "A conversation couldn’t be included in the latest project copy. Your work remains on this device.";
    case "project setup needs attention in its terminal":
      return "Project setup didn’t finish. Your saved work is intact, but this project can’t continue in the cloud yet.";
    default:
      return "The latest project copy is incomplete. Your local work is still available.";
  }
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
  if (!projects.configured) return { title: "Getting your projects ready", detail: "Copies start automatically.", state: "active" };
  if (rows.some(p => p.ownership?.state === "hydrating")) return { title: "Restoring your project", detail: "Your files and conversations are being restored here.", state: "active" };
  if (rows.some(p => p.ownership?.state === "transferring")) return { title: "Keeping your work with you", detail: "Chimaera is saving your files and conversation so work can continue.", state: "active" };
  const settingUp = rows.filter(p => p.ownership?.state === "setting_up");
  if (settingUp.length) return { title: "Project setup needs attention", detail: settingUp.some(p => p.blocked_providers?.length) ? "This project is paused. Its agent connection and continuation status are shown below." : "The project is paused until its setup succeeds.", state: "attention" };
  if (rows.some(p => p.ownership?.state === "awaiting_verification")) return { title: "Checking your project", detail: "Chimaera is checking where you left off before continuing.", state: "active" };
  if (rows.some(p => p.ownership?.state === "privacy_disabled")) return { title: "Project copying is disabled", detail: "Review this project's setting in Projects and privacy.", state: "attention" };
  if (rows.some(p => p.mirror?.error || p.privacy_pending)) return { title: "A project needs attention", detail: "Open Projects and privacy below for details. Existing copies are retained.", state: "attention" };
  const copied = rows.filter(p => p.mirror?.last_mirrored_at != null || (typeof p.checkpoint_id === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(p.checkpoint_id))).length;
  if (copied) return { title: "Cloud copies saved", detail: `${copied} ${copied === 1 ? "project has" : "projects have"} a completed cloud copy.`, state: "quiet" };
  return { title: "Project copy status", detail: "Your latest copy status isn’t available on this device yet. Existing copies are retained.", state: "quiet" };
}

export function recoverableAccountRestore(error: unknown): boolean {
  return error === "account_restore_locked" || error === "account_restore_unavailable";
}

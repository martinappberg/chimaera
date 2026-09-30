import type { CloudProvisioningStatus, MirrorStatus, MirrorWorkspace, ProAuthScreenHint } from "../net/native";
import { formatFullTimestamp } from "../shared/time";

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
/** `agents` is whether an agent was connected in the cloud at the last
 * catalog read (remembered by the app, never probed): unknown claims nothing,
 * and none connected reads as the next step, not as an error. `readyOnce` is
 * whether this account's cloud has been ready before (`cloudReadyOnce`): only
 * its very first setup reads as setup; any later `preparing` (a service
 * update, say) is the same calm availability as ready and idle. */
export function cloudCopy(state: string, reason: string | null, _phase?: CloudProvisioningStatus["phase"], agents: boolean | null = null, readyOnce = false): { title: string; detail: string } {
  if (reason === "provisioning_disabled") return { title: "Cloud access is temporarily unavailable", detail: "Cloud work isn’t available yet. Work on this computer continues as usual." };
  if (reason === "beta_invite_required") return { title: "Cloud access is by invitation", detail: "This preview needs an invitation before you can use cloud work." };
  if (reason === "hours_exhausted") return { title: "Cloud allowance used for this month", detail: "Work continues on your computer. Your cloud allowance resets next month." };
  if (reason === "storage_exhausted") return { title: "Cloud copying needs more room", detail: "Your local projects remain available. The latest cloud copy couldn’t fit within your allowance. Review Usage and plan details." };
  if (reason === "spend_limit_reached") return { title: "Cloud use is paused", detail: "Your account’s cloud spending limit has been reached. Your local work is unaffected." };
  if (state === "preparing" && !readyOnce) return { title: "Getting things ready", detail: "Setting up your cloud. This usually takes a couple of minutes." };
  switch (state) {
    // Idle compute, and a cloud being updated, are implementation details,
    // not a different level of access. The connections section below shows
    // the agents, so this stays about availability.
    case "ready": case "sleeping": case "preparing":
      return { title: "Available when you need it", detail: agents === false
        ? "Connect an agent below to start cloud work."
        : agents
          ? "Your connected agents can keep working while you’re away. Your projects and conversations come with you."
          : "Agents you connect can keep working while you’re away. Your projects and conversations come with you." };
    case "no_plan": return { title: "Cloud is included with Pro", detail: "Choose a plan when you’re ready. Local work and ordinary SSH stay available." };
    case "limited": return { title: "Cloud use is paused", detail: "Your account has reached a cloud limit. Your local work is unaffected." };
    // A state this app does not know (a newer service) is not an outage.
    case "unknown": return { title: "Checking cloud access", detail: "Your local work is available while Chimaera checks." };
    default: return { title: "Cloud is temporarily unavailable", detail: "We couldn’t check cloud access. Chimaera keeps trying; your local work is available." };
  }
}
/** Whether this account's cloud has been ready before, from the app's memory
 * (`cloud_ready_once`). An older shell without it: a remembered catalog fact
 * means a catalog was read, which needs a ready cloud. */
export function cloudReadyOnce(status: Pick<CloudProvisioningStatus, "cloud_ready_once" | "agents_connected" | "remembered_providers"> | null): boolean {
  if (status === null) return false;
  return status.cloud_ready_once ?? (status.agents_connected != null || (status.remembered_providers?.length ?? 0) > 0);
}
/** The account's `return_window_ended` code (403 once a plan has ended and the
 * time to bring its cloud work home has passed). Quiet: a plain sentence, no
 * alarm and no retry. */
export const RETURN_WINDOW_ENDED_COPY = "The time to bring this work home has passed. Contact support.";

/** The one quiet line for an ended plan inside its return window
 * (`status.ts` `returningUntil`): when its cloud work can still be brought
 * home. Once that time has passed it reads as the code does. Null when there
 * is no window. */
export function returningLine(until: string | null, now = Date.now(), locale?: string): string | null {
  const ends = until === null ? Number.NaN : Date.parse(until);
  if (!Number.isFinite(ends)) return null;
  if (ends <= now) return RETURN_WINDOW_ENDED_COPY;
  return `Your plan has ended. Bring your work home from the cloud by ${formatFullTimestamp(ends, locale)}.`;
}
/** The fixed code the native shell and the browser transport give a cloud
 * request answered by a cloud that is asleep or still starting (503
 * `worker_asleep`/`worker_unavailable`, or a reply marked sleeping). That is a
 * state, never an error, and never words on a passive path: the connections
 * section keeps its remembered rows and the page keeps checking. Only an
 * action the user took says it is still connecting. */
export const CLOUD_ASLEEP = "cloud_asleep";
export function cloudAsleep(reason: unknown): boolean {
  return (reason instanceof Error ? reason.message : String(reason)) === CLOUD_ASLEEP;
}
/** How long an action the user took (pressing Connect or Disconnect) keeps
 * asking while the cloud comes up behind it before the action reports its
 * usual failure. */
export const WAKE_BOUND_MS = 120_000;
/** A live answer this late may show one muted "Checking…" beside the
 * section title; sooner, the remembered rows simply stay. */
export const CHECKING_AFTER_MS = 5000;
export function friendlyError(reason: unknown, fallback: string): string {
  const text = reason instanceof Error ? reason.message : String(reason);
  if (text === "return_window_ended") return RETURN_WINDOW_ENDED_COPY;
  // An action the user took found the cloud still coming up.
  if (text === CLOUD_ASLEEP) return "Still connecting to your cloud. Try again in a moment.";
  if (text === "service_unsupported") return "Cloud work is off for now because this version of Chimaera and your account don’t match. Installing an update, if one is offered, turns it back on; otherwise it resumes on its own. Work on this computer isn’t affected.";
  // Sign-out finished here; the app removes the saved sign-in by itself.
  if (text === "sign_out_pending") return "You’re signed out on this computer. The saved sign-in is cleared automatically next time you’re online.";
  const note = signInNoteCopy(text);
  if (note !== null) return note;
  if (text === "account_restore_locked") return "Chimaera couldn’t read your saved sign-in. Unlock your computer’s credential store, then choose Check again.";
  if (text === "account_restore_unavailable") return "Chimaera couldn’t confirm your saved sign-in yet. Check your connection, then choose Check again. Your local work remains available.";
  if (text === "account_credentials_unsaved") return "You’re signed in, but Chimaera couldn’t save your session securely. Check your computer’s credential store and free disk space, then choose Check again. You may need to sign in again after restarting the app.";
  if (/expired|sign.in required|sign in first|authorization revoked/i.test(text)) return "Your sign-in has expired. Sign in again to continue.";
  if (/Could not open.*browser/i.test(text)) return "Your browser couldn’t open. Please try again.";
  if (/finishing/i.test(text)) return "Sign-in is finishing. Please wait a moment.";
  if (alreadySubscribed(reason)) return "Your account already has a plan.";
  return fallback;
}

/** One quiet line for a browser sign-in that ended without signing in (the
 * native `sign_in_*` / `browser_unavailable` codes); null for anything else.
 * The person is simply signed out, so the page keeps its plans and its
 * Sign in button, which the browser's failure page names too. */
export function signInNoteCopy(code: string | null | undefined): string | null {
  switch (code) {
    case "sign_in_timed_out": return "Sign-in timed out. Start again when you’re ready.";
    case "sign_in_incomplete": return "Sign-in didn’t finish. Start again when you’re ready.";
    case "browser_unavailable": return "Your browser couldn’t open for sign-in. Start again when you’re ready.";
    default: return null;
  }
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
  conversation_not_saved: "A conversation couldn’t be included in the latest project copy. Your work remains on this computer.",
  root_setup_required: "Project setup didn’t finish. Your saved work is intact, but this project can’t continue in the cloud yet.",
  // The saved setup command failed in the cloud; its output is kept in the
  // project's setup log there.
  cloud_setup_failed: "Project setup in the cloud didn’t finish. Your files are safe; the project’s setup log in the cloud has the details.",
  cache_recovery_needed: "The cloud copy needs a repair. Chimaera rebuilds it from the last saved copy on its own.",
  previous_processes_running: "Waiting for this project’s earlier agents to finish before it continues.",
  ownership_changed: "This project moved. Chimaera is catching up with where it runs now.",
  ownership_unverified: "Checking where this project is running…",
  account_changed: "Your account changed. Open the project again.",
  return_window_ended: RETURN_WINDOW_ENDED_COPY,
  pending: "Copying…",
  checkpoint_pending: "Saving the latest copy…",
};
/** Codes for work still under way, not a problem: the next copy or ownership
 * check clears them without the user, so they read quietly and never make a
 * project "need attention". */
const PROGRESS_CODES: ReadonlySet<string> = new Set(["pending", "checkpoint_pending", "ownership_unverified"]);
/** Codes that are neither progress nor a problem to fix here: one quiet
 * sentence, with no "needs attention" state and no warning colour. */
const NOTE_CODES: ReadonlySet<string> = new Set(["return_window_ended"]);

/** What a project's recorded copy error means: nothing, progress, a quiet
 * note, or a problem. */
export function copyIssue(mirror: MirrorWorkspace["mirror"] | undefined): "none" | "progress" | "note" | "problem" {
  if (!mirror?.error) return "none";
  if (mirror.error_code == null) return "problem";
  if (PROGRESS_CODES.has(mirror.error_code)) return "progress";
  return NOTE_CODES.has(mirror.error_code) ? "note" : "problem";
}

export function projectCopyError(reason: string, code?: string | null): string {
  if (code && COPY_ERROR_CODES[code]) return COPY_ERROR_CODES[code];
  switch (reason) {
    case "workspace exceeds mirror storage quota":
    case "workspace and conversations exceed mirror storage quota":
      return "This project exceeds your current cloud allowance. Your local work is still available. Review Usage and plan details for your account’s allowance.";
    case "session archive exceeds mirror file limit":
      return "A conversation is too large to include in this cloud copy. It remains available on this computer.";
    case "Claude transcript is unavailable": case "Codex rollout is unavailable": case "native conversation is not ready to export":
      return "A conversation couldn’t be included in the latest project copy. Your work remains on this computer.";
    case "project setup needs attention in its terminal":
      return "Project setup didn’t finish. Your saved work is intact, but this project can’t continue in the cloud yet.";
    default:
      return "The latest project copy is incomplete. Your local work is still available.";
  }
}


/** The line under Project copies while automatic copying is not set up on
 * this computer; null once it is. A lapsed account connection is renewed by
 * the app on its own, so it reads as quiet progress with nothing to press. */
export function projectCopiesSetupLine(projects: Pick<MirrorStatus, "configured" | "renewal_failed"> | null): string | null {
  if (projects === null || projects.configured) return null;
  return projects.renewal_failed === true ? "Reconnecting your account…" : "Automatic project copying is getting ready. You can keep working here.";
}

/** One quiet line for an informational connection state (`status.ts`
 * `connectionWarningCode`). The app recovers each by itself; none offers a
 * button, and none is an account failure. */
export function connectionWarningCopy(code: string | null): string {
  switch (code) {
    case "connection_retrying": return "Reconnecting to the cloud… Work on this computer continues as usual.";
    case "account_unreachable": return "Your account can’t be reached right now. Work on this computer continues as usual.";
    // connection_preparing, and any code a newer app sends.
    default: return "Connecting to the cloud… Work on this computer continues as usual.";
  }
}

/** Reads in a row that must fail before the page reports a problem: one
 * miss is usually the cloud going idle in between, or the account service
 * being updated, never an outage. */
export const MISSES_REPORTED = 2;

/** After an account status read that failed: the first in a row keeps the
 * last confirmed status (the calm copy stays, nothing is reported, the page
 * checks again soon); the next one in a row is reported as a failed read. A
 * successful read starts the count again at zero. */
export function afterFailedRead(status: CloudProvisioningStatus | null, missesBefore: number): { status: CloudProvisioningStatus | null; misses: number; report: boolean } {
  const misses = missesBefore + 1;
  return misses < MISSES_REPORTED ? { status, misses, report: false } : { status: { state: "error", reason: null }, misses, report: true };
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

/** Whether a project's `setting_up` row is waiting on the person (an agent
 * connection, or setup that failed) rather than the normal phase while the
 * saved setup command runs (up to ten minutes in the cloud). */
function setupNeeds(workspace: MirrorWorkspace): "agents" | "failed" | null {
  if (workspace.blocked_providers?.length) return "agents";
  return workspace.mirror?.error_code === "cloud_setup_failed" ? "failed" : null;
}

/** A recorded copy is historical evidence, never a claim that a sync is running.
 * `where` is whose status this is: the laptop's own (setup there is a brief
 * check on return) or the cloud's own page (setup runs the saved command). */
export function cloudProjectStatus(projects: MirrorStatus | null, workspaceId?: string, where: "computer" | "cloud" = "computer"): CloudProjectStatus | null {
  if (!projects) return null;
  const rows = projects.workspaces.filter(p => !p.never_mirror && (!workspaceId || p.workspace_id === workspaceId));
  if (!rows.length) return null;
  if (!projects.configured && projects.renewal_failed === true) return { title: "Reconnecting your account…", detail: "Copies continue once it’s back. Work on this computer continues as usual.", state: "active" };
  if (!projects.configured) return { title: "Getting your projects ready", detail: "Copies start automatically.", state: "active" };
  if (rows.some(p => p.ownership?.state === "hydrating")) return { title: "Restoring your project", detail: "Your files and conversations are being restored here.", state: "active" };
  if (rows.some(p => p.ownership?.state === "transferring")) return { title: "Keeping your work with you", detail: "Chimaera is saving your files and conversation so work can continue.", state: "active" };
  const settingUp = rows.filter(p => p.ownership?.state === "setting_up");
  if (settingUp.some(p => setupNeeds(p) === "agents")) return { title: "Connect an agent to continue", detail: "This project waits for an agent connection. The details are below.", state: "attention" };
  if (settingUp.some(p => setupNeeds(p) === "failed")) return { title: "Project setup didn’t finish", detail: "Your files are safe. The project’s setup log in the cloud has the details.", state: "attention" };
  if (rows.some(p => p.ownership?.state === "awaiting_verification")) return { title: "Checking your project", detail: "Chimaera is checking where you left off before continuing.", state: "active" };
  if (rows.some(p => p.ownership?.state === "privacy_disabled")) return { title: "Project copying is off", detail: "Review this project’s setting in Projects and privacy.", state: "attention" };
  if (rows.some(p => copyIssue(p.mirror) === "problem")) return { title: "A project needs attention", detail: "Open Projects and privacy below for details. Existing copies are retained.", state: "attention" };
  // Normal setup is progress, so it never hides another project's problem.
  if (settingUp.length) return where === "cloud"
    ? { title: "Setting up the project in the cloud…", detail: "Its saved setup command is running. Work continues when it finishes.", state: "active" }
    : { title: "Getting your project ready…", detail: "Work continues here in a moment.", state: "active" };
  // Turning copies off is the system's job to finish; the row itself says so quietly.
  if (rows.some(p => p.privacy_pending)) return { title: "Turning off cloud copies", detail: "A project now stays on this computer. Chimaera confirms it with your account on its own.", state: "active" };
  const copied = rows.filter(p => p.mirror?.last_mirrored_at != null || (typeof p.checkpoint_id === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(p.checkpoint_id))).length;
  if (copied) return { title: "Cloud copies saved", detail: `${copied} ${copied === 1 ? "project has" : "projects have"} a completed cloud copy.`, state: "quiet" };
  return { title: "Project copy status", detail: "The latest copy status isn’t available here yet. Existing copies are kept.", state: "quiet" };
}

/** A project's one-line place: where it runs now, or the step it is in.
 * Only `setupNeeds` makes setup read as something to act on. */
export function projectPlace(workspace: MirrorWorkspace): string {
  if (workspace.never_mirror) return workspace.privacy_pending ? "Keeping this project on this computer…" : "Only on this computer";
  switch (workspace.ownership?.state) {
    case "local": return "This project is running on your computer right now";
    case "remote": return "This project is running in the cloud right now";
    case "transferring": return "Moving to the cloud…";
    case "privacy_disabled": return "Automatic copying is off";
    case "setting_up": {
      const needs = setupNeeds(workspace);
      return needs === "agents" ? "Waiting for an agent connection" : needs === "failed" ? "Project setup didn’t finish" : "Getting the project ready…";
    }
    case "hydrating": return "Restoring files and conversations…";
    case "awaiting_verification": return "Checking for recent changes…";
    default: return "Waiting for the first copy";
  }
}

export function recoverableAccountRestore(error: unknown): boolean {
  return error === "account_restore_locked" || error === "account_restore_unavailable";
}

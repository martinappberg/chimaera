import type { MirrorWorkspace } from "../net/native";
import { RETURN_WINDOW_ENDED_COPY } from "./presentation";

const ERRORS: Record<string, string> = {
  project_busy: "This project is updating its saved copy. Try Open again shortly.",
  project_copy_update_required: "Update Chimaera to open a local copy without moving execution.",
  project_checkpoint_pending: "The project has no published copy yet. Try again after its current computer saves it.",
  project_folder_not_empty: "That folder already contains files. Choose a new empty project folder so your existing work stays untouched.",
  project_folder_missing: "This project's local folder is unavailable. Restore or reconnect that folder, then try again.",
  project_folder_nested: "Choose a folder that isn't inside another project or Git repository.",
  account_changed: "Your account changed. Open the project again.",
  project_already_opening: "Another project action is finishing. Try again when it's done.",
  project_unavailable: "This project's saved copy isn't available right now. Try again shortly.",
  return_window_ended: RETURN_WINDOW_ENDED_COPY,
};
/** Fixed native codes only; daemon paths and diagnostics never become UI prose. */
export function projectCopyError(reason: unknown, action: "open" | "takeover" = "open"): string {
  const code = reason instanceof Error ? reason.message : String(reason);
  return Object.hasOwn(ERRORS, code) ? ERRORS[code] : action === "takeover"
    ? "Execution couldn't move here. Refresh the project status and try Take over again."
    : "This project couldn't open here. Its saved copy is intact. Please try again shortly.";
}
export interface StagingPresentation { text: string; paths: string[]; recovery: string | null }
/** Older checkpoints must never be described as an empty or synced index. */
export function stagingPresentation(value: unknown): StagingPresentation | null {
  if (typeof value !== "object" || value === null) return null;
  const row = value as { state?: unknown; paths?: unknown; total?: unknown; recovery?: unknown };
  if (row.state === "uncaptured") return { text: "This older saved copy has no Git staging snapshot. Your current staged changes are preserved.", paths: [], recovery: null };
  if (row.state === "synced") return { text: "Git staging copied.", paths: [], recovery: null };
  if (row.state !== "conflicts" || !Number.isSafeInteger(row.total) || (row.total as number) < 1) return null;
  const paths = Array.isArray(row.paths) ? row.paths.filter((path): path is string => typeof path === "string" && path.length <= 16_384).slice(0, 16) : [];
  const recovery = typeof row.recovery === "string" && /^chimaera-staging\/[A-Za-z0-9_-]+$/.test(row.recovery) ? row.recovery : null;
  return { text: `Git staging needs review for ${row.total} ${(row.total as number) === 1 ? "path" : "paths"}. Your conflicting staged version is kept; both index snapshots are saved for recovery.`, paths, recovery };
}

/** A fresh exact copy role and ownership epoch authorize the explicit action. */
export function takeoverEpoch(row: MirrorWorkspace | null | undefined): number | null {
  if (row?.local_copy?.state !== "ready" || row.local_copy.ready !== true || row.execution_allowed === true) return null;
  // A released holder has no Remote row. This epoch comes from the copy's
  // authenticated Baton read, not its historical checkpoint source epoch.
  const epoch = row.local_copy.owner_epoch ?? (row.ownership?.state === "remote" ? row.ownership.epoch : undefined);
  return Number.isSafeInteger(epoch) && (epoch as number) >= 0 ? epoch as number : null;
}

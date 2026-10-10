import { RETURN_WINDOW_ENDED_COPY } from "./presentation";

/** Opening a cloud project on this computer (Home's Open). Words name the
 *  project and the computer, never the copy machinery behind them. */
const ERRORS: Record<string, string> = {
  project_busy: "This project is saving its latest changes. Try Open again shortly.",
  project_copy_update_required: "Update Chimaera to open this project here.",
  project_checkpoint_pending: "This project hasn’t finished saving yet. Try again in a moment.",
  project_folder_not_empty: "That folder already contains files. Choose a new empty project folder so your existing work stays untouched.",
  project_folder_missing: "This project's local folder is unavailable. Restore or reconnect that folder, then try again.",
  project_folder_nested: "Choose a folder that isn't inside another project or Git repository.",
  account_changed: "Your account changed. Open the project again.",
  project_already_opening: "Another project action is finishing. Try again when it's done.",
  project_unavailable: "This project's files in your cloud aren't available right now. Try again shortly.",
  return_window_ended: RETURN_WINDOW_ENDED_COPY,
};
/** Fixed native codes only; daemon paths and diagnostics never become UI prose. */
export function projectCopyError(reason: unknown): string {
  const code = reason instanceof Error ? reason.message : String(reason);
  return Object.hasOwn(ERRORS, code) ? ERRORS[code] : "This project couldn't open here. Its files in your cloud are safe. Please try again shortly.";
}

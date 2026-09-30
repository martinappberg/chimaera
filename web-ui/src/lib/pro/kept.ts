/**
 * Both versions a Chimaera Pro return kept. When the cloud and this computer
 * both changed a file while apart, the cloud's version took the file's path
 * and this computer's version was saved right beside it as
 * `<name>.mine-<yyyymmdd-hhmm>` (the daemon's `pro/kept.rs`). This module is
 * the review's words and wire: which names are such copies (the file tree's
 * badge), what the chat's "back" line says, and the four daemon routes that
 * list the pairs, read both versions, and settle a pair.
 *
 * Plain words only: "the cloud", "this Mac" (or "this computer" off a Mac, or
 * "your computer" in a browser view of the project), "both versions".
 */

import { api, isRemoteHost } from "../net/api";
import { isBrowserGateway } from "../net/base";
import { isMac } from "../shared/keys";

/** The `.mine-<yyyymmdd-hhmm>` stamp, with the `-<n>` a second copy in the
 *  same minute gets (mirrors `canonical::kept_copy_name`). */
const KEPT_COPY = /\.mine-\d{8}-\d{4}(?:-\d+)?$/;

/** Whether a file name is a kept copy of this computer's version. */
export function isKeptCopy(name: string): boolean {
  const match = KEPT_COPY.exec(name);
  return match !== null && match.index > 0;
}

/** The file a kept copy sits beside (`notes.md` for
 *  `notes.md.mine-20260929-1412`), or null for any other name. */
export function keptOriginal(name: string): string | null {
  const match = KEPT_COPY.exec(name);
  return match !== null && match.index > 0 ? name.slice(0, match.index) : null;
}

/** The machine the project's files are on, as the viewer should call it. A
 *  browser view of the project (possibly on a phone) is not that machine,
 *  and a window onto a remote host is not "this Mac" either. */
export function hereName(): string {
  if (isBrowserGateway()) return "your computer";
  if (isRemoteHost() || !isMac) return "this computer";
  return "this Mac";
}

/** "This Mac" at the start of a sentence. */
export function hereTitle(): string {
  const here = hereName();
  return here.charAt(0).toUpperCase() + here.slice(1);
}

function files(n: number): string {
  return n === 1 ? "1 file" : `${n} files`;
}

/** The chat's line where the work came home with files both sides changed. */
export function backNote(total: number, here = hereName()): string {
  return `Back on ${here}. The cloud and ${here} both changed ${files(total)} while apart.`;
}

/** "Use the cloud's" hover hint for one file: where this computer's copy
 *  goes (the Trash, or deleted when the folder's drive has none). */
export function useCloudHint(deletedInCloud: boolean, trash: boolean, here = hereName()): string {
  if (deletedInCloud) {
    return trash
      ? `The cloud deleted this file; ${here}'s copy moves to the Trash`
      : `The cloud deleted this file; ${here}'s copy is deleted too`;
  }
  return trash
    ? `The cloud's version stays; ${here}'s copy moves to the Trash`
    : `The cloud's version stays; ${here}'s copy is deleted`;
}

/** The "Use the cloud's for all" confirmation's text. */
export function useCloudForAllBody(count: number, trash: boolean, here = hereName()): string {
  const Here = here.charAt(0).toUpperCase() + here.slice(1);
  return trash
    ? `${Here}'s versions of ${files(count)} will be moved to the Trash. The cloud's versions stay.`
    : `${Here}'s versions of ${files(count)} will be deleted: this folder's drive has no Trash. The cloud's versions stay.`;
}

/** After a choice the review said would use the Trash: copies no Trash
 *  could take, so they were deleted. */
export function deletedNote(count: number, here = hereName()): string {
  return count === 1
    ? `The Trash couldn't take ${here}'s copy, so it was deleted.`
    : `The Trash couldn't take ${count} of ${here}'s copies, so they were deleted.`;
}

/** The file tree badge's hover hint beside a kept copy. */
export function keptCopyHint(name: string, here = hereName()): string {
  const original = keptOriginal(name) ?? "the file";
  return (
    `${here.charAt(0).toUpperCase()}${here.slice(1)}'s version of ${original}, kept when the cloud and ` +
    `${here} both changed it while apart. Right-click to review both versions.`
  );
}

/** One pair, paths relative to the project folder. `path` holds the cloud's
 *  version (`size` null: the cloud deleted the file), `mine_path` this
 *  computer's. Times are Unix ms. */
export interface KeptPair {
  path: string;
  mine_path: string;
  size: number | null;
  mine_size: number;
  changed_at: number | null;
  mine_changed_at: number | null;
}

/** `GET /pro/projects/{w}/kept`. */
export interface KeptReview {
  workspace_id: string;
  /** Files still waiting for a choice (listed or not). */
  files: number;
  /** Files that return kept in both versions (never lowered by choices). */
  total: number;
  /** When the work came home (Unix ms); null once nothing waits. */
  returned_at: number | null;
  /** Kept copies the return did not name (it names up to 32). */
  unlisted: number;
  pairs: KeptPair[];
  /** The cloud's diverged branches, kept beside this computer's. */
  branches: string[];
  /** Whether the project is this computer's to change right now. */
  here: boolean;
  /** Whether a copy discarded here ("Use the cloud's") moves to the Trash;
   *  false: this folder's drive has no Trash, so it is deleted. */
  trash?: boolean;
  /** After `resolve_all`: pairs that could not take the choice. */
  failed?: { mine_path: string; error_code: string }[];
  /** After a choice: kept copies it moved to the Trash, and ones it
   *  deleted because no Trash could take them. */
  discarded?: { trash: number; deleted: number };
}

/** One version for the side-by-side view (`null`: not there). */
export interface KeptSide {
  size: number;
  changed_at: number | null;
  text: string | null;
  binary?: boolean;
  too_large?: boolean;
}

/** `GET /pro/projects/{w}/kept/file`. */
export interface KeptFile {
  path: string;
  mine_path: string;
  mine: KeptSide;
  cloud: KeptSide | null;
}

export type KeptChoice = "use_mine" | "use_cloud" | "keep_both";

/** A refused request, with the daemon's stable code. */
export class KeptError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
  ) {
    super(keptErrorLine(code));
    this.name = "KeptError";
  }
}

/** The daemon's codes, in the review's words. */
export function keptErrorLine(code: string): string {
  switch (code) {
    case "not_here":
      return "This project isn't on this computer right now. Choose once it's back.";
    case "busy":
      return "The project is being saved. Try again in a moment.";
    case "gone":
      return "This version is no longer in the folder.";
    case "not_kept":
      return "That file was already settled.";
    case "unsafe_path":
      return "That file can't be changed from here: it is a link or not a plain file.";
    case "folder_unavailable":
      return "The project folder isn't available.";
    case "unknown_project":
      return "This project isn't on this computer.";
    default:
      return "That didn't go through. Try again in a moment.";
  }
}

function base(workspaceId: string): string {
  return `/pro/projects/${encodeURIComponent(workspaceId)}/kept`;
}

async function read<T>(response: Response): Promise<T> {
  if (response.ok) return (await response.json()) as T;
  let code = "failed";
  try {
    const body = (await response.json()) as { error_code?: unknown };
    if (typeof body.error_code === "string") code = body.error_code;
  } catch {
    // Not the daemon's JSON (an older daemon's 404, a proxy page).
  }
  throw new KeptError(response.status, code);
}

export async function fetchKept(workspaceId: string, signal?: AbortSignal): Promise<KeptReview> {
  return read<KeptReview>(await api(base(workspaceId), { signal }));
}

export async function fetchKeptFile(workspaceId: string, minePath: string, signal?: AbortSignal): Promise<KeptFile> {
  const query = new URLSearchParams({ mine_path: minePath });
  return read<KeptFile>(await api(`${base(workspaceId)}/file?${query}`, { signal }));
}

export async function resolveKept(workspaceId: string, minePath: string, choice: KeptChoice): Promise<KeptReview> {
  return read<KeptReview>(
    await api(`${base(workspaceId)}/resolve`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ mine_path: minePath, choice }),
    }),
  );
}

export async function resolveAllKept(workspaceId: string, choice: KeptChoice): Promise<KeptReview> {
  return read<KeptReview>(
    await api(`${base(workspaceId)}/resolve_all`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ choice }),
    }),
  );
}

/** A size people read: "812 bytes", "4.2 KB", "1.3 MB". */
export function sizeLabel(bytes: number): string {
  if (bytes < 1000) return bytes === 1 ? "1 byte" : `${bytes} bytes`;
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1000;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/** The session key the daemon's `kept_both` notice carries for a project
 *  (`notices::push_kept_both`): a click on that notice opens the review. */
export const KEPT_NOTICE_PREFIX = "kept-both-";

export function keptNoticeWorkspace(sessionId: string): string | null {
  if (!sessionId.startsWith(KEPT_NOTICE_PREFIX)) return null;
  const id = sessionId.slice(KEPT_NOTICE_PREFIX.length);
  return /^[A-Za-z0-9_-]{1,128}$/.test(id) ? id : null;
}

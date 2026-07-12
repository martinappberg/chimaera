/**
 * The client-side fs-mutation bus: any create/rename/delete (tree, Finder,
 * pane-tab rename) bumps `fsEpoch` so every listing surface re-lists, and
 * publishes the mutation on `lastFsMutation` so App can rewrite/close open
 * tabs. The client analogue of the git-epoch refresh — and the only channel
 * for non-repo paths, which the server's git nudge never reaches (an in-repo
 * mutation refreshes twice; both re-lists are idempotent and cheap).
 *
 * The *Op wrappers are what surfaces call: files.ts stays a pure API client,
 * the notify lives here.
 */

import { writable } from "svelte/store";
import { fsCopy, fsCreate, fsDelete, fsMove, fsRename } from "../previews/files";

export type FsMutation =
  | { seq: number; kind: "create"; path: string }
  | { seq: number; kind: "rename"; from: string; to: string }
  | { seq: number; kind: "delete"; path: string };

/** Bumped on every mutation: "your listings may be stale, re-list". */
export const fsEpoch = writable(0);

/** The most recent mutation, for App's tab rewrite/close subscription. */
export const lastFsMutation = writable<FsMutation | null>(null);

let seq = 0;

type FsMutationInput =
  | { kind: "create"; path: string }
  | { kind: "rename"; from: string; to: string }
  | { kind: "delete"; path: string };

function notify(mutation: FsMutationInput): void {
  seq += 1;
  lastFsMutation.set({ ...mutation, seq });
  fsEpoch.update((n) => n + 1);
}

/** Announce that `path` appeared by some other route (an OS-desktop drop that
 *  already streamed to disk) so every listing surface re-lists. */
export function notifyCreated(path: string): void {
  notify({ kind: "create", path });
}

/** Create + notify. Resolves to the canonical created path. */
export async function fsCreateOp(path: string, kind: "file" | "dir"): Promise<string> {
  const created = await fsCreate(path, kind);
  notify({ kind: "create", path: created });
  return created;
}

/** Rename + notify (`from` as shown in listings, `to` canonical). */
export async function fsRenameOp(from: string, to: string): Promise<string> {
  const renamed = await fsRename(from, to);
  notify({ kind: "rename", from, to: renamed });
  return renamed;
}

/** Copy + notify. Published as a `create` (a new path appeared) so listings
 *  re-list; nothing follows a copy's source. Resolves to the new path. */
export async function fsCopyOp(
  from: string,
  to: string,
  onConflict: "fail" | "unique" = "fail",
): Promise<string> {
  const created = await fsCopy(from, to, onConflict);
  notify({ kind: "create", path: created });
  return created;
}

/** Move + notify. Published as a `rename` so open tabs FOLLOW the moved path
 *  (App's tab rewrite keys on rename). Resolves to the new path. */
export async function fsMoveOp(from: string, to: string): Promise<string> {
  const moved = await fsMove(from, to);
  notify({ kind: "rename", from, to: moved });
  return moved;
}

/** Delete + notify. */
export async function fsDeleteOp(path: string): Promise<void> {
  await fsDelete(path);
  notify({ kind: "delete", path });
}

/** A delete awaiting user confirmation; App renders the ConfirmDialog. */
export interface PendingDelete {
  path: string;
  kind: "dir" | "file";
}

export const pendingDelete = writable<PendingDelete | null>(null);

/** Ask the user to confirm deleting `path` (any surface may call this). */
export function requestDelete(path: string, kind: "dir" | "file"): void {
  pendingDelete.set({ path, kind });
}

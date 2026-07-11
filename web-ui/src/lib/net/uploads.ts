/**
 * Session-scoped uploads: the client half of POST /sessions/{id}/upload.
 * OS-desktop drops and terminal image pastes stream a file to the daemon
 * that OWNS the session (for remote windows the fetch rides the ssh tunnel,
 * so the file lands on the remote host), then the returned absolute path is
 * inserted into that session's input.
 *
 * The path INSERTION is registered by App (same one-handler pattern as
 * reference.ts): only App knows session kinds, cwds, and the chat-composer
 * vs PTY routing. A small store tracks in-flight jobs so the workbench can
 * show quiet progress/error chrome.
 */

import { writable } from "svelte/store";
import { api } from "./api";

export interface UploadResult {
  /** Absolute path on the host that owns the session. */
  path: string;
  /** Final (possibly dedupe-prefixed) filename. */
  name: string;
  size: number;
}

export interface UploadJob {
  id: number;
  name: string;
  /** Non-null once the job failed; the chip lingers briefly to say why. */
  error: string | null;
}

/** In-flight (and briefly, failed) uploads, for the status chrome. */
export const uploadJobs = writable<UploadJob[]>([]);

let jobSeq = 0;
/** How long a failed job's chip stays visible. */
const ERROR_LINGER_MS = 5000;

function finishJob(id: number, error: string | null): void {
  if (error === null) {
    uploadJobs.update((jobs) => jobs.filter((j) => j.id !== id));
    return;
  }
  uploadJobs.update((jobs) => jobs.map((j) => (j.id === id ? { ...j, error } : j)));
  setTimeout(() => {
    uploadJobs.update((jobs) => jobs.filter((j) => j.id !== id));
  }, ERROR_LINGER_MS);
}

/** Surface an upload-adjacent failure (e.g. a folder drop) in the same
 *  chrome as a failed upload. */
export function reportUploadError(message: string): void {
  const id = ++jobSeq;
  uploadJobs.update((jobs) => [...jobs, { id, name: message, error: message }]);
  setTimeout(() => {
    uploadJobs.update((jobs) => jobs.filter((j) => j.id !== id));
  }, ERROR_LINGER_MS);
}

/** POST the blob to the session's upload route (fetch streams the body). */
export async function uploadToSession(
  sessionId: string,
  blob: Blob,
  name: string,
): Promise<UploadResult> {
  const res = await api(`/sessions/${sessionId}/upload?name=${encodeURIComponent(name)}`, {
    method: "POST",
    body: blob,
  });
  if (!res.ok) {
    let message = `upload failed (${res.status})`;
    try {
      const body = (await res.json()) as { error?: string };
      if (typeof body.error === "string") message = body.error;
    } catch {
      // non-JSON error body: keep the status message
    }
    throw new Error(message);
  }
  return (await res.json()) as UploadResult;
}

type PathInserter = (sessionId: string, absPath: string) => void;
let pathInserter: PathInserter | null = null;

/** App-level wiring: the one function that composes + types an uploaded
 *  path into a session's input (agent @mention / shell-escaped path). */
export function setUploadPathInserter(fn: PathInserter | null): void {
  pathInserter = fn;
}

/**
 * Upload `blob` for `sessionId` under `name`, tracking a job chip, then hand
 * the resulting path to the registered inserter. Errors surface on the chip;
 * nothing is ever typed for a failed upload.
 */
export async function uploadAndInsert(sessionId: string, blob: Blob, name: string): Promise<void> {
  const id = ++jobSeq;
  uploadJobs.update((jobs) => [...jobs, { id, name, error: null }]);
  try {
    const result = await uploadToSession(sessionId, blob, name);
    finishJob(id, null);
    pathInserter?.(sessionId, result.path);
  } catch (e) {
    finishJob(id, e instanceof Error ? e.message : "upload failed");
  }
}

/** Filename for a pasted clipboard image, from its mime type. */
export function pastedImageName(mediaType: string): string {
  const ext = mediaType === "image/jpeg" ? "jpg" : (mediaType.split("/")[1] ?? "png");
  const stamp = new Date()
    .toISOString()
    .replace(/[-:]/g, "")
    .replace(/\..+$/, "")
    .replace("T", "-");
  return `pasted-${stamp}.${ext}`;
}

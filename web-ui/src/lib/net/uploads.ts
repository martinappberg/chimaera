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
import { getToken, notifyUnauthorized } from "./api";

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
  /** 0…1 while bytes are moving; null for non-upload file operations. */
  progress: number | null;
  /** Present only while an operation can be cancelled safely. */
  cancel: (() => void) | null;
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
  uploadJobs.update((jobs) =>
    jobs.map((j) => (j.id === id ? { ...j, error, cancel: null } : j)),
  );
  setTimeout(() => {
    uploadJobs.update((jobs) => jobs.filter((j) => j.id !== id));
  }, ERROR_LINGER_MS);
}

function updateJob(id: number, patch: Partial<UploadJob>): void {
  uploadJobs.update((jobs) => jobs.map((job) => (job.id === id ? { ...job, ...patch } : job)));
}

/** Surface an upload-adjacent failure (e.g. a folder drop) in the same
 *  chrome as a failed upload. */
export function reportUploadError(message: string): void {
  const id = ++jobSeq;
  uploadJobs.update((jobs) => [
    ...jobs,
    { id, name: message, error: message, progress: null, cancel: null },
  ]);
  setTimeout(() => {
    uploadJobs.update((jobs) => jobs.filter((j) => j.id !== id));
  }, ERROR_LINGER_MS);
}

export interface UploadProgress {
  onProgress?: (progress: number) => void;
  onCancelReady?: (cancel: () => void) => void;
}

const UPLOAD_STALL_MS = 120_000;

/** Native Blob upload with progress and a no-progress deadline. XHR sends the
 * Blob without copying it into JavaScript memory, works in WKWebView and the
 * browser, and exposes upload progress that fetch still does not. */
function postBlob(path: string, blob: Blob, progress: UploadProgress = {}): Promise<UploadResult> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    let stallTimer: ReturnType<typeof setTimeout> | null = null;
    let settled = false;

    const clearStall = () => {
      if (stallTimer !== null) clearTimeout(stallTimer);
      stallTimer = null;
    };
    const fail = (message: string) => {
      if (settled) return;
      settled = true;
      clearStall();
      reject(new Error(message));
    };
    const armStall = () => {
      if (settled) return;
      clearStall();
      stallTimer = setTimeout(() => {
        fail("upload stopped making progress — the SSH tunnel or destination filesystem may be unavailable");
        xhr.abort();
      }, UPLOAD_STALL_MS);
    };
    const errorMessage = (): string => {
      let message = `upload failed (${xhr.status})`;
      try {
        const body = JSON.parse(xhr.responseText) as { error?: string };
        if (typeof body.error === "string") message = body.error;
      } catch {
        // Non-JSON response: keep the status message.
      }
      return message;
    };

    xhr.open("POST", `/api/v1${path}`);
    const token = getToken();
    if (token !== null) xhr.setRequestHeader("Authorization", `Bearer ${token}`);
    xhr.upload.onprogress = (event) => {
      armStall();
      const total = event.lengthComputable && event.total > 0 ? event.total : blob.size;
      if (total > 0) progress.onProgress?.(Math.min(0.99, event.loaded / total));
    };
    xhr.upload.onload = armStall;
    xhr.onreadystatechange = () => {
      if (xhr.readyState >= XMLHttpRequest.HEADERS_RECEIVED) armStall();
    };
    xhr.onerror = () => fail("upload connection failed");
    xhr.onabort = () => fail("upload cancelled");
    xhr.onload = () => {
      if (settled) return;
      clearStall();
      if (xhr.status === 401) notifyUnauthorized();
      if (xhr.status < 200 || xhr.status >= 300) {
        fail(errorMessage());
        return;
      }
      try {
        const result = JSON.parse(xhr.responseText) as UploadResult;
        settled = true;
        progress.onProgress?.(1);
        resolve(result);
      } catch {
        fail("upload completed but the host returned an invalid response");
      }
    };
    progress.onCancelReady?.(() => xhr.abort());
    progress.onProgress?.(0);
    armStall();
    xhr.send(blob);
  });
}

/** POST the blob to the session's upload route. */
export async function uploadToSession(
  sessionId: string,
  blob: Blob,
  name: string,
  progress: UploadProgress = {},
): Promise<UploadResult> {
  return postBlob(`/sessions/${sessionId}/upload?name=${encodeURIComponent(name)}`, blob, progress);
}

/** POST the blob to the folder-upload route (an OS-desktop drop onto a Finder
 *  pane or the FILES tree lands in `dir`). Streams the body; returns the
 *  daemon-absolute path it landed at. */
export async function uploadToDir(
  dir: string,
  blob: Blob,
  name: string,
  progress: UploadProgress = {},
): Promise<UploadResult> {
  const q = `dir=${encodeURIComponent(dir)}&name=${encodeURIComponent(name)}`;
  return postBlob(`/fs/upload?${q}`, blob, progress);
}

/**
 * Run a file operation (copy/move/folder-upload) behind a transient chip in
 * the same chrome as uploads: `label` shows while it runs, clears on success,
 * lingers with the error on failure. Returns the result, or null on failure
 * (the chip already surfaced why). Never throws.
 */
export async function trackFileOp<T>(
  label: string,
  run: (progress: UploadProgress) => Promise<T>,
): Promise<T | null> {
  const id = ++jobSeq;
  let cancelled = false;
  uploadJobs.update((jobs) => [
    ...jobs,
    { id, name: label, error: null, progress: null, cancel: null },
  ]);
  const reporter: UploadProgress = {
    onProgress: (progress) => updateJob(id, { progress }),
    onCancelReady: (cancel) =>
      updateJob(id, {
        cancel: () => {
          cancelled = true;
          cancel();
        },
      }),
  };
  try {
    const result = await run(reporter);
    finishJob(id, null);
    return result;
  } catch (e) {
    if (cancelled) {
      finishJob(id, null);
      return null;
    }
    finishJob(id, e instanceof Error ? e.message : "operation failed");
    return null;
  }
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
  let cancelled = false;
  uploadJobs.update((jobs) => [
    ...jobs,
    { id, name: `Uploading ${name}…`, error: null, progress: 0, cancel: null },
  ]);
  try {
    const result = await uploadToSession(sessionId, blob, name, {
      onProgress: (progress) => updateJob(id, { progress }),
      onCancelReady: (cancel) =>
        updateJob(id, {
          cancel: () => {
            cancelled = true;
            cancel();
          },
        }),
    });
    finishJob(id, null);
    pathInserter?.(sessionId, result.path);
  } catch (e) {
    if (cancelled) {
      finishJob(id, null);
      return;
    }
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

/**
 * The upload transport: the client half of the daemon's `upload::chunked`
 * routes (`POST /uploads`, `PUT /uploads/{id}?offset=`, `GET /uploads/{id}`,
 * `POST /uploads/{id}/finish`, `DELETE /uploads/{id}`).
 *
 * A file goes up in pieces, and the next piece is sent only after the daemon
 * answered the last one. That one rule is the fix for long uploads to a
 * remote host: a remote window's requests all ride ONE ssh connection, and a
 * whole file handed to it at once sat in that connection's send queue for
 * the entire transfer, so the terminals, the chat sockets and the app's
 * tunnel health check each waited seconds behind it (the app then judged the
 * tunnel dead and tore it down under the upload). With one piece in flight,
 * sized to take about `CHUNK_TARGET_MS`, nothing waits longer than that.
 *
 * The same rule makes the rest honest:
 * - progress counts bytes the daemon has confirmed, not bytes handed to a
 *   socket;
 * - a piece that fails is asked about (`GET` answers the daemon's offset) and
 *   sent again from where the daemon stands, so a dropped link costs one
 *   piece, and a piece is never applied twice;
 * - each piece carries its SHA-256, and `finish` carries a running digest of
 *   all of them (`chain`), which the daemon compares with its own before it
 *   publishes the file.
 *
 * Memory is one piece (at most `CHUNK_MAX`) whatever the file size.
 */

import { api } from "./api";

export interface UploadResult {
  /** Absolute path on the host that owns the session. */
  path: string;
  /** Final (possibly dedupe-prefixed) filename. */
  name: string;
  size: number;
}

/** A session's landing pad, or a folder on the host. */
export type UploadDestination = { session: string } | { dir: string };

export interface ChunkedUploadOptions {
  /** 0…1 of the file the daemon has confirmed (1 only once it is published). */
  onProgress?: (fraction: number) => void;
  /** true while the transport waits for the link to come back, false once
   *  bytes move again. */
  onWaiting?: (waiting: boolean) => void;
  /** Aborting cancels the upload and removes the daemon's partial copy. */
  signal?: AbortSignal;
}

/** The smallest piece: on a link too slow for even this to take
 *  `CHUNK_TARGET_MS`, smaller pieces would mostly carry headers. */
export const CHUNK_MIN = 16 * 1024;
/** The first piece, before anything is known about the link: small enough
 *  that a 20 KB/s link clears it within the app's 2 s health deadline. */
export const CHUNK_START = 32 * 1024;
/** The largest piece — also what one upload holds in memory. */
export const CHUNK_MAX = 8 * 1024 * 1024;
/** How long one piece should take to be sent and answered. It bounds how
 *  long anything else on the connection waits behind an upload, against the
 *  round trip it costs between pieces (~10–15% of throughput at 60 ms RTT). */
export const CHUNK_TARGET_MS = 500;
/** A piece may grow at most this much over the last one, so one lucky
 *  measurement cannot queue seconds of data. */
const CHUNK_GROWTH = 4;
/** One piece's deadline. Past it the request is dropped and the daemon is
 *  asked where it stands. */
const CHUNK_TIMEOUT_MS = 60_000;
/** begin / status / cancel. */
const CONTROL_TIMEOUT_MS = 20_000;
/** finish: the daemon syncs the file before it publishes it. */
const FINISH_TIMEOUT_MS = 120_000;
/** How long an upload keeps trying while nothing gets through. */
export const STALL_MS = 120_000;
/** Time spent in one failed attempt counts for at most this much toward
 *  `STALL_MS`, so a laptop that slept does not wake up already timed out. */
const STALL_STEP_MAX_MS = 30_000;
const BACKOFF_MS = [500, 1000, 2000, 4000, 8000];
/** A piece the daemon says arrived changed is sent again this many times. */
const CHECKSUM_RETRIES = 3;

/** What the transport needs from the world; tests replace it. */
export interface UploadIo {
  request(path: string, init: RequestInit, timeoutMs: number, signal?: AbortSignal): Promise<Response>;
  now(): number;
  sleep(ms: number, signal?: AbortSignal): Promise<void>;
  /** SHA-256 of `bytes`, or null where WebCrypto is unavailable (a page not
   *  served from localhost or https) — then pieces go unverified. */
  sha256(bytes: ArrayBuffer | Uint8Array): Promise<Uint8Array | null>;
}

/** Thrown for a cancel, so callers can tell it from a failure. */
export class UploadCancelled extends Error {
  constructor() {
    super("upload cancelled");
    this.name = "UploadCancelled";
  }
}

/** Run `request` with a deadline and the caller's cancel signal, without
 *  `AbortSignal.any` (older WebKit web views lack it). The answer's body is
 *  read inside the same deadline: a link that dies between the headers and
 *  the (tiny) body must fail the attempt, not hang it. */
async function requestWithDeadline(
  path: string,
  init: RequestInit,
  timeoutMs: number,
  signal?: AbortSignal,
): Promise<Response> {
  const controller = new AbortController();
  const onAbort = () => controller.abort();
  if (signal?.aborted) controller.abort();
  signal?.addEventListener("abort", onAbort, { once: true });
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await api(path, { ...init, signal: controller.signal });
    const body = await res.text();
    return new Response(body.length > 0 ? body : null, { status: res.status, headers: res.headers });
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener("abort", onAbort);
  }
}

export const browserIo: UploadIo = {
  request: requestWithDeadline,
  now: () => performance.now(),
  sleep: (ms, signal) =>
    new Promise((resolve) => {
      const done = () => {
        clearTimeout(timer);
        signal?.removeEventListener("abort", done);
        resolve();
      };
      const timer = setTimeout(done, ms);
      signal?.addEventListener("abort", done, { once: true });
    }),
  sha256: async (bytes) => {
    const subtle = globalThis.crypto?.subtle;
    if (subtle === undefined) return null;
    return new Uint8Array(await subtle.digest("SHA-256", bytes as BufferSource));
  },
};

function hex(bytes: Uint8Array): string {
  let out = "";
  for (const byte of bytes) out += byte.toString(16).padStart(2, "0");
  return out;
}

/** `1.2 GB` — binary multiples, the way the daemon's messages write them. */
export function humanBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} bytes`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit + 1 < units.length) {
    value /= 1024;
    unit += 1;
  }
  const rounded = value >= 100 || Math.abs(value - Math.round(value)) < 0.05;
  return `${rounded ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

/**
 * The next piece's size: what the link just carried in `CHUNK_TARGET_MS`,
 * within `CHUNK_MIN`…`max` and at most `CHUNK_GROWTH` times the last piece.
 * `elapsedMs` is the whole round trip of a piece of `bytes`, so the latency
 * is priced in and the pieces converge on the target duration.
 */
export function nextChunkSize(
  previous: number,
  bytes: number,
  elapsedMs: number,
  max: number = CHUNK_MAX,
): number {
  const perMs = bytes / Math.max(1, elapsedMs);
  const wanted = Math.floor(perMs * CHUNK_TARGET_MS);
  const ceiling = Math.min(max, previous * CHUNK_GROWTH);
  return Math.max(CHUNK_MIN, Math.min(ceiling, wanted));
}

/** The daemon's `{error}` for a refused request, else the status. */
async function refusal(res: Response): Promise<string> {
  try {
    const body = (await res.json()) as { error?: unknown };
    if (typeof body.error === "string") return body.error;
  } catch {
    // Not JSON: the status has to do.
  }
  return `upload failed (${res.status})`;
}

/** A status a later try can get past: the tunnel's own gateway errors, and a
 *  piece the daemon stopped waiting for. */
function transient(status: number): boolean {
  return status === 408 || status === 502 || status === 503 || status === 504;
}

/** Where a failed piece left the upload, as far as the client can tell. */
type Standing =
  | { kind: "at"; offset: number }
  /** The daemon no longer knows the upload (it restarted, or it expired). */
  | { kind: "gone" };

/**
 * Upload `blob` to `destination` under `name`; resolves with where it
 * landed. Rejects with `UploadCancelled` on abort, else with an `Error`
 * whose message says what happened. Either way the daemon keeps no partial
 * file: a failure or a cancel removes it, and an upload the daemon never
 * hears from again expires on its own.
 */
export async function uploadChunked(
  destination: UploadDestination,
  blob: Blob,
  name: string,
  options: ChunkedUploadOptions = {},
  io: UploadIo = browserIo,
): Promise<UploadResult> {
  const { signal } = options;
  const total = blob.size;
  /** Milliseconds of failed attempts since the daemon last confirmed a byte. */
  let stalled = 0;
  let waiting = false;
  let id: string | null = null;

  const setWaiting = (next: boolean) => {
    if (next === waiting) return;
    waiting = next;
    options.onWaiting?.(next);
  };
  const cancelled = () => signal?.aborted === true;
  const checkCancelled = () => {
    if (cancelled()) throw new UploadCancelled();
  };
  /** Tell the daemon to drop its partial copy. Best effort: one it never
   *  hears about expires on its own. */
  const discard = (uploadId: string | null) => {
    if (uploadId === null) return;
    void io.request(`/uploads/${uploadId}`, { method: "DELETE" }, CONTROL_TIMEOUT_MS).catch(() => {});
  };

  /**
   * One request that may be tried again. Returns the response, or null for
   * an attempt the link swallowed (no answer, or a gateway error) after
   * charging it to the stall budget and backing off. Throws once the budget
   * is spent.
   */
  let attempt = 0;
  const tryRequest = async (
    path: string,
    init: RequestInit,
    timeoutMs: number,
    sent: number,
  ): Promise<Response | null> => {
    checkCancelled();
    const started = io.now();
    let res: Response | null = null;
    try {
      res = await io.request(path, init, timeoutMs, signal);
    } catch {
      checkCancelled();
    }
    if (res !== null && !transient(res.status)) {
      attempt = 0;
      return res;
    }
    setWaiting(true);
    const backoff = BACKOFF_MS[Math.min(attempt, BACKOFF_MS.length - 1)];
    attempt += 1;
    stalled += Math.min(STALL_STEP_MAX_MS, io.now() - started);
    if (stalled >= STALL_MS) {
      throw new Error(
        `upload stopped at ${humanBytes(sent)} of ${humanBytes(total)}: the connection to the host did not come back`,
      );
    }
    await io.sleep(backoff, signal);
    stalled += backoff;
    checkCancelled();
    return null;
  };

  const begin = async (): Promise<{ id: string; chunkMax: number }> => {
    for (;;) {
      const res = await tryRequest(
        "/uploads",
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ ...destination, name, size: total }),
        },
        CONTROL_TIMEOUT_MS,
        0,
      );
      if (res === null) continue;
      if (!res.ok) throw new Error(await refusal(res));
      const body = (await res.json()) as { id: string; chunk_max?: number };
      const chunkMax = typeof body.chunk_max === "number" ? body.chunk_max : CHUNK_MAX;
      return { id: body.id, chunkMax: Math.max(CHUNK_MIN, Math.min(CHUNK_MAX, chunkMax)) };
    }
  };

  /** Ask the daemon where it stands, until it answers. */
  const standing = async (uploadId: string, sent: number): Promise<Standing> => {
    for (;;) {
      const res = await tryRequest(`/uploads/${uploadId}`, { method: "GET" }, CONTROL_TIMEOUT_MS, sent);
      if (res === null) continue;
      if (res.status === 404) return { kind: "gone" };
      if (!res.ok) throw new Error(await refusal(res));
      const body = (await res.json()) as { offset: number };
      return { kind: "at", offset: body.offset };
    }
  };

  /** One whole pass over the file. `false` = the daemon lost the upload and
   *  the file has to go again from the start. */
  const pass = async (): Promise<UploadResult | false> => {
    const opened = await begin();
    id = opened.id;
    const uploadId = opened.id;
    let offset = 0;
    let size = Math.min(CHUNK_START, opened.chunkMax);
    let chain: Uint8Array | null = new Uint8Array(32);
    let checksumFailures = 0;
    options.onProgress?.(0);

    while (offset < total) {
      checkCancelled();
      const end = Math.min(total, offset + size);
      let bytes: ArrayBuffer;
      try {
        bytes = await blob.slice(offset, end).arrayBuffer();
      } catch {
        throw new Error(`could not read ${name}: it changed or moved during the upload`);
      }
      const digest = await io.sha256(bytes);
      const headers: Record<string, string> = { "Content-Type": "application/octet-stream" };
      if (digest !== null) headers["x-chunk-sha256"] = hex(digest);

      const started = io.now();
      const res = await tryRequest(
        `/uploads/${uploadId}?offset=${offset}`,
        { method: "PUT", headers, body: bytes },
        CHUNK_TIMEOUT_MS,
        offset,
      );
      /** Where the daemon is after this piece, once known. */
      let landed: number;
      if (res === null) {
        // No answer. The piece may or may not have been committed: ask.
        const where = await standing(uploadId, offset);
        if (where.kind === "gone") return false;
        landed = where.offset;
      } else if (res.ok) {
        landed = ((await res.json()) as { offset: number }).offset;
      } else if (res.status === 409) {
        const body = (await res.json().catch(() => ({}))) as { offset?: unknown };
        // Without an offset the daemon only said another request had the
        // seat (an earlier try of this piece, still draining): ask.
        if (typeof body.offset === "number") {
          landed = body.offset;
        } else {
          const where = await standing(uploadId, offset);
          if (where.kind === "gone") return false;
          landed = where.offset;
        }
      } else if (res.status === 404) {
        return false;
      } else if (res.status === 422) {
        checksumFailures += 1;
        if (checksumFailures > CHECKSUM_RETRIES) throw new Error(await refusal(res));
        continue;
      } else {
        throw new Error(await refusal(res));
      }

      if (landed === offset) continue; // not committed: the same piece again
      if (landed !== end) {
        // Neither before nor after this piece: not an upload this client
        // can vouch for any more.
        throw new Error("upload failed: the host and this window disagree about what was sent");
      }
      if (chain !== null) {
        if (digest === null) {
          chain = null;
        } else {
          const link = new Uint8Array(64);
          link.set(chain, 0);
          link.set(digest, 32);
          chain = await io.sha256(link);
        }
      }
      offset = end;
      checksumFailures = 0;
      stalled = 0;
      setWaiting(false);
      options.onProgress?.(total > 0 ? Math.min(0.99, offset / total) : 0.99);
      if (res !== null && res.ok) {
        size = nextChunkSize(size, bytes.byteLength, io.now() - started, opened.chunkMax);
      }
    }

    for (;;) {
      const res = await tryRequest(
        `/uploads/${uploadId}/finish`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(chain === null ? {} : { chain: hex(chain) }),
        },
        FINISH_TIMEOUT_MS,
        total,
      );
      if (res === null) continue;
      if (res.status === 404) return false;
      if (!res.ok) throw new Error(await refusal(res));
      setWaiting(false);
      return (await res.json()) as UploadResult;
    }
  };

  try {
    // The daemon forgets an upload when it restarts; the file then goes
    // once more from the start, and a second loss is reported.
    for (let restarts = 0; ; restarts += 1) {
      const result = await pass();
      if (result !== false) {
        options.onProgress?.(1);
        // The answer is in hand: the daemon need not keep it for a retry.
        discard(id);
        id = null;
        return result;
      }
      id = null;
      if (restarts >= 1) {
        throw new Error("upload failed: the host lost it twice (the daemon restarted during the upload)");
      }
    }
  } catch (error) {
    discard(id);
    if (cancelled()) throw new UploadCancelled();
    throw error;
  } finally {
    setWaiting(false);
  }
}

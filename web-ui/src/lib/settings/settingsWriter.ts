/**
 * The single writer behind `PUT /api/v1/settings`. The PUT replaces the whole
 * map, so the map itself is the queue: there is never more than one body worth
 * sending — the current one — and at most one request whose answer counts.
 *
 *   idle      the map matches what the daemon last accepted from us
 *   pending   a local edit waits for its debounce (or for a retry slot)
 *   inflight  a PUT is outstanding; edits made meanwhile ride the next one
 *   failed    the daemon did not answer (or asked us to retry); a timer, a
 *             new edit, a settings frame (the events socket reconnected), or
 *             a flush re-sends
 *   rejected  the daemon refused this map (e.g. 413); re-sending the same
 *             body cannot succeed, so only an edit or a flush sends again
 *
 * `pending`, `inflight` and `failed` are the echo-guard: remote frames must
 * not revert edits the daemon has not accepted yet. `rejected` is not — a map
 * the daemon will never store must not deafen the window to every other
 * window's changes, so the next frame replaces it (the indicator stays up,
 * saying why the edit was lost, until an edit or a successful flush).
 */

import { retryDelayMs } from "../net/reconnect";

export type SavePhase = "idle" | "pending" | "inflight" | "failed" | "rejected";

/** One PUT's outcome: the HTTP status (0 when none came back) and, for a
 *  refusal, the daemon's error text. */
export interface PutResult {
  status: number;
  error?: string;
}

export const PUT_DEBOUNCE_MS = 400;
const RETRY_INITIAL_MS = 1_000;
const RETRY_MAX_MS = 30_000;

export interface WriterDeps {
  /** The body to persist now — the serialized current map. */
  snapshot(): string;
  /** Send one PUT. Rejecting counts as no answer. */
  put(body: string, signal: AbortSignal): Promise<PutResult>;
  /** Whether nobody can see the window (the retry takes the slow tier). */
  hidden(): boolean;
  /** The phase or the rejection message changed. */
  onChange(): void;
}

interface Request {
  body: string;
  abort: AbortController;
  done: Promise<boolean>;
}

/** No answer, a server fault, or a request the daemon asked us to repeat.
 *  Any other non-2xx refuses this map. */
function transient(status: number): boolean {
  return status === 0 || status === 408 || status === 429 || status >= 500;
}

export function retryBackoffMs(failures: number): number {
  return Math.min(RETRY_INITIAL_MS * 2 ** Math.max(0, failures - 1), RETRY_MAX_MS);
}

export class SettingsWriter {
  #phase: SavePhase = "idle";
  #rejection = "";
  #timer: ReturnType<typeof setTimeout> | null = null;
  #current: Request | null = null;
  #failures = 0;

  constructor(private readonly deps: WriterDeps) {}

  get phase(): SavePhase {
    return this.#phase;
  }

  /** The daemon's reason while `rejected`, else "". */
  get rejection(): string {
    return this.#rejection;
  }

  /** True while remote frames must not replace the local map. */
  dirty(): boolean {
    return this.#phase !== "idle" && this.#phase !== "rejected";
  }

  /** A local edit landed: send it once edits pause. */
  edited(): void {
    this.#arm(PUT_DEBOUNCE_MS);
    if (this.#current === null) this.#set("pending");
  }

  /** A settings frame arrived. In `failed` it means the daemon is reachable
   *  again (the events socket pushes the map on every connect), so retry. A
   *  rejected map is not re-sent: the same body would be refused again. */
  remoteFrame(): void {
    if (this.#phase === "failed") void this.flush();
  }

  /**
   * Send the current map now; resolves true once the daemon accepted a map
   * holding every edit made before this call, false if that write failed.
   *
   * An outstanding PUT of the same body is joined, not repeated. One of an
   * older body is aborted and superseded, and its waiters follow the newer
   * request. The abort only stops a request that has not reached the daemon
   * yet: one already delivered may still be applied, and not necessarily
   * before the newer PUT. What holds is that this window never takes the
   * older answer as the outcome, and the newest map is the last one it sends.
   */
  flush(): Promise<boolean> {
    if (this.#phase === "idle") return Promise.resolve(true);
    this.#disarm();
    const body = this.deps.snapshot();
    if (this.#current?.body === body) return this.#current.done;
    this.#current?.abort.abort();
    return this.#send(body);
  }

  #send(body: string): Promise<boolean> {
    const abort = new AbortController();
    const request: Request = { body, abort, done: Promise.resolve(false) };
    this.#current = request;
    this.#set("inflight");
    request.done = this.deps.put(body, abort.signal).then(
      (result) => this.#settle(request, result),
      () => this.#settle(request, { status: 0 }),
    );
    return request.done;
  }

  #settle(request: Request, result: PutResult): boolean | Promise<boolean> {
    if (this.#current !== request) return this.#supersededOutcome();
    this.#current = null;
    if (result.status < 200 || result.status >= 300) {
      this.#fail(result);
      return false;
    }
    this.#failures = 0;
    if (this.deps.snapshot() === request.body) {
      this.#disarm();
      this.#set("idle");
      return true;
    }
    // Edited while the PUT was out: send now unless a debounce is still open.
    this.#set("pending");
    if (this.#timer === null) void this.#send(this.deps.snapshot());
    return true;
  }

  /** A superseded request's answer is older than the map sent after it, so
   *  its waiters take the newer request's outcome instead. */
  #supersededOutcome(): boolean | Promise<boolean> {
    if (this.#current !== null) return this.#current.done;
    return this.#phase !== "failed" && this.#phase !== "rejected";
  }

  #fail(result: PutResult): void {
    this.#failures += 1;
    if (transient(result.status)) {
      this.#set("failed");
      this.#arm(retryDelayMs(retryBackoffMs(this.#failures), this.deps.hidden(), this.#failures));
      return;
    }
    this.#set("rejected", result.error ?? `refused (${result.status})`);
  }

  #arm(delayMs: number): void {
    this.#disarm();
    this.#timer = setTimeout(() => {
      this.#timer = null;
      // An outstanding PUT settles first and picks the edit up itself.
      if (this.#current === null) void this.#send(this.deps.snapshot());
    }, delayMs);
  }

  #disarm(): void {
    if (this.#timer === null) return;
    clearTimeout(this.#timer);
    this.#timer = null;
  }

  #set(phase: SavePhase, rejection = ""): void {
    if (this.#phase === phase && this.#rejection === rejection) return;
    this.#phase = phase;
    this.#rejection = rejection;
    this.deps.onChange();
  }
}

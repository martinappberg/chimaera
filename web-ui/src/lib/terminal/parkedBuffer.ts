/**
 * The parked-terminal buffering state machine (see termPoolRuntime's module
 * header: parked terminals buffer, don't parse). Pure state — no xterm, no
 * sockets — so the interleavings that matter (park → write → adopt ordering,
 * the overflow latch, reset-supersedes-buffer, foreign-resize discard, the
 * exited flush, snapshot write-through) are enumerable in unit tests.
 *
 * Parking is an explicit lifecycle transition (release() parks, adopt()
 * unparks) — never derived from DOM topology, which flows like a pane
 * drag-out can rearrange without a release.
 *
 * States while parked:
 * - buffering: binary frames queue in order, bounded by `maxBytes`.
 * - awaiting-snapshot: a reset-bearing frame (server resync, or a
 *   reconnect's ready) was applied; the NEXT binary frame is the full
 *   snapshot — a wire invariant, the server sends the reset frame and the
 *   snapshot adjacently — and is written through to the terminal even while
 *   hidden. One bounded hidden parse beats discarding the snapshot and
 *   forcing a second server-side render on adopt.
 * - desynced: the stream was discarded (buffer overflow, a foreign resize
 *   that re-wrapped the server grid under old-width bytes, or an exit while
 *   awaiting a snapshot). The grid cannot catch up from the stream; adopt
 *   must resync the socket (a fresh attach re-snapshots), and the latch
 *   clears only once a resync was actually issued — a refused resync (fatal
 *   socket) keeps it latched rather than pretending the grid is current.
 */

/** Where one binary frame should go. */
export type BinaryRoute = "write" | "buffered" | "dropped";

export interface AdoptDirective {
  /** Buffered frames to replay, in order, before live writes resume. */
  flush: Uint8Array[];
  /**
   * The stream was discarded: force a socket resync. On success call
   * resyncIssued() to clear the latch; a failed/refused resync keeps it.
   */
  resync: boolean;
}

export class ParkedBuffer {
  private parked = false;
  private chunks: Uint8Array[] = [];
  private bytes = 0;
  private desynced = false;
  private awaitingSnapshot = false;

  constructor(private readonly maxBytes: number) {}

  /** The entry moved to the hidden stash (release). */
  park(): void {
    this.parked = true;
  }

  /** Whether the entry is currently parked — the socket reads this to attach
   *  (and re-auth after a reconnect) in parked mode, where the server
   *  withholds output and skips the snapshot. */
  isParked(): boolean {
    return this.parked;
  }

  /**
   * The grid can no longer catch up from the stream — discard and latch
   * desynced so adopt resyncs (a fresh visible attach re-snapshots). Fired
   * on a parked reconnect: the server sent no snapshot (parked auth), and
   * any bytes buffered before the connection dropped predate an output gap
   * of unknown size. No-op while visible.
   */
  desync(): void {
    if (!this.parked) return;
    this.discard();
    this.awaitingSnapshot = false;
    this.desynced = true;
  }

  /**
   * Route one binary frame: "write" = parse into the terminal now (visible,
   * or the parked snapshot write-through), "buffered" = queued for adopt,
   * "dropped" = discarded (desynced; adopt will resync).
   */
  binary(data: Uint8Array): BinaryRoute {
    if (!this.parked) return "write";
    if (this.awaitingSnapshot) {
      this.awaitingSnapshot = false;
      return "write";
    }
    if (this.desynced) return "dropped";
    this.bytes += data.byteLength;
    if (this.bytes > this.maxBytes) {
      // Replaying a truncated escape stream would corrupt the grid.
      this.discard();
      this.desynced = true;
      return "dropped";
    }
    this.chunks.push(data);
    return "buffered";
  }

  /**
   * A reset-bearing frame arrived (server resync, or a reconnect's ready).
   * The caller applies resize-before-reset to the terminal in both parked
   * and visible states; the snapshot that follows supersedes everything
   * buffered — and recovers a desynced entry without a reconnect.
   */
  reset(): void {
    this.discard();
    this.desynced = false;
    this.awaitingSnapshot = this.parked;
  }

  /**
   * The server grid was resized out from under a parked terminal (a foreign
   * resize — a parked pane never initiates one). Buffered old-width bytes
   * are unreplayable: discard and latch desynced. The server's debounced
   * foreign-resize resync normally repaints (clearing the latch via
   * reset()); an adopt inside that window resyncs instead. Visible
   * terminals are unaffected — xterm reflows its own buffer.
   */
  resized(): void {
    this.desync();
  }

  /**
   * The session exited. Returns the buffered tail to flush (the last words)
   * — empty when the stream was already discarded. Either way a PARKED exit
   * latches desynced: a park-aware server withholds output while parked, so
   * the buffer cannot be trusted to hold the complete tail (and the exit
   * event can even outrun the PTY's final bytes server-side) — adopt
   * resyncs into the server's last-words replay, which IS the final screen.
   * A visible exit flushes and stays in sync, as before.
   */
  exited(): Uint8Array[] {
    if (this.awaitingSnapshot) {
      // Unreachable by wire order (the snapshot lands before exited), but
      // if it ever happens the safe answer is a resync, not a stale grid.
      this.awaitingSnapshot = false;
      this.desynced = true;
    }
    if (this.desynced) return [];
    const flush = this.chunks;
    this.discard();
    this.desynced = this.parked;
    return flush;
  }

  /** The entry is being shown again. */
  adopt(): AdoptDirective {
    this.parked = false;
    // An in-flight snapshot now lands as a direct write either way; keeping
    // the flag would misroute a live chunk on a later re-park.
    this.awaitingSnapshot = false;
    if (this.desynced) return { flush: [], resync: true };
    const flush = this.chunks;
    this.discard();
    return { flush, resync: false };
  }

  /** A resync was actually issued for a desynced entry: clear the latch. */
  resyncIssued(): void {
    this.desynced = false;
  }

  private discard(): void {
    this.chunks = [];
    this.bytes = 0;
  }
}

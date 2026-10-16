/**
 * The one divider/handle resize drag. Every splitter in the workbench (pane
 * splits, the Finder preview, the split edit|preview, the Mastermind panel,
 * the rail) had grown its own copy of the same loop; this is that loop once.
 *
 * What it owns: pointer capture on the handle (falls back to the window
 * listeners when capture is unavailable), a pointerId filter, an optional
 * movement threshold, an optional rAF throttle (one `apply` per frame, so a
 * drag never forces layout more than once a frame), Escape and pointercancel
 * restoring the value the drag started from, and teardown of every listener.
 *
 * What the caller owns: the value and its clamp (`compute` maps a pointer
 * position to the new value, so min/max/total live with the site), the
 * "resizing" state, and any side effect at the start or end (terminal refit
 * holds, persisting the width). A pending frame is DROPPED at the end, not
 * flushed: the last `apply` is the last frame the user saw.
 *
 * `resizeKeydown` is the keyboard half for a focusable `role="separator"`:
 * arrows step, Home/End jump to the bounds.
 */

export interface ResizeDragEnv {
  /** Where move/up/cancel/keydown listeners go (default `window`). */
  target: Pick<EventTarget, "addEventListener" | "removeEventListener">;
  requestFrame(callback: () => void): number;
  cancelFrame(id: number): void;
}

export interface ResizeDragOptions {
  /** Which pointer coordinate drives the value: clientX or clientY. */
  axis: "x" | "y";
  /** The value at drag start; Escape (and, by default, pointercancel) put it back. */
  initial: number;
  /** Map the pointer position (and the position the drag started at) to the
   *  new value, clamped by the caller. `null` skips this move. */
  compute(pos: number, start: number): number | null;
  /** Show a value. Also called with `initial` when the drag is cancelled. */
  apply(value: number): void;
  /** After teardown (and after the restore, when cancelled). */
  onEnd?(cancelled: boolean): void;
  /** Pixels the pointer must travel before the first move applies (default 0). */
  threshold?: number;
  /** Throttle `apply` to one per animation frame (default true). */
  frame?: boolean;
  /** Escape cancels and restores `initial` (default true). */
  escape?: boolean;
  /** pointercancel restores `initial` (default true); false ends the drag as committed. */
  cancelRestores?: boolean;
  /** End the drag (committed) when the handle loses pointer capture (default false). */
  endOnLostCapture?: boolean;
  /** The element that captures the pointer (default `e.currentTarget`). */
  handle?: HTMLElement | null;
  /** Test seam: listener target and frame scheduler. */
  env?: Partial<ResizeDragEnv>;
}

export interface ResizeDrag {
  /** End as committed (the current value stays). */
  end(): void;
  /** End and restore `initial`. */
  cancel(): void;
}

/** Start a resize drag from a pointerdown. The caller checks the button and
 *  any precondition first; this calls `preventDefault` on the event. */
export function startResizeDrag(e: PointerEvent, options: ResizeDragOptions): ResizeDrag {
  // `window` and the frame functions are read only when not injected (node tests).
  const env: ResizeDragEnv = {
    target: options.env?.target ?? window,
    requestFrame: options.env?.requestFrame ?? ((callback) => requestAnimationFrame(callback)),
    cancelFrame: options.env?.cancelFrame ?? ((id) => cancelAnimationFrame(id)),
  };
  const { axis, initial, compute, apply, onEnd } = options;
  const threshold = options.threshold ?? 0;
  const frame = options.frame ?? true;
  const escape = options.escape ?? true;
  const cancelRestores = options.cancelRestores ?? true;
  const handle = options.handle !== undefined ? options.handle : (e.currentTarget as HTMLElement | null);
  const pointerId = e.pointerId;
  const at = (ev: PointerEvent) => (axis === "x" ? ev.clientX : ev.clientY);

  e.preventDefault();
  try {
    handle?.setPointerCapture(pointerId);
  } catch {
    // capture unavailable; the window-level listeners still track the drag
  }

  const start = at(e);
  let last = start;
  let moved = threshold <= 0;
  let raf = 0;
  let done = false;

  const run = () => {
    raf = 0;
    const value = compute(last, start);
    if (value !== null) apply(value);
  };

  const onMove = (ev: PointerEvent) => {
    if (ev.pointerId !== pointerId) return;
    const pos = at(ev);
    if (!moved) {
      if (Math.abs(pos - start) < threshold) return;
      moved = true;
    }
    last = pos;
    if (!frame) run();
    else if (raf === 0) raf = env.requestFrame(run);
  };

  const finish = (cancelled: boolean) => {
    if (done) return;
    done = true;
    env.target.removeEventListener("pointermove", onMove as EventListener);
    env.target.removeEventListener("pointerup", onUp as EventListener);
    env.target.removeEventListener("pointercancel", onCancel as EventListener);
    if (escape) env.target.removeEventListener("keydown", onKey as EventListener, true);
    if (options.endOnLostCapture) handle?.removeEventListener("lostpointercapture", onLost as EventListener);
    if (raf !== 0) env.cancelFrame(raf);
    raf = 0;
    if (cancelled) apply(initial);
    onEnd?.(cancelled);
  };

  const onUp = (ev: PointerEvent) => {
    if (ev.pointerId === pointerId) finish(false);
  };
  const onCancel = (ev: PointerEvent) => {
    if (ev.pointerId === pointerId) finish(cancelRestores);
  };
  const onLost = (ev: PointerEvent) => {
    if (ev.pointerId === pointerId) finish(false);
  };
  const onKey = (ev: KeyboardEvent) => {
    if (ev.key !== "Escape") return;
    ev.preventDefault();
    ev.stopPropagation();
    finish(true);
  };

  env.target.addEventListener("pointermove", onMove as EventListener);
  env.target.addEventListener("pointerup", onUp as EventListener);
  env.target.addEventListener("pointercancel", onCancel as EventListener);
  if (escape) env.target.addEventListener("keydown", onKey as EventListener, true);
  if (options.endOnLostCapture) handle?.addEventListener("lostpointercapture", onLost as EventListener);

  return { end: () => finish(false), cancel: () => finish(true) };
}

export interface ResizeKeyOptions {
  /** "x": ArrowLeft/ArrowRight; "y": ArrowUp/ArrowDown. */
  axis: "x" | "y";
  value: number;
  min: number;
  max: number;
  /** Change per arrow press. */
  step: number;
  apply(value: number): void;
  /** ArrowLeft/ArrowUp grow the value (the sized pane sits after the handle). */
  invert?: boolean;
}

/** Keyboard resize on a focusable separator. Returns true (and stops the
 *  event) when the key was a resize key; Escape is left to the drag. */
export function resizeKeydown(e: KeyboardEvent, o: ResizeKeyOptions): boolean {
  const back = o.axis === "x" ? "ArrowLeft" : "ArrowUp";
  const forward = o.axis === "x" ? "ArrowRight" : "ArrowDown";
  const clamp = (v: number) => Math.max(o.min, Math.min(o.max, v));
  if (e.key === back || e.key === forward) {
    const grow = (e.key === forward) !== (o.invert ?? false);
    o.apply(clamp(o.value + (grow ? o.step : -o.step)));
  } else if (e.key === "Home") o.apply(clamp(o.min));
  else if (e.key === "End") o.apply(clamp(o.max));
  else return false;
  e.preventDefault();
  e.stopPropagation();
  return true;
}

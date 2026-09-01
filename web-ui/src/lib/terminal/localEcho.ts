/**
 * Predictive local echo for remote shells: on a high-RTT link (a tunneled
 * login node measured at ~170 ms — docs/perf-remote-plan.md) every typed
 * character takes a full round trip to appear. Nothing server-side can beat
 * that floor, so mask it the mosh/VS Code way: paint the predicted character
 * immediately and reconcile when the real echo arrives.
 *
 * Safety model — the prediction is a GHOST, never buffer state: predicted
 * characters render as a translucent DOM overlay positioned after the
 * cursor; the xterm buffer only ever contains what the server sent. A wrong
 * prediction therefore needs no rollback machinery — the ghost is cleared
 * and the truth is already on screen. The worst failure is cosmetic (a ghost
 * character that briefly showed and vanished), never a corrupted grid.
 *
 * Arming is deliberately narrow (the `armed` callback + local checks):
 * remote window, measured RTT above LOCAL_ECHO_MIN_RTT_MS, the shell at its
 * OSC 133 prompt (never inside a running command — password reads don't
 * echo; never agent TUIs, whose phase stays "unknown"), primary screen only,
 * and viewport scrolled to the bottom. Printable ASCII only: no prediction
 * for backspace, arrows, or anything the line editor might interpret.
 */

import type { Terminal } from "@xterm/xterm";

/** Below this measured link RTT the ghost costs more than it hides. */
export const LOCAL_ECHO_MIN_RTT_MS = 45;

/** Ghosts pending longer than this are cleared: the echo is never coming
 *  (a non-echoing read, a wedged link — either way, stop predicting). */
const GHOST_TTL_MS = 3_000;

/** More pending than this means a paste torrent — show nothing rather than
 *  a wall of translucent text the echo will immediately repaint. */
const GHOST_MAX_CHARS = 64;

const ESC = 0x1b;
const DEL = 0x7f;

/**
 * The reconciliation core, pure for unit tests: a queue of predicted
 * characters vs the raw output stream. Echoed printables consume ghosts
 * one-for-one; anything that can rewrite the line (control bytes, escape
 * sequences — a zsh highlight repaint, a prompt redraw, CR/LF) clears every
 * ghost, because the real render it belongs to is arriving in the same
 * chunk. Over-clearing is always safe: truth is already painted.
 */
export class EchoMatcher {
  private pending = 0;

  get count(): number {
    return this.pending;
  }

  /** Predict `n` more characters (already validated printable). */
  predict(n: number): void {
    this.pending += n;
  }

  /**
   * Reconcile one output chunk. Returns how many ghosts remain. Bytes
   * 0x20–0x7e and UTF-8 lead/continuation bytes count as echoed printables;
   * any control byte or ESC clears everything.
   */
  output(data: Uint8Array): number {
    if (this.pending === 0) return 0;
    let printable = 0;
    for (const b of data) {
      if (b === ESC || b === DEL || b < 0x20) {
        this.pending = 0;
        return 0;
      }
      // Count UTF-8 lead bytes and ASCII, not continuations — one ghost per
      // echoed character, approximately. Drift over-clears below, never
      // under-clears into a stale ghost.
      if (b < 0x80 || b >= 0xc0) printable += 1;
    }
    if (printable >= this.pending) {
      this.pending = 0;
    } else {
      this.pending -= printable;
    }
    return this.pending;
  }

  clear(): void {
    this.pending = 0;
  }
}

/** True when `data` is entirely printable ASCII (predictable input). */
export function isPredictable(data: string): boolean {
  if (data.length === 0 || data.length > GHOST_MAX_CHARS) return false;
  for (let i = 0; i < data.length; i++) {
    const c = data.charCodeAt(i);
    if (c < 0x20 || c > 0x7e) return false;
  }
  return true;
}

export interface LocalEcho {
  /** Raw term.onData input, before it goes to the socket. */
  onInput(data: string): void;
  /** Raw socket output, before term.write. */
  onOutput(data: Uint8Array): void;
  /** Drop all ghosts (reset, park, resize — any grid discontinuity). */
  clear(): void;
  dispose(): void;
}

export function createLocalEcho(
  term: Terminal,
  armed: () => boolean,
): LocalEcho {
  const matcher = new EchoMatcher();
  let ghost: HTMLSpanElement | null = null;
  let text = "";
  let ttl: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;

  function el(): HTMLSpanElement | null {
    if (ghost !== null && ghost.isConnected) return ghost;
    // xterm's .xterm-screen hosts the render canvases at (0,0); the ghost
    // overlays them. Re-created lazily — a theme/renderer swap can rebuild
    // the screen element under us.
    const screen = term.element?.querySelector(".xterm-screen");
    if (!(screen instanceof HTMLElement)) return null;
    ghost = document.createElement("span");
    ghost.className = "term-ghost";
    ghost.setAttribute("aria-hidden", "true");
    screen.appendChild(ghost);
    return ghost;
  }

  function hide(): void {
    text = "";
    matcher.clear();
    if (ttl !== null) {
      clearTimeout(ttl);
      ttl = null;
    }
    if (ghost !== null) ghost.style.display = "none";
  }

  function render(): void {
    const span = el();
    if (span === null) return;
    const buf = term.buffer.active;
    // Only at the bottom of the primary screen — a scrolled-back or
    // alt-screen view has no meaningful "after the cursor".
    if (buf.type !== "normal" || buf.viewportY !== buf.baseY || text === "") {
      span.style.display = "none";
      return;
    }
    const screen = span.parentElement;
    if (screen === null || term.cols === 0 || term.rows === 0) return;
    const cellW = screen.clientWidth / term.cols;
    const cellH = screen.clientHeight / term.rows;
    // Clip to the row: predictions past the right edge wait invisibly (the
    // real echo will wrap correctly; a ghost must not guess at wrapping).
    const maxChars = Math.max(0, term.cols - buf.cursorX);
    span.textContent = text.slice(0, maxChars);
    span.style.cssText =
      `display:block;position:absolute;pointer-events:none;z-index:20;` +
      `left:${buf.cursorX * cellW}px;top:${buf.cursorY * cellH}px;` +
      `height:${cellH}px;line-height:${cellH}px;white-space:pre;` +
      `font-family:${term.options.fontFamily ?? "monospace"};` +
      `font-size:${term.options.fontSize ?? 14}px;` +
      `color:${term.options.theme?.foreground ?? "inherit"};opacity:0.55;`;
  }

  function armTtl(): void {
    if (ttl !== null) clearTimeout(ttl);
    ttl = setTimeout(hide, GHOST_TTL_MS);
  }

  return {
    onInput(data: string): void {
      if (disposed) return;
      if (!isPredictable(data)) {
        // Control input (Enter, backspace, arrows, a multi-line paste) can
        // rewrite the line arbitrarily — drop every ghost.
        hide();
        return;
      }
      if (!armed() || term.buffer.active.type !== "normal") return;
      if (matcher.count + data.length > GHOST_MAX_CHARS) {
        hide();
        return;
      }
      matcher.predict(data.length);
      text += data;
      render();
      armTtl();
    },
    onOutput(data: Uint8Array): void {
      if (disposed || matcher.count === 0) return;
      const remaining = matcher.output(data);
      if (remaining === 0) {
        hide();
      } else {
        text = text.slice(text.length - remaining);
        // Defer past this chunk's term.write so the ghost re-anchors at the
        // advanced cursor, not the stale one.
        requestAnimationFrame(() => {
          if (!disposed && matcher.count > 0) render();
        });
        armTtl();
      }
    },
    clear: hide,
    dispose(): void {
      disposed = true;
      hide();
      ghost?.remove();
      ghost = null;
    },
  };
}

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

/**
 * No prediction for this long after any non-predictable input (Enter,
 * Ctrl-C, arrows): the OSC 133 phase gate lags the shell by at least one
 * link RTT plus the events-bus cadence, so the keystrokes right after an
 * Enter land while the gate still reads the PRE-command phase — including a
 * password prompt's non-echoing read. The cooldown covers that stale window;
 * fresh phase evidence (the gate itself) takes over after it.
 */
const POST_CONTROL_COOLDOWN_MS = 1_200;

/** More pending than this means a paste torrent — show nothing rather than
 *  a wall of translucent text the echo will immediately repaint. */
const GHOST_MAX_CHARS = 64;

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
   * Reconcile one output chunk. Returns how many ghosts remain. Any C0
   * control byte (ESC included), DEL, or a byte in the C1 range clears
   * everything — a line repaint is arriving, in 7- or 8-bit form. C1
   * overlaps UTF-8 continuation bytes (0x80–0x9f), so some multi-byte
   * characters also clear; over-clearing is always the safe direction.
   */
  output(data: Uint8Array): number {
    if (this.pending === 0) return 0;
    let printable = 0;
    for (let i = 0; i < data.length; i++) {
      const b = data[i];
      if (b < 0x20 || b === DEL || (b >= 0x80 && b <= 0x9f)) {
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
  /** Raw term.onData input (after it went to the socket). */
  onInput(data: string): void;
  /** Raw socket output, called before the corresponding term.write. */
  onOutput(data: Uint8Array): void;
  /** Whether any ghosts pend — gates the write-callback rerender. */
  hasGhosts(): boolean;
  /** Re-anchor surviving ghosts; call from term.write's completion callback
   *  (xterm parses asynchronously — only then has the cursor advanced). */
  rerender(): void;
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
  let coolUntil = 0;
  let disposed = false;

  function el(): HTMLSpanElement | null {
    if (ghost !== null && ghost.isConnected) return ghost;
    // xterm's .xterm-screen hosts the render canvases at (0,0); the ghost
    // overlays them. Re-created lazily — a theme/renderer swap can rebuild
    // the screen element under us.
    const screen = term.element?.querySelector(".xterm-screen");
    if (!(screen instanceof HTMLElement)) return null;
    ghost = document.createElement("span");
    // Palette + static layout live on the .term-ghost rule in app.css
    // (theme tokens track live theme flips there); only geometry and the
    // terminal's font land inline, as individual properties — never a
    // cssText string, which would let the free-text fontFamily setting
    // inject arbitrary declarations.
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
    const w = screen.clientWidth;
    const h = screen.clientHeight;
    if (w === 0 || h === 0) {
      // Mid-layout (a dragged pane, a not-yet-fitted host): a degenerate
      // 0-size cell would pin the ghost at the origin.
      span.style.display = "none";
      return;
    }
    const cellW = w / term.cols;
    const cellH = h / term.rows;
    // Clip to the row: predictions past the right edge wait invisibly (the
    // real echo will wrap correctly; a ghost must not guess at wrapping).
    const maxChars = Math.max(0, term.cols - buf.cursorX);
    span.textContent = text.slice(0, maxChars);
    const st = span.style;
    st.display = "block";
    st.left = `${buf.cursorX * cellW}px`;
    st.top = `${buf.cursorY * cellH}px`;
    st.height = `${cellH}px`;
    st.lineHeight = `${cellH}px`;
    st.fontFamily = term.options.fontFamily ?? "monospace";
    st.fontSize = `${term.options.fontSize ?? 14}px`;
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
        // rewrite the line arbitrarily — drop every ghost, and hold off
        // predicting while the phase gate is provably stale (see
        // POST_CONTROL_COOLDOWN_MS — the sudo-password window).
        coolUntil = Date.now() + POST_CONTROL_COOLDOWN_MS;
        hide();
        return;
      }
      if (!armed() || term.buffer.active.type !== "normal" || Date.now() < coolUntil) {
        // Not arming is also a state change: predictions made under the old
        // state no longer correspond to anything — never leave them shown.
        hide();
        return;
      }
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
        // The re-anchor happens in term.write's completion callback (the
        // pool's rerender call) — xterm parses asynchronously, so only then
        // has the cursor actually advanced.
        armTtl();
      }
    },
    hasGhosts(): boolean {
      return matcher.count > 0;
    },
    rerender(): void {
      if (!disposed && matcher.count > 0) render();
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

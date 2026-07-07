/**
 * The "which-key" discovery state: true while the user is HOLDING the app's
 * base modifier (⌘ on macOS, Ctrl+Shift elsewhere) without yet committing to a
 * chord, so surfaces can fade in what the next key would do (the ⌘1–9 digit
 * badges on rail rows). Deliberately quiet — it arms only after a short hold,
 * so a fast chord (⌘1 struck and released) never flashes anything, and it
 * clears the instant a real key lands or the modifier lifts.
 *
 * This is pure teaching chrome: it drives opacity only, never layout or focus,
 * so it can never get in the way of the very chords it advertises.
 */

import { isMac } from "./keys";

/** Hold this long before the hints arm — long enough that executing a chord
 *  outruns it, short enough that a pause to think reveals them. */
const ARM_DELAY_MS = 380;

let active = $state(false);
let timer: ReturnType<typeof setTimeout> | null = null;

/** True while the discovery hints should be shown. */
export function hintsActive(): boolean {
  return active;
}

/** Just the app base modifier is down (no second layer, no stray modifier) —
 *  the state that, held, means "I'm about to chord but haven't picked a key". */
function baseModifierOnly(e: KeyboardEvent): boolean {
  if (isMac) return e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey;
  return e.ctrlKey && e.shiftKey && !e.metaKey && !e.altKey;
}

/** Modifier keys never count as "committing" to a chord. */
function isModifierKey(key: string): boolean {
  return key === "Meta" || key === "Control" || key === "Shift" || key === "Alt";
}

function disarm(): void {
  if (timer !== null) {
    clearTimeout(timer);
    timer = null;
  }
  active = false;
}

/**
 * Attach the global listeners; returns a teardown. Idempotent per call site
 * (App mounts it once). Capture phase so a chord handler's stopPropagation
 * elsewhere can't starve us of the keyup that clears the hints.
 */
export function initChordHints(): () => void {
  const onKeydown = (e: KeyboardEvent): void => {
    // A real (non-modifier) key means the user committed — clear immediately,
    // whether or not the hints had armed.
    if (!isModifierKey(e.key)) {
      if (active || timer !== null) disarm();
      return;
    }
    if (!baseModifierOnly(e)) {
      // A second layer (Shift/Alt) joined the modifier — a different chord
      // family, not the digit layer these hints teach.
      if (active || timer !== null) disarm();
      return;
    }
    if (active || timer !== null) return; // already armed/pending (auto-repeat)
    timer = setTimeout(() => {
      timer = null;
      active = true;
    }, ARM_DELAY_MS);
  };

  const onKeyup = (): void => {
    // Any modifier lifting can only weaken the base-modifier state; re-check
    // is unnecessary since keyup carries the post-release modifier flags.
    disarm();
  };

  const onBlur = (): void => disarm();

  window.addEventListener("keydown", onKeydown, true);
  window.addEventListener("keyup", onKeyup, true);
  window.addEventListener("blur", onBlur);
  document.addEventListener("visibilitychange", onBlur);

  return () => {
    disarm();
    window.removeEventListener("keydown", onKeydown, true);
    window.removeEventListener("keyup", onKeyup, true);
    window.removeEventListener("blur", onBlur);
    document.removeEventListener("visibilitychange", onBlur);
  };
}

/**
 * The "which-key" discovery state: true while the user is HOLDING the app's
 * base modifier (⌘ on macOS, Ctrl+Shift elsewhere) without yet committing to a
 * chord, so panes can reveal their numbered focus/move shortcuts.
 * Deliberately quiet — it arms only after a short hold,
 * so a fast chord (⌘1 struck and released) never flashes anything, and it
 * clears the instant a real key lands or the modifier lifts.
 *
 * This is pure teaching chrome: it drives opacity only, never layout or focus,
 * so it can never get in the way of the very chords it advertises.
 */

import { resolveMod, resolvePaneMoveMod } from "./keys";
import { modifierSetting } from "./keybindings";

/** Hold this long before the hints arm — long enough that executing a chord
 *  outruns it, short enough that a pause to think reveals them. */
const ARM_DELAY_MS = 380;

let active = $state(false);
let layer = $state<"base" | "move" | null>(null);
let timer: ReturnType<typeof setTimeout> | null = null;

/** True while the discovery hints should be shown. */
export function hintsActive(): boolean {
  return active && layer === "base";
}

export function paneHintsActive(): boolean {
  return active && layer === "move";
}

/** Match the configured modifier and its move layer, without stray modifiers. */
function modifierLayer(e: KeyboardEvent): "base" | "move" | null {
  const setting = modifierSetting();
  const matches = (m: ReturnType<typeof resolveMod>) =>
    e.metaKey === m.meta && e.ctrlKey === m.ctrl && e.altKey === m.alt && e.shiftKey === m.shift;
  if (matches(resolveMod(setting))) return "base";
  if (matches(resolvePaneMoveMod(setting))) return "move";
  return null;
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
  layer = null;
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
    const next = modifierLayer(e);
    if (next === null) {
      if (active || timer !== null) disarm();
      return;
    }
    if (layer === next && (active || timer !== null)) return;
    // Adding the move modifier after discovery should not flash the backdrop
    // away and make the user wait through a second hold delay.
    if (active) {
      layer = next;
      return;
    }
    disarm();
    layer = next;
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

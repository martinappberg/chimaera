/**
 * Live keybindings: the keys.ts action registry joined with the settings
 * store. Components call `keyHint()` for tooltip chords and App.svelte calls
 * `matchAction()` per keydown — both read the reactive settings, so a rebind
 * or modifier switch updates every surface immediately.
 */

import {
  ACTIONS,
  displayChord,
  matchChord,
  modLabel as modLabelFor,
  parseChord,
  type ActionId,
  type ArrowDir,
  type ModifierSetting,
} from "./keys";
import { getSetting } from "../settings/store.svelte";

/** The active base-modifier setting. */
export function modifierSetting(): ModifierSetting {
  return getSetting("keys.modifier");
}

/** The effective chord string for an action ("" = disabled). */
export function effectiveChord(id: ActionId): string {
  return getSetting(`keys.${id}` as "keys.closeView");
}

/** Tooltip label for an action's current chord ("" when disabled). */
export function keyHint(id: ActionId): string {
  const chord = effectiveChord(id);
  return chord === "" ? "" : displayChord(chord, modifierSetting());
}

/** Tooltip suffix like " (⌘E)" — empty when the action is unbound. */
export function keyHintSuffix(id: ActionId): string {
  const hint = keyHint(id);
  return hint === "" ? "" : ` (${hint})`;
}

/** Inline label for the base modifier ("⌘" / "Ctrl+Shift+"). */
export function activeModLabel(): string {
  return modLabelFor(modifierSetting());
}

export interface ActionHit {
  id: ActionId;
  /** Which arrow fired, for arrow-set actions. */
  dir: ArrowDir | null;
}

/**
 * Match a keydown against every bound action, registry order (first hit
 * wins on duplicate chords). Parsing per event is a dozen string splits —
 * nothing worth caching against settings churn.
 */
export function matchAction(e: KeyboardEvent): ActionHit | null {
  const setting = modifierSetting();
  for (const action of ACTIONS) {
    const chord = effectiveChord(action.id as ActionId);
    if (chord === "") continue;
    const parsed = parseChord(chord, setting);
    if (parsed === null) continue;
    const hit = matchChord(e, parsed);
    if (hit === null) continue;
    return { id: action.id as ActionId, dir: hit === "hit" ? null : hit };
  }
  return null;
}

// --- capture handshake --------------------------------------------------------

/** True while a settings row is recording a chord — App's global handler
 *  stands down so the captured press doesn't also fire an action. */
let capturing = false;

export function setCapturing(v: boolean): void {
  capturing = v;
}

export function isCapturing(): boolean {
  return capturing;
}

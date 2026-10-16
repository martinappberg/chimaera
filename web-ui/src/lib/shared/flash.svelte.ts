/**
 * The window's transient feedback chip (bottom center, auto-fades): the
 * outcome of an action that would otherwise fail silently — a refused
 * detach, a blocked popup, a cross-window move that didn't complete, a
 * failed link. One chip per window: a new message replaces the shown one and
 * restarts its timer. App renders `flash.text`; anything may call
 * `showFlash`.
 */

/** How long a message stays up after its last `showFlash`. */
export const FLASH_MS = 3200;

let text = $state<string | null>(null);
let timer: ReturnType<typeof setTimeout> | null = null;

/** The chip's current message (null = hidden). Read-only outside this module. */
export const flash = {
  get text(): string | null {
    return text;
  },
};

export function showFlash(message: string): void {
  text = message;
  if (timer !== null) clearTimeout(timer);
  timer = setTimeout(() => (text = null), FLASH_MS);
}

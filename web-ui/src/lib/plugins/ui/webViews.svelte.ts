/**
 * Plugin web views in a window (docs/design/rich-views-plan.md, "Resource
 * limits"): each is a page of a plugin's own, often a WebGL one, so a window
 * keeps at most `WEB_LIVE_MAX` of them running — the ones most recently on
 * screen or worked in. One that drops out of that set closes its page and
 * shows a still (or a quiet "paused"); shown again, it loads again. A view
 * hidden for `PARKED_MS` (its tab in the background) closes too.
 */

import { untrack } from "svelte";

/** Live web views per window. */
export const WEB_LIVE_MAX = 4;
/** A hidden web view keeps its page this long, then closes it. */
export const PARKED_MS = 2 * 60 * 1000;

class WebViews {
  /** Views most recently shown or worked in, newest first. */
  private recent = $state<string[]>([]);

  /** The view came on screen, or the person worked in it. Called from a
   *  node's effect, so it reads the list untracked: a tracked read made two
   *  views on screen at once re-run each other's effect without end
   *  (`effect_update_depth_exceeded`), each moving itself to the front. */
  seen(id: string): void {
    const recent = untrack(() => this.recent);
    if (recent[0] === id) return;
    this.recent = [id, ...recent.filter((r) => r !== id)];
  }

  /** The view went (unmounted, or closed while parked). */
  drop(id: string): void {
    const recent = untrack(() => this.recent);
    if (recent.includes(id)) this.recent = recent.filter((r) => r !== id);
  }

  /** Whether the view may run its page. */
  live(id: string): boolean {
    const at = this.recent.indexOf(id);
    return at >= 0 && at < WEB_LIVE_MAX;
  }
}

export const webViews = new WebViews();

/**
 * HTML embed cards in the app: which cards hold a live page view, which one
 * the user is working in, and the stills that stand in for the rest.
 *
 * Every live card is a web process of its own (`shared/paneView.ts`), so a
 * window keeps at most `LIVE_MAX` of them: the cards most recently on
 * screen. A card that drops out of that set closes its view and shows its
 * still; scrolled back, it loads again (the bytes come from the HTTP cache).
 * A still is the view's own snapshot, kept per URL (a `/raw` ticket names
 * one version of one file) and width (a hover preview's narrow card of the
 * same file must not stretch over a wide one), so it also covers a card
 * while its document scrolls, while a menu sits over it, and while it
 * reloads.
 *
 * One card at a time is *active*: the user clicked into it, and the wheel
 * scrolls the page inside it. Every other live card is passive: the wheel
 * over it scrolls the document around it.
 */

/** Live views per window. */
export const LIVE_MAX = 3;
/** Stills kept per window (about 0.3–1 MB each as data URLs). */
const STILLS_MAX = 12;

/** A page as its card last showed it: a PNG at the view's CSS size. */
export interface Still {
  src: string;
  width: number;
  height: number;
}

const stillKey = (url: string, width: number): string => `${Math.round(width)} ${url}`;

class LiveViews {
  /** Cards most recently on screen, newest first (ids only). */
  private recent = $state<string[]>([]);
  /** The card the user is working in. */
  active = $state<string | null>(null);
  /** Width + URL → its page's still, oldest first. */
  private stills = $state<Map<string, Still>>(new Map());

  /** The card came on screen. */
  seen(id: string): void {
    if (this.recent[0] === id) return;
    this.recent = [id, ...this.recent.filter((r) => r !== id)];
  }

  /** The card went (unmounted). */
  drop(id: string): void {
    if (this.recent.includes(id)) this.recent = this.recent.filter((r) => r !== id);
    if (this.active === id) this.active = null;
  }

  /** Whether the card may hold a live view. */
  live(id: string): boolean {
    const at = this.recent.indexOf(id);
    return at >= 0 && at < LIVE_MAX;
  }

  activate(id: string | null): void {
    if (this.active !== id) this.active = id;
  }

  /** The still of `url` taken in a card `width` CSS px wide. */
  still(url: string | null, width: number): Still | null {
    return url === null ? null : (this.stills.get(stillKey(url, width)) ?? null);
  }

  keepStill(url: string, still: Still): void {
    const key = stillKey(url, still.width);
    const next = new Map(this.stills);
    next.delete(key);
    next.set(key, still);
    while (next.size > STILLS_MAX) {
      const oldest = next.keys().next().value;
      if (oldest === undefined) break;
      next.delete(oldest);
    }
    this.stills = next;
  }
}

export const liveViews = new LiveViews();

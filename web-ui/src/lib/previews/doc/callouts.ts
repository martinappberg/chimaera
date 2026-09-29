/**
 * Callouts: GitHub's alerts (`> [!NOTE]`) and Obsidian's callouts
 * (`> [!example]- Title`) read as one construct — a quote whose first line
 * opens with `[!type]`, an optional fold mark (`-` starts folded, `+`
 * open) and an optional title (markdown) after it.
 *
 * The type picks a look. GitHub's five keep GitHub's colors and glyphs
 * (so `important` is purple and `caution` red, not Obsidian's aliases);
 * Obsidian's other types and aliases get Obsidian's families; an unknown
 * type draws as a note under its own name, as Obsidian draws it. Only the
 * five GitHub types render on the daemon's comrak fallback — the rest are
 * read on the client, like wikilinks (the parity corpus records it).
 *
 * The look lives here once — tint token and glyph per family — for every
 * stylesheet that draws a callout: the reading view and the exported page.
 */

/** A callout's family: the class suffix and the look. */
export type CalloutLook =
  | "note"
  | "abstract"
  | "info"
  | "todo"
  | "tip"
  | "success"
  | "question"
  | "warning"
  | "failure"
  | "danger"
  | "bug"
  | "example"
  | "quote"
  | "important"
  | "caution";

/** The marker at the head of a quote's first line: the `>` and one space,
 *  as comrak reads GitHub's (so a GitHub alert renders the same on the
 *  fallback), then `[!type]`, the fold mark and the space before a title. */
export const CALLOUT_MARKER = /^> \[!([\p{L}\p{N}_-]+)\]([+-]?)[ \t]*/u;

/** Obsidian's aliases (`important` and `caution` are GitHub's own). */
const ALIASES: Readonly<Record<string, CalloutLook>> = {
  summary: "abstract",
  tldr: "abstract",
  hint: "tip",
  check: "success",
  done: "success",
  help: "question",
  faq: "question",
  attention: "warning",
  fail: "failure",
  missing: "failure",
  error: "danger",
  cite: "quote",
};

/** Glyph paths (24-unit, stroked), drawn as a mask in the title's color. */
const ICONS: Readonly<Record<CalloutLook, string>> = {
  note: "<circle cx='12' cy='12' r='9'/><path d='M12 8h.01M11 12h1v4h1'/>",
  abstract: "<path d='M9 5H7a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2h-2M9 3h6v4H9zM9 12h6M9 16h4'/>",
  info: "<circle cx='12' cy='12' r='9'/><path d='M12 8h.01M11 12h1v4h1'/>",
  todo: "<circle cx='12' cy='12' r='9'/><path d='M9 12l2 2l4-4'/>",
  tip: "<path d='M3 12h1m8-9v1m8 8h1M5.6 5.6l.7.7m12.1-.7-.7.7M9 16a5 5 0 1 1 6 0a3.5 3.5 0 0 0-1 3a2 2 0 0 1-4 0a3.5 3.5 0 0 0-1-3M9.7 17h4.6'/>",
  success: "<path d='M5 12l5 5L20 7'/>",
  question: "<circle cx='12' cy='12' r='9'/><path d='M12 17h.01M9.5 9.2a2.6 2.6 0 0 1 5 .8c0 1.8-2.5 2.2-2.5 3.7'/>",
  warning: "<path d='M12 9v4M10.4 3.6L2.3 17.1a1.9 1.9 0 0 0 1.6 2.9h16.2a1.9 1.9 0 0 0 1.6-2.9L13.6 3.6a1.9 1.9 0 0 0-3.2 0zM12 16h.01'/>",
  failure: "<path d='M18 6L6 18M6 6l12 12'/>",
  danger: "<path d='M13 3L4 14h7l-1 7l9-11h-7l1-7z'/>",
  bug: "<path d='M9 9V8a3 3 0 0 1 6 0v1M8 9h8a1 1 0 0 1 1 1v3a5 5 0 0 1-10 0v-3a1 1 0 0 1 1-1zM3 13h4M17 13h4M12 20v-6M4 19l3.4-2M20 19l-3.4-2M4 7l3.8 2.4M20 7l-3.8 2.4'/>",
  example: "<path d='M9 6h11M9 12h11M9 18h11M5 6v.01M5 12v.01M5 18v.01'/>",
  quote: "<path d='M10 11H6a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h3a1 1 0 0 1 1 1v6c0 2.7-1.3 4.3-4 5M19 11h-4a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h3a1 1 0 0 1 1 1v6c0 2.7-1.3 4.3-4 5'/>",
  important: "<path d='M18 4a3 3 0 0 1 3 3v8a3 3 0 0 1-3 3h-5l-5 3v-3H6a3 3 0 0 1-3-3V7a3 3 0 0 1 3-3zM12 8v3M12 14v.01'/>",
  caution: "<path d='M12.8 2.6l8.6 8.6a1.1 1.1 0 0 1 0 1.6l-8.6 8.6a1.1 1.1 0 0 1-1.6 0l-8.6-8.6a1.1 1.1 0 0 1 0-1.6l8.6-8.6a1.1 1.1 0 0 1 1.6 0zM12 8v4M12 16h.01'/>",
};

/** Semantic theme tokens, so every curated theme restyles them. */
const TINTS: Readonly<Record<CalloutLook, string>> = {
  note: "var(--syn-func)",
  abstract: "var(--syn-type)",
  info: "var(--syn-func)",
  todo: "var(--syn-func)",
  tip: "var(--syn-string)",
  success: "var(--syn-string)",
  question: "var(--syn-number)",
  warning: "var(--warn)",
  failure: "var(--err)",
  danger: "var(--err)",
  bug: "var(--err)",
  example: "var(--rate)",
  quote: "var(--muted)",
  important: "var(--rate)",
  caution: "var(--err)",
};

export const CALLOUT_LOOKS = Object.keys(TINTS) as CalloutLook[];

/** The family a written type draws as (case-insensitive). */
export function calloutLook(type: string): CalloutLook {
  const t = type.toLowerCase();
  // Own keys only: a type named like an Object.prototype member is a note.
  if (Object.hasOwn(TINTS, t)) return t as CalloutLook;
  return Object.hasOwn(ALIASES, t) ? ALIASES[t] : "note";
}

/** The title a callout without one shows: its type as written, capitalized
 *  (`[!faq]` → "Faq", `[!NOTE]` → "Note"), as both GitHub and Obsidian do. */
export function defaultCalloutTitle(type: string): string {
  const t = type.toLowerCase();
  return t.charAt(0).toUpperCase() + t.slice(1);
}

/** A glyph as a CSS `url()`: the stroked path in a 24-unit SVG. */
function maskUrl(paths: string): string {
  const svg =
    "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' " +
    `stroke-width='2' stroke-linecap='round' stroke-linejoin='round'>${paths}</svg>`;
  return `url("data:image/svg+xml,${encodeURIComponent(svg)}")`;
}

/** Every family's tint and glyph, as rules under `scope` (a selector
 *  prefix, `""` for none): the base rule first (a note's look and the fold
 *  chevron), so a family's rule, equally specific and later, wins. */
export function calloutRules(scope: string): string {
  const pre = scope === "" ? "" : `${scope} `;
  const base = `${pre}.markdown-alert{--md-alert:${TINTS.note};--md-alert-icon:${maskUrl(ICONS.note)};--md-fold-icon:${maskUrl("<path d='M6 9l6 6l6-6'/>")}}`;
  const families = CALLOUT_LOOKS.map(
    (k) => `${pre}.markdown-alert-${k}{--md-alert:${TINTS[k]};--md-alert-icon:${maskUrl(ICONS[k])}}`,
  );
  return [base, ...families].join("\n");
}

let installed = false;

/** Put the families' rules in the page once, for every `.md-doc` a view
 *  draws (the reading view, live blocks, hover previews, the daemon's
 *  fallback). The shared rules — the card, the title row, folding — stay
 *  in the view's own stylesheet. */
export function installCalloutStyle(): void {
  if (installed || typeof document === "undefined") return;
  installed = true;
  const style = document.createElement("style");
  style.dataset.callouts = "";
  style.textContent = calloutRules(".md-doc");
  document.head.append(style);
}

const FOLDABLE = "details.markdown-alert";

/** A drawn callout's first source line (its `data-sourcepos`). */
function firstLine(d: Element): number | null {
  const n = parseInt(d.getAttribute("data-sourcepos") ?? "", 10);
  return Number.isNaN(n) ? null : n;
}

/**
 * A document's callout folds as its reader left them, by each callout's
 * first source line — one per document view, shared by its reading article
 * and its live blocks. A callout opened in one is open in the other, and a
 * block drawn again keeps its state: the two views then lay out alike, so a
 * double-click lands the cursor where it was aimed. Session-only.
 */
export class CalloutFolds {
  private readonly state = new Map<number, boolean>();
  /** Every foldable callout drawn, to follow a toggle in another drawing
   *  (kept blocks sit outside the page until shown again). */
  private readonly drawn = new Set<WeakRef<HTMLDetailsElement>>();
  private readonly followed = new WeakSet<Element>();

  /** The foldable callouts in freshly drawn `nodes`: each takes the state
   *  remembered for its line, and its toggles are followed. */
  adopt(nodes: Iterable<Node>): void {
    for (const n of nodes) {
      if (!(n instanceof Element)) continue;
      if (n.matches(FOLDABLE)) this.follow(n as HTMLDetailsElement);
      for (const d of n.querySelectorAll<HTMLDetailsElement>(FOLDABLE)) this.follow(d);
    }
  }

  private follow(d: HTMLDetailsElement): void {
    if (this.followed.has(d)) return;
    const line = firstLine(d);
    if (line === null) return;
    this.followed.add(d);
    const open = this.state.get(line);
    if (open !== undefined) d.open = open;
    this.drawn.add(new WeakRef(d));
    d.addEventListener("toggle", () => this.toggled(d));
  }

  private toggled(d: HTMLDetailsElement): void {
    // Read again: a kept block's lines shift with edits above it.
    const line = firstLine(d);
    if (line === null || this.state.get(line) === d.open) return;
    this.state.set(line, d.open);
    for (const ref of this.drawn) {
      const other = ref.deref();
      if (other === undefined) this.drawn.delete(ref);
      else if (other !== d && other.open !== d.open && firstLine(other) === line) other.open = d.open;
    }
  }
}

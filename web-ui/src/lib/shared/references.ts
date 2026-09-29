/**
 * Ids as links, from any source: a source says which id shapes it answers
 * for and resolves an id to targets; surfaces (chat transcripts, the
 * Knowledge reader, markdown previews, the Timeline) turn matching text into
 * chips through this registry — never through a source's own store. A chip
 * previews its target on hover (the target's span, drawn as written) and
 * opens it on click. The Knowledge snapshot is the first source
 * (`knowledge/references.ts`); the plugin platform's `references/1`
 * publishers register the same way (docs/plugin-platform-plan.md).
 *
 * A candidate only becomes a chip when a source resolves it, so "COVID-19"
 * or a stray "D-1" that names nothing stays text. What a chip points at is
 * kept OFF the DOM (a WeakMap from the element this module built): agent
 * HTML can forge classes and data attributes, never an entry in this map.
 */
import { writable, get, type Readable } from "svelte/store";

import type { HostTarget } from "../previews/doc/hoverController.svelte";
import { pathTarget } from "./embed/embed";

/** A place in a file: 1-based lines, inclusive; workspace-relative path. */
export interface RefSpan {
  path: string;
  line: number;
  end_line: number;
}

/** Where an open lands the target from: the pane the chip is in. */
export interface OpenFrom {
  paneId: string | null;
  newSplit: boolean;
}

export interface RefTarget {
  /** Unique within its source. */
  key: string;
  /** The source's own kind word ("finding", "label"). */
  kind: string;
  title: string;
  span?: RefSpan;
  /** The absolute root a relative `span.path` is under. */
  base?: string | null;
  /** One line above the preview ("F-228 · finding · 2026-09-28"). */
  note?: string;
  open: (from: OpenFrom) => void;
}

export interface RefShape {
  kind: string;
  /** A JavaScript regex source, without anchors or groups. */
  pattern: string;
}

export interface RefSource {
  id: string;
  shapes: RefShape[];
  /** Absolute workspace root the spans are relative to. */
  root: string | null;
  /** The targets `id` names. `kind` is the shape it matched; `near` is the
   *  source's own context (Knowledge: the citing entry), for narrowing a
   *  reused id. */
  lookup: (id: string, kind: string, near?: unknown) => RefTarget[];
}

const sources = writable<Map<string, RefSource>>(new Map());

/** The registered sources; a change re-links what shows them. */
export const referenceSources: Readable<Map<string, RefSource>> = { subscribe: sources.subscribe };

/** Register (or replace) a source; returns the unregister. */
export function registerReferenceSource(src: RefSource): () => void {
  sources.update((m) => new Map(m).set(src.id, src));
  return () =>
    sources.update((m) => {
      if (m.get(src.id) !== src) return m;
      const next = new Map(m);
      next.delete(src.id);
      return next;
    });
}

export interface RefMatcher {
  re: RegExp;
  /** Per capture group: the source and the shape's kind. */
  groups: { source: RefSource; kind: string }[];
}

/** A shape is used only when it is a plain, bounded regex source that
 *  can't match nothing (an empty match would never advance the scan). */
/** The most unbounded repeats (`*`, `+`, `{n,}`) in any one `|`
 *  alternative of a regex source without groups (the daemon's references/1
 *  check, `surfaces::unbounded_per_alternative`, mirrored). */
export function unboundedPerAlternative(pattern: string): number {
  let most = 0;
  let here = 0;
  let inClass = false;
  for (let i = 0; i < pattern.length; i++) {
    const c = pattern[i];
    if (c === "\\") {
      i++;
    } else if (c === "[" && !inClass) {
      inClass = true;
    } else if (c === "]" && inClass) {
      inClass = false;
    } else if (inClass) {
      // A class's own `*` and `+` are characters.
    } else if (c === "*" || c === "+") {
      here++;
    } else if (c === "{") {
      const end = pattern.indexOf("}", i);
      if (end !== -1) {
        if (pattern.slice(i + 1, end).endsWith(",")) here++;
        i = end;
      }
    } else if (c === "|") {
      most = Math.max(most, here);
      here = 0;
    }
  }
  return Math.max(most, here);
}

/** A shape this page will scan text with: short, no groups (so no nested
 *  repeats), at most two unbounded repeats per alternative (each more one
 *  multiplies the backtracking over a long word), and never empty. */
function usable(pattern: string): boolean {
  if (pattern.length > 80 || pattern.includes("(") || unboundedPerAlternative(pattern) > 2) return false;
  try {
    return !new RegExp(`^(?:${pattern})$`).test("");
  } catch {
    return false;
  }
}

/** One regex over every source's shapes, bounded by non-word, non-dash
 *  edges; null when no source has a usable shape. */
export function referenceMatcher(all: ReadonlyMap<string, RefSource> = get(sources)): RefMatcher | null {
  const parts: string[] = [];
  const groups: RefMatcher["groups"] = [];
  for (const source of all.values()) {
    for (const s of source.shapes) {
      if (!usable(s.pattern)) continue;
      parts.push(`(${s.pattern})`);
      groups.push({ source, kind: s.kind });
    }
  }
  if (parts.length === 0) return null;
  return { re: new RegExp(`(?<![\\w-])(?:${parts.join("|")})(?![\\w-])`, "g"), groups };
}

/** What each chip a surface built points at. */
export class ReferenceChips {
  readonly #map = new WeakMap<Element, RefTarget[]>();

  set(el: Element, targets: RefTarget[]): void {
    this.#map.set(el, targets);
  }

  get(el: Element): RefTarget[] | undefined {
    return this.#map.get(el);
  }

  /** The chip `node` is (or is inside), with its targets. */
  chipAt(node: EventTarget | null): { el: Element; targets: RefTarget[] } | null {
    const el = node instanceof Element ? node.closest(".kchip") : null;
    if (el === null) return null;
    const targets = this.#map.get(el);
    return targets !== undefined && targets.length > 0 ? { el, targets } : null;
  }

  /** The hover controller's question: what a chip previews. */
  hoverTarget(el: Element): HostTarget | null {
    const targets = this.#map.get(el);
    return targets !== undefined ? referenceHoverTarget(targets) : null;
  }
}

/** A chip's preview: the first target's own lines, with its note. Null when
 *  it has nothing to show (no span, or one line of a table). */
export function referenceHoverTarget(targets: readonly RefTarget[]): HostTarget | null {
  const t = targets[0];
  const src = t?.span;
  if (t === undefined || src === undefined) return null;
  const whole = src.end_line <= 0;
  if (!whole && src.end_line <= src.line) return null;
  const path = src.path.startsWith("/") ? src.path : t.base ? `${t.base}/${src.path}` : null;
  if (path === null) return null;
  const fragment = whole ? null : `L${src.line}-L${src.end_line}`;
  const others = targets.length > 1 ? ` · ${targets.length} entries use this id` : "";
  return {
    target: { kind: "file", target: `${pathTarget(path)}${fragment !== null ? `#${fragment}` : ""}`, fragment, byName: false },
    ...(t.note !== undefined || others !== "" ? { note: `${t.note ?? t.title}${others}` } : {}),
  };
}

const SKIP = new Set(["A", "PRE", "SCRIPT", "STYLE", "TEXTAREA", "BUTTON", "INPUT", "SVG"]);

/**
 * Turn resolvable ids in `root`'s text into chips (`span.kchip`, focusable,
 * role link), registered in `chips`. Text inside links, code blocks,
 * controls, equations and existing chips is left alone. `near` is passed to
 * each source's lookup. Returns how many chips it made.
 */
export function linkReferences(
  root: HTMLElement,
  matcher: RefMatcher,
  chips: ReferenceChips,
  near?: unknown,
): number {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      for (let p = node.parentElement; p !== null && p !== root.parentElement; p = p.parentElement) {
        if (SKIP.has(p.tagName) || p.classList.contains("kchip") || p.classList.contains("katex")) return NodeFilter.FILTER_REJECT;
      }
      return NodeFilter.FILTER_ACCEPT;
    },
  });
  const texts: Text[] = [];
  for (let n = walker.nextNode(); n !== null; n = walker.nextNode()) texts.push(n as Text);
  let made = 0;
  for (const node of texts) {
    const text = node.data;
    matcher.re.lastIndex = 0;
    let m: RegExpExecArray | null;
    let last = 0;
    const frag = document.createDocumentFragment();
    while ((m = matcher.re.exec(text)) !== null) {
      const id = m[0];
      if (id === "") {
        matcher.re.lastIndex += 1;
        continue;
      }
      const g = m.slice(1).findIndex((x) => x !== undefined);
      const group = matcher.groups[g];
      if (group === undefined) continue;
      const found = group.source.lookup(id, group.kind, near);
      if (found.length === 0) continue;
      if (m.index > last) frag.append(text.slice(last, m.index));
      const chip = document.createElement("span");
      chip.className = "kchip";
      chip.setAttribute("role", "link");
      chip.tabIndex = 0;
      chip.textContent = id;
      chips.set(chip, found);
      frag.append(chip);
      last = m.index + id.length;
      made++;
    }
    if (last === 0) continue;
    if (last < text.length) frag.append(text.slice(last));
    node.replaceWith(frag);
  }
  return made;
}

/** The targets a bare id names, from the first source whose shape it
 *  matches whole and that resolves it (a Timeline row's "F-228"). */
export function resolveReference(id: string, all: ReadonlyMap<string, RefSource> = get(sources)): RefTarget[] {
  for (const source of all.values()) {
    for (const s of source.shapes) {
      if (!usable(s.pattern) || !new RegExp(`^(?:${s.pattern})$`).test(id)) continue;
      const found = source.lookup(id, s.kind);
      if (found.length > 0) return found;
    }
  }
  return [];
}

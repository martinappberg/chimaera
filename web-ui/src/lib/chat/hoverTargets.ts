/**
 * What a chat element previews on hover: the file a stamped path link
 * names, a document chip's file. One registry per ChatView, which hands it
 * to the hover controller (`previews/doc/hoverController.svelte.ts`) as the
 * host's `targetOf`. Kept off the DOM, like a path link's stamp: sanitized
 * agent HTML can forge classes and `data-*`, so only an element the chat
 * built or stamped previews anything. Entries die with their elements.
 */

import { pathTarget } from "../shared/embed/embed";
import type { FileRef } from "../shared/fileRef";
import type { HostTarget } from "../previews/doc/hoverController.svelte";

export interface ChatHoverTarget {
  /** The file, absolute (the daemon resolved it). */
  path: string;
  /** The piece to show (`L12-L20`, `page=3`, a heading), without `#`. */
  fragment: string | null;
  /** A line above the preview ("changed after this turn"). */
  note?: string;
}

export class HoverTargets {
  readonly #map = new WeakMap<Element, ChatHoverTarget>();

  set(el: Element, t: ChatHoverTarget): void {
    this.#map.set(el, t);
  }

  delete(el: Element): void {
    this.#map.delete(el);
  }

  /** The controller's question: what `el` previews, or null. */
  targetOf(el: Element): HostTarget | null {
    const t = this.#map.get(el);
    if (t === undefined) return null;
    return {
      target: {
        kind: "file",
        target: `${pathTarget(t.path)}${t.fragment !== null ? `#${t.fragment}` : ""}`,
        fragment: t.fragment,
        byName: false,
      },
      ...(t.note !== undefined ? { note: t.note } : {}),
    };
  }
}

/** The piece of its file a path reference names, as an embed fragment: a
 *  link's own `#fragment` when it has one, else its line range. */
export function refFragment(ref: FileRef, href: string | null = null): string | null {
  const hash = href?.indexOf("#") ?? -1;
  if (href !== null && hash >= 0 && hash < href.length - 1) return href.slice(hash + 1);
  if (ref.line === undefined) return null;
  return ref.endLine !== undefined ? `L${ref.line}-L${ref.endLine}` : `L${ref.line}`;
}

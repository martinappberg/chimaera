/**
 * `mountEmbed`: an EmbedCard for renderers that own raw DOM (the markdown
 * views' sanitized HTML, chat prose) instead of Svelte markup. The host
 * places an empty element where the embed goes — built by the host itself,
 * never taken from untrusted HTML — and mounts a card into it:
 *
 *   const card = mountEmbed(slot, { path, info, fragment, alt, width });
 *   card.update({ info: fresher });   // props are live
 *   card.destroy();                   // on the host's teardown / re-render
 *
 * Every card a host mounts must be destroyed when its slot leaves the DOM:
 * a card holds observers and, while on screen, a disk-watch registration.
 */

import { mount, unmount } from "svelte";
import EmbedCard from "./EmbedCard.svelte";
import type { TargetResult } from "./embed";
import type { Reveal } from "../reveal";

export interface EmbedProps {
  /** The absolute path when `info` is a hit; else the target as written
   *  (the card shows it in its missing state, or resolves it via
   *  `resolve`). */
  path: string;
  /** The `resolveTargets` answer for this target; omit to resolve lazily
   *  (`resolve`, else `path` as an absolute path) once near the viewport. */
  info?: TargetResult | null;
  /** The target's fragment without `#` (`page=3`, `L10-L30`, …). */
  fragment?: string | null;
  /** Alt text / caption. */
  alt?: string;
  /** Width hint in px (`![x|400](…)`). */
  width?: number | null;
  /** A gallery tile's fixed, shorter body. */
  compact?: boolean;
  resolve?: () => Promise<TargetResult | null>;
  /** Open in a pane (default: the workbench opener, `openPath`). */
  onOpen?: (path: string, kind: "file" | "dir", reveal?: Reveal) => void;
}

export interface EmbedHandle {
  /** Replace some props; the card re-renders in place. */
  update(next: Partial<EmbedProps>): void;
  /** Unmount the card (idempotent). */
  destroy(): void;
}

export function mountEmbed(target: HTMLElement, props: EmbedProps): EmbedHandle {
  const live = $state<EmbedProps>({ ...props });
  let component: ReturnType<typeof mount> | null = mount(EmbedCard, { target, props: live });
  return {
    update(next) {
      Object.assign(live, next);
    },
    destroy() {
      if (component === null) return;
      void unmount(component);
      component = null;
    },
  };
}

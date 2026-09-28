/**
 * One way for any surface (a markdown document, chat prose, a tool card, a
 * terminal link) to open a daemon path in the workbench. App owns the layout
 * and registers the opener; everything else calls `openPath`. Same
 * module-handler pattern as `setUrlPaneOpener` / the reference inserter.
 */

import { requestReveal, type Reveal } from "./reveal";

export type PathKind = "file" | "dir";

export interface OpenPathOptions {
  /** Cmd/Ctrl-click: open beside the source in a fresh split. */
  split?: boolean;
  /** Scroll to and flash this line range once the file shows. Files only. */
  reveal?: Reveal;
  /** The pane the gesture came from, when known (else the focused pane). */
  fromPane?: string;
}

type PathOpener = (path: string, kind: PathKind, opts: OpenPathOptions) => void;
let opener: PathOpener | null = null;

/** App-level wiring (null unregisters). */
export function setPathOpener(fn: PathOpener | null): void {
  opener = fn;
}

/** Open `path` (an absolute, already-resolved daemon path). False if no opener is registered. */
export function openPath(path: string, kind: PathKind, opts: OpenPathOptions = {}): boolean {
  if (opener === null) return false;
  if (kind === "file" && opts.reveal !== undefined) requestReveal(path, opts.reveal);
  opener(path, kind, opts);
  return true;
}

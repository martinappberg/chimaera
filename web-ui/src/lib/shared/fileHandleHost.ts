/**
 * Who carries out what a file handle (`fileHandle.ts`) asks for: App, which
 * owns drags and the typing into an agent's input, registers itself here.
 * A file of its own because App is in the always-loaded entry (a size budget
 * the build enforces): it imports this registry and nothing of the handle's
 * menu and listeners, which load with the surfaces that show files.
 */

import type { PathKind } from "./openPath";

export interface FileHandleHost {
  /** Begin a pointer drag of the file from `e` (a pointerdown). A release
   *  before the drag threshold is left to the element's own click. */
  drag(e: PointerEvent, path: string, kind: PathKind): void;
  /** Whether `reference` has somewhere to type: the chat `sessionId` while
   *  it lives, or (null) the window's reference target. */
  canReference(sessionId: string | null): boolean;
  /** Type the file's mention into that input, never sending. */
  reference(path: string, kind: PathKind, sessionId: string | null): void;
}

let host: FileHandleHost | null = null;

/** App-level wiring (null unregisters). */
export function setFileHandleHost(next: FileHandleHost | null): void {
  host = next;
}

/** The registered host, if any. */
export function fileHandleHost(): FileHandleHost | null {
  return host;
}

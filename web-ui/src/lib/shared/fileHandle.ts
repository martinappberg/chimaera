/**
 * A file shown on a surface (a chat attachment tile, an embed card, a
 * written-file chip) handled as the file it is, without opening it first:
 * drag it onto an agent's input to reference it there (onto a pane, and it
 * opens there, like a FILES-tree row), or right-click it for the same.
 *
 * The file already lives on the workspace host, so a reference is its path
 * typed as a mention: nothing is uploaded or copied. App owns drags and the
 * typing into an agent's input, and registers both (`setFileHandleHost`);
 * every surface attaches `fileHandle`. Same module-handler pattern as
 * `openPath.ts`.
 *
 * Attach it to the element that takes the click (the tile's button, the
 * chip): a drag captures the pointer on the element it started from, and
 * Chromium sends the click of a captured press to that element, so a handle
 * on an ancestor would take its children's clicks.
 */

import type { Attachment } from "svelte/attachments";
import { revealEntries } from "../net/native";
import { contextMenu, type ContextMenuEntry } from "./contextMenu.svelte";
import { openPath, pathPaneFrom, type PathKind } from "./openPath";
import { OPEN_BESIDE } from "./urlOpen";

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

export interface HandledFile {
  /** Absolute, already resolved on the workspace host. */
  path: string;
  kind?: PathKind;
}

/**
 * Where a file shown at `el` is referenced, read off the DOM so no component
 * has to thread a session id down to its tiles: the chat it is shown in
 * (its composer names it, `data-chat-session`), `null` for the window's
 * reference target (a surface outside any chat), or `undefined` when it is
 * in a chat that takes no input (ended, read-only): a reference made there
 * must not land in some other agent's draft.
 */
export function referenceScope(el: Element | null): string | null | undefined {
  const chat = el?.closest(".chat") ?? null;
  if (chat === null) return null;
  return chat.querySelector<HTMLElement>("[data-chat-session]")?.dataset.chatSession;
}

/** Label of the menu row that references a file in a chat's input. */
export const REFERENCE_IN_CHAT = "Reference in chat";

/**
 * The menu rows for a handled file: open it here or beside, reference it,
 * show it in the file manager (only where the files are this computer's).
 * The reference row is absent when no input can take it. Pure over its
 * arguments, so the rows a surface offers are testable without a DOM.
 */
export function fileMenuEntries(
  file: HandledFile,
  ctx: { canReference: boolean; reveal: ContextMenuEntry[] },
  act: { open(split: boolean): void; reference(): void },
): ContextMenuEntry[] {
  const entries: ContextMenuEntry[] = [
    { label: "Open", onSelect: () => act.open(false) },
    ...(file.kind === "dir" ? [] : [{ label: OPEN_BESIDE, onSelect: () => act.open(true) }]),
  ];
  if (ctx.canReference) entries.push("separator", { label: REFERENCE_IN_CHAT, onSelect: act.reference });
  if (ctx.reveal.length > 0) entries.push("separator", ...ctx.reveal);
  return entries;
}

/** Whether a file shown at `el` can be referenced: its chat is live, or
 *  (outside a chat) the window has a reference target. */
export function canReferenceFile(el: Element | null): boolean {
  const scope = referenceScope(el);
  return scope !== undefined && host?.canReference(scope) === true;
}

/** Type `file`'s mention into the chat `el` is shown in (or the window's
 *  reference target). False when nothing can take it. */
export function referenceHandledFile(file: HandledFile, el: Element | null): boolean {
  const scope = referenceScope(el);
  if (host === null || scope === undefined || !host.canReference(scope)) return false;
  host.reference(file.path, file.kind ?? "file", scope);
  return true;
}

/** The right-click menu for `file`, opened at `e` (see `fileMenuEntries`). */
export function openFileMenu(e: MouseEvent, file: HandledFile, el: Element | null): void {
  const kind = file.kind ?? "file";
  const fromPane = pathPaneFrom(el);
  contextMenu.openAt(
    e,
    fileMenuEntries(
      file,
      { canReference: canReferenceFile(el), reveal: revealEntries(file.path) },
      {
        open: (split) => void openPath(file.path, kind, { split, fromPane }),
        reference: () => void referenceHandledFile(file, el),
      },
    ),
  );
}

/**
 * Make `el` a handle on the file `get()` names now (null: not known yet, or
 * gone — then it is an ordinary element). Listeners are native and read
 * `get` at each gesture, so the attachment never re-runs for a path that
 * resolves later.
 */
export function fileHandle(get: () => HandledFile | null): Attachment<HTMLElement> {
  return (el) => {
    const onPointerDown = (e: PointerEvent) => {
      const file = get();
      // A mouse only: a finger on a figure is scrolling the transcript.
      if (file === null || host === null || e.button !== 0 || e.pointerType !== "mouse") return;
      host.drag(e, file.path, file.kind ?? "file");
    };
    const onContextMenu = (e: MouseEvent) => {
      const file = get();
      if (file !== null) openFileMenu(e, file, el);
    };
    el.addEventListener("pointerdown", onPointerDown);
    el.addEventListener("contextmenu", onContextMenu);
    return () => {
      el.removeEventListener("pointerdown", onPointerDown);
      el.removeEventListener("contextmenu", onContextMenu);
    };
  };
}

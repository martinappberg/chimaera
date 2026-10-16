/**
 * The inline rename/create edit: F2, double-click or a menu item swaps a name
 * for an input; Enter commits, Escape cancels, blur commits. The files tree,
 * the Finder and the pane tabs each carried a copy of this state machine.
 *
 * The rules it keeps in one place:
 *   - A blank name cancels. A name `check` calls unchanged closes quietly.
 *   - An invalid name shows `error` inline on Enter; on blur it cancels
 *     instead, so no error floats beside an input that lost focus.
 *   - Escape clears the edit BEFORE the input unmounts, so the blur that
 *     follows finds nothing to commit.
 *   - One submit per edit: a second commit (Enter, then the blur it causes,
 *     or a held Enter) while the first is in flight is ignored.
 *   - A failed submit keeps the input open with the error; a submit that
 *     resolves closes the edit (unless the site already closed it, or a newer
 *     edit replaced it meanwhile).
 *
 * The site owns what the edit targets, the check and the submit, the error
 * text, where the error renders, and where focus goes after close
 * (`onClose`). `renameFocus` is the input's mount action.
 */
import type { Action } from "svelte/action";
import { stemLength } from "./fsNames";

/** `submit` the name, close quietly (`unchanged`), or show `{ invalid }`. */
export type RenameCheck = "submit" | "unchanged" | { invalid: string };

export interface InlineRenameSpec<T> {
  /** Decide what a trimmed, non-empty name does. Default: submit. */
  check?(target: T, name: string): RenameCheck;
  /** Do the rename. Call `close()` early when work continues after the edit
   *  should disappear (reveal a created file, fire-and-forget a session rename). */
  submit(target: T, name: string, close: () => void): Promise<void> | void;
  /** Inline text for a submit that threw. */
  errorText(err: unknown): string;
  /** After every close (cancel or success), e.g. to put focus back on the row. */
  onClose?(target: T): void;
}

export class InlineRename<T> {
  /** What is being edited; null while no edit is open. */
  target = $state<T | null>(null);
  /** The input's text (`bind:value`). */
  draft = $state("");
  /** Inline error for the open edit. */
  error = $state<string | null>(null);

  #spec: InlineRenameSpec<T>;
  /** Identity of the open edit; a fresh object per `begin`. */
  #session: object | null = null;
  #inflight: object | null = null;

  constructor(spec: InlineRenameSpec<T>) {
    this.#spec = spec;
  }

  begin(target: T, draft: string): void {
    this.target = target;
    this.draft = draft;
    this.error = null;
    this.#session = {};
  }

  /** Close without committing. Safe to call when nothing is open. */
  cancel(): void {
    const target = this.target;
    this.target = null;
    this.draft = "";
    this.error = null;
    this.#session = null;
    if (target !== null) this.#spec.onClose?.(target);
  }

  /** Commit the draft. `viaBlur` turns an invalid name into a cancel. */
  async commit(viaBlur = false): Promise<void> {
    const target = this.target;
    const session = this.#session;
    if (target === null || session === null || this.#inflight === session) return;
    const name = this.draft.trim();
    if (name === "") {
      this.cancel();
      return;
    }
    const verdict = this.#spec.check?.(target, name) ?? "submit";
    if (verdict === "unchanged") {
      this.cancel();
      return;
    }
    if (verdict !== "submit") {
      if (viaBlur) this.cancel();
      else this.error = verdict.invalid;
      return;
    }
    const close = () => {
      if (this.#session === session) this.cancel();
    };
    this.#inflight = session;
    try {
      await this.#spec.submit(target, name, close);
      close();
    } catch (err) {
      if (this.#session === session) this.error = this.#spec.errorText(err);
    } finally {
      if (this.#inflight === session) this.#inflight = null;
    }
  }

  /** The input's keydown: keeps every key from the row, tree and app chords. */
  keydown(e: KeyboardEvent): void {
    e.stopPropagation();
    if (e.key === "Enter") {
      e.preventDefault();
      void this.commit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      this.cancel(); // nulling first makes the following blur a no-op
    }
  }

  /** The input's blur. */
  blur(): void {
    void this.commit(true);
  }
}

/** Focus the fresh input; `selectStem` preselects the name minus its
 *  extension (`stemLength`), so typing replaces the stem. */
export const renameFocus: Action<HTMLInputElement, boolean> = (node, selectStem) => {
  node.focus();
  if (selectStem) node.setSelectionRange(0, stemLength(node.value));
};

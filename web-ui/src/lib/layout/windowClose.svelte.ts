/**
 * The window half of "never lose an edit" in the native app (document
 * workbench plan, Phase 0). The shell holds a window close, or the app's
 * quit, while this window reports unsaved files (`reportUnsaved`), and asks
 * here (`onUnsavedPrompt`). This runs the tab close's Save all / Don't save /
 * Cancel over every unsaved file in the window and replies:
 *
 * - `shown` at once — proof the page is alive, so the shell waits for the
 *   user instead of proceeding after its hung-page timeout;
 * - `proceed` once nothing is left unsaved (every save landed, or Don't
 *   save), and the shell closes the window or moves the quit on;
 * - `cancel` to keep the window (a quit is abandoned).
 *
 * Save is bounded by the close dialog's deadline: past it (a dead link) the
 * dialog says "not saved" and hands control back, the save carrying on in the
 * background. The shell never closes on a failed save; only an explicit
 * Don't save discards.
 */
import { get } from "svelte/store";
import type { UnsavedPrompt, UnsavedReason, UnsavedReply } from "../net/native";
import {
  CLOSE_SAVE_DEADLINE_MS,
  dirtyFiles,
  discardDirtyFile,
  saveDirtyFiles,
  unsavedCloseError,
} from "../shared/editing";

export interface WindowClosePrompt {
  /** The shell's prompt id; every reply carries it. */
  id: number;
  reason: UnsavedReason;
  /** The unsaved files, as listed when asked (refreshed after a partial save). */
  paths: string[];
}

function unsavedPaths(): string[] {
  return [...get(dirtyFiles)].sort();
}

export class WindowCloseGuard {
  /** The open dialog, or null. */
  prompt = $state<WindowClosePrompt | null>(null);
  saving = $state(false);
  error = $state<string | null>(null);

  #attempt = 0;
  readonly #reply: (id: number, reply: UnsavedReply) => Promise<void>;
  readonly #deadlineMs: number;

  constructor(
    reply: (id: number, reply: UnsavedReply) => Promise<void>,
    deadlineMs = CLOSE_SAVE_DEADLINE_MS,
  ) {
    this.#reply = reply;
    this.#deadlineMs = deadlineMs;
  }

  /** The shell asks, or asks again (a second close click, a quit taking
   *  over a close): the open dialog stays as it is, under the newest ask. */
  receive(p: UnsavedPrompt): void {
    const paths = unsavedPaths();
    if (paths.length === 0) {
      // Saved or discarded since the shell last heard: nothing to lose.
      this.#attempt++;
      this.saving = false;
      this.prompt = { ...p, paths };
      this.#settle("proceed");
      return;
    }
    this.#send(p.id, "shown");
    if (this.prompt === null) {
      this.prompt = { ...p, paths };
      this.error = null;
    } else {
      this.prompt = { ...this.prompt, id: p.id, reason: p.reason };
    }
  }

  /** Save all; proceed only once nothing is left unsaved. */
  async save(): Promise<void> {
    const prompt = this.prompt;
    if (prompt === null || this.saving) return;
    this.saving = true;
    this.error = null;
    const attempt = ++this.#attempt;
    const r = await saveDirtyFiles(prompt.paths, {
      deadlineMs: this.#deadlineMs,
      cancelled: () => attempt !== this.#attempt,
    });
    // Cancelled (or settled by a newer ask) while waiting: touch nothing.
    if (r === null) return;
    this.saving = false;
    const left = unsavedPaths();
    if (left.length === 0) {
      this.#settle("proceed");
      return;
    }
    const current = this.prompt;
    if (current === null) return;
    this.prompt = { ...current, paths: left };
    const failed = r.unsaved.filter((path) => left.includes(path));
    this.error = unsavedCloseError(failed.length > 0 ? failed : left, r.timedOut);
  }

  /** Don't save: drop every unsaved edit (and its journaled draft). Not
   *  while saving — a write in flight could land after the discard. */
  discard(): void {
    if (this.prompt === null || this.saving) return;
    for (const path of unsavedPaths()) discardDirtyFile(path);
    this.#settle("proceed");
  }

  /** Keep the window. Also while saving: the files stay dirty, and a save
   *  already sent may still land in the background. */
  cancel(): void {
    if (this.prompt === null) return;
    this.#attempt++;
    this.saving = false;
    this.#settle("cancel");
  }

  #settle(reply: "proceed" | "cancel"): void {
    const prompt = this.prompt;
    if (prompt === null) return;
    this.prompt = null;
    this.error = null;
    this.#send(prompt.id, reply);
  }

  #send(id: number, reply: UnsavedReply): void {
    // A lost reply is not a trap: the shell proceeds after its hung-page
    // timeout (no `shown`), or the user's next close / quit asks again.
    void this.#reply(id, reply).catch(() => {});
  }
}

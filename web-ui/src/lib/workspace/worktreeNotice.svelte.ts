/**
 * The rail's one "kept" notice under the agents: a managed worktree whose
 * last session just ended (or whose conversation was archived). Never
 * auto-removed — Remove or Keep is the user's call; a newer notice replaces
 * it. Plus "Open worktree in new window" from a row's menu.
 *
 * App constructs one `KeptWorktreeNotice` and calls `track()` where the
 * notice's two effects used to be declared, so the window's effect order is
 * unchanged. The removal request, its refusal text and the 6 s error linger
 * are as before.
 */
import { untrack } from "svelte";
import { ApiError, getHostLabel } from "../net/api";
import { openWindow } from "../net/native";
import { removeWorktree } from "./git";
import { baseName, createWorkspace } from "./sessions";

export interface KeptWorktree {
  ws: string;
  path: string;
  label: string;
  busy: boolean;
  error: string | null;
}

export interface KeptWorktreeDeps {
  activeWsId: () => string | null;
  /** The worktree went: the active workspace's Recents may have changed. */
  refreshRecents: () => void;
  /** Opening the worktree's window failed: App's inline create error. */
  onError: (message: string) => void;
}

/** How long a refusal stays on screen. */
const REFUSAL_LINGER_MS = 6000;

export class KeptWorktreeNotice {
  notice = $state<KeptWorktree | null>(null);

  #timer: ReturnType<typeof setTimeout> | null = null;
  readonly #deps: KeptWorktreeDeps;

  constructor(deps: KeptWorktreeDeps) {
    this.#deps = deps;
  }

  #clearTimer(): void {
    if (this.#timer !== null) {
      clearTimeout(this.#timer);
      this.#timer = null;
    }
  }

  dismiss = (): void => {
    this.#clearTimer();
    this.notice = null;
  };

  offer(ws: string, path: string, branch: string | null): void {
    this.#clearTimer();
    // The branch's last segment: the rail is narrow, and "chimaera/" says
    // nothing the notice needs (the full path is the tooltip).
    this.notice = { ws, path, label: baseName(branch ?? path), busy: false, error: null };
  }

  async remove(): Promise<void> {
    const n = this.notice;
    if (n === null || n.busy) return;
    this.notice = { ...n, busy: true };
    try {
      await removeWorktree(n.ws, n.path);
      if (this.notice?.path === n.path) this.notice = null;
      if (this.#deps.activeWsId() === n.ws) this.#deps.refreshRecents();
    } catch (e) {
      if (this.notice?.path !== n.path) return;
      // The daemon's refusal (uncommitted changes, unmerged commits, a live
      // session, a lock) is the honest answer: say it, keep the worktree.
      this.notice = {
        ...n,
        busy: false,
        error: e instanceof ApiError ? e.message : "could not remove the worktree",
      };
      this.#clearTimer();
      this.#timer = setTimeout(() => {
        this.#timer = null;
        if (this.notice?.path === n.path) this.notice = null;
      }, REFUSAL_LINGER_MS);
    }
  }

  /** Declares the notice's effects, in order: a workspace switch drops a
   *  notice that belongs to another workspace; teardown clears the timer. */
  track(): void {
    $effect(() => {
      const ws = this.#deps.activeWsId();
      untrack(() => {
        if (this.notice !== null && this.notice.ws !== ws) this.dismiss();
      });
    });
    $effect(() => {
      return () => this.#clearTimer();
    });
  }

  /** Open a managed worktree as its own window — the folder picker's "new
   *  window" path; this window stays where it is. */
  async openWindow(path: string): Promise<void> {
    try {
      const w = await createWorkspace(path);
      const alias = getHostLabel() === "local" ? null : getHostLabel();
      await openWindow(alias, w.id, true);
    } catch (e) {
      this.#deps.onError(e instanceof ApiError ? e.message : "could not open the worktree");
    }
  }
}

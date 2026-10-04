/** Finite presentation domain for the original kept-review workspace. No URL,
 * token or generic request crosses this boundary. The existing host owns IO. */
import { toStore } from "svelte/store";
import { fetchKeptFile, resolveKept, resolveAllKept } from "../pro/kept";
import { keptReviews } from "../pro/keptReviews.svelte";
import type { KeptChoice, KeptFile, KeptPair, KeptReview, KeptSide } from "../pro/kept";
import { relativeFilePath, type Observable } from "./application";
export type { KeptChoice, KeptFile, KeptPair, KeptReview };
export interface KeptEditor { fontSize: number; lineHeight: number; tabSize: number; lineNumbers: boolean }
export interface KeptViewSnapshot { revision: number; review: Readonly<KeptReview> | null | undefined; error: "invalid" | null }
export interface KeptConfirmation { readonly choice: KeptChoice; readonly revision: number; readonly count: number; readonly trash: boolean }
export interface KeptReviewDomain extends Observable<KeptViewSnapshot> {
  readonly workspaceId: string;
  readonly hereName: "this Mac" | "this computer" | "your computer";
  readonly editor: Readonly<KeptEditor>;
  refresh(): Promise<void>;
  read(minePath: string, revision: number, signal: AbortSignal): Promise<KeptFile>;
  choose(minePath: string, choice: KeptChoice, revision: number): Promise<KeptReview>;
  confirmAll(choice: KeptChoice, revision: number): KeptConfirmation;
  cancelConfirmation(confirmation: KeptConfirmation): void;
  chooseAll(confirmation: KeptConfirmation): Promise<KeptReview>;
  dispose(): void;
}
export interface KeptReviewBackend {
  observe(workspaceId: string): Observable<KeptReview | null | undefined>;
  refresh(workspaceId: string): Promise<void>;
  read(workspaceId: string, minePath: string, signal: AbortSignal): Promise<KeptFile>;
  choose(workspaceId: string, minePath: string, choice: KeptChoice): Promise<KeptReview>;
  chooseAll(workspaceId: string, choice: KeptChoice): Promise<KeptReview>;
  /** Same original host singleton used by chat/tree/settings. */
  set(workspaceId: string, review: KeptReview): void;
}
export interface KeptReviewBinding {
  workspaceId: string;
  current(): boolean;
  hereName: KeptReviewDomain["hereName"];
  editor: KeptEditor;
}
export class KeptDomainError extends Error {
  constructor(readonly code: "retired" | "invalid" | "changed" | "busy" | "failed") {
    super(code === "changed" ? "These versions changed. Review them again before choosing." :
      code === "retired" ? "This review is no longer current." : code === "busy" ? "A review operation is still in progress." : "The kept versions couldn't be read or changed.");
    this.name = "KeptDomainError";
  }
}
const count = (value: number) => Number.isSafeInteger(value) && value >= 0;
const stamp = (value: number | null) => value === null || (count(value) && value <= 8_640_000_000_000_000);
const choiceIsValid = (value: string): value is KeptChoice => ["use_mine", "use_cloud", "keep_both"].includes(value);
const refused = (): never => { throw new KeptDomainError("invalid"); };
function pair(value: KeptPair): KeptPair {
  if (!relativeFilePath(value.path) || !relativeFilePath(value.mine_path) || value.path === value.mine_path ||
      !(value.size === null || count(value.size)) || !count(value.mine_size) || !stamp(value.changed_at) || !stamp(value.mine_changed_at) ||
      (value.can_use_mine !== undefined && typeof value.can_use_mine !== "boolean")) refused();
  return Object.freeze({ path: value.path, mine_path: value.mine_path, size: value.size, mine_size: value.mine_size,
    changed_at: value.changed_at, mine_changed_at: value.mine_changed_at,
    ...(value.can_use_mine === undefined ? {} : { can_use_mine: value.can_use_mine }) });
}
export function keptReviewProjection(value: KeptReview, workspace: string): KeptReview {
  if (value.workspace_id !== workspace || !count(value.files) || !count(value.total) || value.total < value.files ||
      !count(value.unlisted) || !stamp(value.returned_at) || typeof value.here !== "boolean" ||
      (value.trash !== undefined && typeof value.trash !== "boolean") || !Array.isArray(value.pairs) || value.pairs.length > 32 ||
      !Array.isArray(value.branches) || value.branches.length > 32 || value.branches.some((b) => typeof b !== "string" || b.length === 0 || b.length > 4096 || b.includes("\0")) ||
      new Set(value.branches).size !== value.branches.length ||
      (value.failed !== undefined && (!Array.isArray(value.failed) || value.failed.length > 32)) ||
      (value.discarded !== undefined && (!count(value.discarded.trash) || !count(value.discarded.deleted)))) refused();
  const pairs = value.pairs.map(pair);
  if (new Set(pairs.map((p) => p.mine_path)).size !== pairs.length || pairs.length > value.files || value.unlisted !== value.files - pairs.length) refused();
  const failed = value.failed?.map((f) => {
    if (!relativeFilePath(f.mine_path) || typeof f.error_code !== "string" || !/^[a-z_]{1,64}$/.test(f.error_code)) refused();
    return Object.freeze({ mine_path: f.mine_path, error_code: f.error_code });
  });
  return Object.freeze({ workspace_id: workspace, files: value.files, total: value.total, returned_at: value.returned_at,
    unlisted: value.unlisted, pairs: Object.freeze(pairs) as unknown as KeptPair[], branches: Object.freeze([...value.branches]) as unknown as string[],
    here: value.here, ...(value.trash === undefined ? {} : { trash: value.trash }),
    ...(failed === undefined ? {} : { failed: Object.freeze(failed) as unknown as KeptReview["failed"] }),
    ...(value.discarded === undefined ? {} : { discarded: Object.freeze({ trash: value.discarded.trash, deleted: value.discarded.deleted }) }) });
}
function side(value: KeptSide): KeptSide {
  if (!count(value.size) || !stamp(value.changed_at) || !(value.text === null || typeof value.text === "string") ||
      (value.text !== null && (value.text.length > 512 * 1024 || new TextEncoder().encode(value.text).byteLength > 512 * 1024)) ||
      (value.binary !== undefined && typeof value.binary !== "boolean") || (value.too_large !== undefined && typeof value.too_large !== "boolean")) refused();
  return Object.freeze({ size: value.size, changed_at: value.changed_at, text: value.text,
    ...(value.binary === undefined ? {} : { binary: value.binary }), ...(value.too_large === undefined ? {} : { too_large: value.too_large }) });
}
function fileProjection(value: KeptFile, expected: KeptPair): KeptFile {
  if (value.path !== expected.path || value.mine_path !== expected.mine_path) refused();
  if (value.mine.size !== expected.mine_size || value.mine.changed_at !== expected.mine_changed_at ||
      (expected.size === null ? value.cloud !== null : value.cloud === null || value.cloud.size !== expected.size || value.cloud.changed_at !== expected.changed_at)) {
    throw new KeptDomainError("changed");
  }
  return Object.freeze({ path: value.path, mine_path: value.mine_path, mine: side(value.mine), cloud: value.cloud === null ? null : side(value.cloud) });
}
/** A selector is not authority: every action rechecks the captured host view,
 * listed pair and revision before and after IO; the daemon retains its guards. */
export function bindKeptReview(binding: KeptReviewBinding, backend: KeptReviewBackend): KeptReviewDomain {
  if (!/^[a-zA-Z0-9_-]{1,128}$/.test(binding.workspaceId) || !["this Mac", "this computer", "your computer"].includes(binding.hereName) ||
      !Number.isFinite(binding.editor.fontSize) || binding.editor.fontSize < 8 || binding.editor.fontSize > 72 ||
      !Number.isFinite(binding.editor.lineHeight) || binding.editor.lineHeight < 1 || binding.editor.lineHeight > 3 ||
      !Number.isInteger(binding.editor.tabSize) || binding.editor.tabSize < 1 || binding.editor.tabSize > 16 || typeof binding.editor.lineNumbers !== "boolean") refused();
  const workspaceId = binding.workspaceId;
  const isCurrent = binding.current;
  let live = true, revision = 1, digest = "initial", review: KeptReview | null | undefined;
  let error: "invalid" | null = null;
  let busy = false, reads = 0, stop: (() => void) | null = null;
  const listeners = new Set<(value: KeptViewSnapshot) => void>();
  const confirmations = new Set<KeptConfirmation>();
  const readControllers = new Set<AbortController>();
  const current = () => { if (!live || !isCurrent()) throw new KeptDomainError("retired"); };
  const snapshot = () => Object.freeze({ revision, review, error });
  let blockedDigest: string | null = null;
  let lastSeen: KeptReview | null | undefined;
  let lastError: "invalid" | null = null;
  const accept = (next: KeptReview | null | undefined, nextError: "invalid" | null, freshRead = false) => {
    const nextDigest = JSON.stringify([next, nextError]);
    if (nextDigest === digest && !freshRead) return;
    if (revision === Number.MAX_SAFE_INTEGER) throw new KeptDomainError("retired");
    digest = nextDigest; review = next; error = nextError; revision += 1;
    for (const send of listeners) send(snapshot());
  };
  const publish = (value: KeptReview | null | undefined) => {
    if (!live || !isCurrent()) return;
    let next: KeptReview | null | undefined;
    let nextError: "invalid" | null = null;
    try { next = value == null ? value : keptReviewProjection(value, workspaceId); }
    catch { next = null; nextError = "invalid"; }
    lastSeen = next; lastError = nextError;
    // An unrelated host-store notification cannot revive a pair already
    // disproved by its file read. Only changed metadata or explicit refresh can.
    if (blockedDigest === JSON.stringify(next)) return;
    blockedDigest = null;
    accept(next, nextError);
  };
  const start = () => { if (stop === null) stop = backend.observe(workspaceId).subscribe(publish); };
  const expected = (at: number, minePath?: string) => {
    current(); if (at !== revision || !review) throw new KeptDomainError("changed");
    if (minePath === undefined) return null;
    const found = review.pairs.find((p) => p.mine_path === minePath);
    if (!found) throw new KeptDomainError("changed"); return found;
  };
  const apply = async (at: number, minePath: string | undefined, effect: () => Promise<KeptReview>): Promise<KeptReview> => {
    if (busy) throw new KeptDomainError("busy"); busy = true;
    try {
      const answer = await effect(); expected(at, minePath); const closed = keptReviewProjection(answer, workspaceId);
      backend.set(workspaceId, closed); publish(closed); return closed;
    } finally { busy = false; }
  };
  const domain: KeptReviewDomain = {
    workspaceId, hereName: binding.hereName, editor: Object.freeze({ fontSize: binding.editor.fontSize, lineHeight: binding.editor.lineHeight, tabSize: binding.editor.tabSize, lineNumbers: binding.editor.lineNumbers }),
    subscribe(send) {
      current(); if (listeners.size >= 8) throw new KeptDomainError("busy");
      start(); listeners.add(send);
      try { send(snapshot()); } catch (error) {
        listeners.delete(send);
        if (listeners.size === 0) { const captured = stop; stop = null; captured?.(); }
        throw error;
      }
      let subscribed = true;
      return () => { if (!subscribed) return; subscribed = false; listeners.delete(send); if (listeners.size === 0) { const captured = stop; stop = null; captured?.(); } };
    },
    async refresh() {
      current(); start(); await backend.refresh(workspaceId); current();
      // A deliberate refresh also retries a failed read with unchanged metadata.
      // It retires old read/confirmation revisions, never replays a mutation.
      blockedDigest = null; accept(lastSeen, lastError, true);
    },
    async read(minePath, at, signal) {
      const original = expected(at, minePath)!;
      if (signal.aborted) throw new KeptDomainError("retired"); if (reads >= 2) throw new KeptDomainError("busy");
      const controller = new AbortController(); const abort = () => controller.abort(); signal.addEventListener("abort", abort, { once: true });
      reads += 1; readControllers.add(controller);
      try {
        const answer = await backend.read(workspaceId, minePath, controller.signal);
        expected(at, minePath); if (controller.signal.aborted) throw new KeptDomainError("retired");
        try { return fileProjection(answer, original); }
        catch (error) {
          // A changed read cannot leave the old displayed pair actionable.
          // Retire this local snapshot; an explicit refresh may read it anew.
          blockedDigest = JSON.stringify(review);
          accept(null, "invalid");
          throw error;
        }
      } finally { reads -= 1; readControllers.delete(controller); signal.removeEventListener("abort", abort); }
    },
    async choose(minePath, choice, at) {
      const original = expected(at, minePath)!;
      if (!choiceIsValid(choice) || !review?.here || (choice === "use_mine" && original.can_use_mine === false)) refused();
      return apply(at, minePath, () => { expected(at, minePath); return backend.choose(workspaceId, minePath, choice); });
    },
    confirmAll(choice, at) {
      expected(at);
      const original = review;
      if (original === null || original === undefined) throw new KeptDomainError("changed");
      // Done acknowledges unnamed kept copies without deleting either version.
      // It still carries this exact original revision and a one-use identity.
      const done = choice === "keep_both" && original.pairs.length === 0 && original.unlisted > 0;
      if (!choiceIsValid(choice) || !original.here || (original.pairs.length === 0 && !done) ||
          (choice === "use_mine" && original.pairs.some((pair) => pair.can_use_mine === false))) refused();
      if (confirmations.size >= 8) throw new KeptDomainError("busy");
      const value = Object.freeze({ choice, revision: at, count: original.pairs.length, trash: original.trash === true });
      confirmations.add(value); return value;
    },
    cancelConfirmation(value) { confirmations.delete(value); },
    async chooseAll(value) {
      current(); if (!confirmations.delete(value)) throw new KeptDomainError("changed");
      expected(value.revision);
      if (!review?.here || (value.choice === "use_mine" && review.pairs.some((p) => p.can_use_mine === false))) refused();
      return apply(value.revision, undefined, () => { expected(value.revision); return backend.chooseAll(workspaceId, value.choice); });
    },
    dispose() {
      if (!live) return; live = false; confirmations.clear(); listeners.clear();
      for (const controller of readControllers) controller.abort();
      const captured = stop; stop = null; captured?.();
    },
  };
  return Object.freeze(domain);
}

/** Adapter to the existing singleton and four existing routes. The private
 * module never imports them; owner IO/daemon validation semantics stay here. */
export function bindExistingKeptReview(binding: KeptReviewBinding): KeptReviewDomain {
  return bindKeptReview(binding, {
    observe: (workspace) => toStore(() => keptReviews.byWorkspace[workspace]),
    refresh: (workspace) => keptReviews.refresh(workspace),
    read: fetchKeptFile, choose: resolveKept, chooseAll: resolveAllKept,
    set: (workspace, answer) => keptReviews.set(workspace, answer),
  });
}

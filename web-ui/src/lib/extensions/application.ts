/** Trusted first-party presentation only. The caller supplies the window's
 * existing runtime services; this module creates no account or route owner. */
import type { KeptReviewDomain } from "./keptReview";
import type { AccountPresentationServices, AccountHostScope, AccountBrandingSubscription } from "./accountPresentation";

export type SurfaceKind = "account" | "account-settings" | "account-home" | "cloud-projects" | "cloud-setup" | "kept-review";
export interface Observable<T> { subscribe(listener: (snapshot: Readonly<T>) => void): () => void }
export interface AccountSnapshot {
  version: 1;
  available: boolean;
  plan: "loading" | "unknown" | "unavailable" | "free" | "pro" | "max";
  signedOut: boolean;
}
export interface KeptSnapshot { version: 1; workspaceId: string; pendingFiles: number; returnedAt: string | null }
export interface OnboardingIntent {
  version: 1;
  intentId: string;
  providerIds: readonly string[];
  workspaceId: string | null;
  projectLabel: string | null;
}
export interface ApplicationRuntime {
  version: 1;
  account: Observable<AccountSnapshot>;
  visibility: Observable<boolean>;
  kept: { observe(workspaceId: string): Observable<KeptSnapshot | null> };
  onboarding: Observable<OnboardingIntent | null>;
}
export interface SurfaceIdentity { version: 1; viewId: string; attemptId: number; workspaceId: string | null }
export interface SurfacePresentation { knownWorkspaceIds?: readonly string[]; revision: number; visible: boolean; projectLabel: string | null; onboarding: OnboardingIntent | null }
export interface SurfaceActions {
  openFile(relativePath: string): Promise<void>;
  completeOnboarding(intentId: string): void;
  close(): void;
  modal(node: HTMLElement): { destroy(): void };
}
export interface SurfaceMount {
  identity: Readonly<SurfaceIdentity>;
  presentation: Readonly<SurfacePresentation>;
  runtime: ApplicationRuntime;
  actions: SurfaceActions;
  signal: AbortSignal;
  /** Original host-bound review only; absent on ordinary/account surfaces. */
  keptReview: KeptReviewDomain | null;
  accountPresentation?: AccountPresentationServices | null;
}
export interface SurfaceOwner { update(presentation: Readonly<SurfacePresentation>): void; dispose(): void }
export interface ApplicationExtension {
  version: 1;
  id: "chimaera-pro";
  mount(kind: SurfaceKind, target: HTMLElement, mount: SurfaceMount): Promise<SurfaceOwner>;
  bindAccountPresentation?(scope: AccountHostScope): Promise<AccountPresentationServices>;
  bindAccountBranding?(scope: AccountBrandingSubscription): Promise<() => void>;
}
export type SurfaceStatus = "absent" | "loading" | "ready" | "failed" | "closed";
export interface HostSurfaceActions {
  /** Captured original pane/root; must independently check current scope. */
  openFile(relativePath: string): Promise<void>;
  /** Checks the original current onboarding intent, never a successor. */
  completeOnboarding(intentId: string): void;
  close(): void;
  /** The original shared host modalFocus action, at ordinary priority. */
  modal(node: HTMLElement): { destroy(): void };
  current(): boolean;
  /** Host-only factory captures original workspace/epoch/pane before mount. */
  keptReview?(): KeptReviewDomain;
  /** Selected entry only; the original host owns every named service. */
  accountPresentation?(actions: SurfaceActions, signal: AbortSignal): Promise<AccountPresentationServices>;
}
const validId = (v: unknown): v is string => typeof v === "string" && /^[a-zA-Z0-9_-]{1,128}$/.test(v);
const counter = (v: number): boolean => Number.isSafeInteger(v) && v > 0;
export function relativeFilePath(path: string): boolean {
  return typeof path === "string" && path.length > 0 && path.length <= 4096 &&
    !path.includes("\0") && !path.startsWith("/") &&
    !path.split("/").some((part) => part === "" || part === "." || part === "..");
}
function snapshot(value: SurfacePresentation): Readonly<SurfacePresentation> {
  const intent = value.onboarding;
  if ((value.knownWorkspaceIds !== undefined && (!Array.isArray(value.knownWorkspaceIds) || value.knownWorkspaceIds.length > 256 ||
      value.knownWorkspaceIds.some(id => !validId(id)) || new Set(value.knownWorkspaceIds).size !== value.knownWorkspaceIds.length)) || !counter(value.revision) || typeof value.visible !== "boolean" ||
      (value.projectLabel !== null && (typeof value.projectLabel !== "string" || value.projectLabel.length > 160)) ||
      (intent !== null && (intent.version !== 1 || !validId(intent.intentId) ||
        !Array.isArray(intent.providerIds) || intent.providerIds.length > 16 ||
        intent.providerIds.some((id) => !validId(id)) || new Set(intent.providerIds).size !== intent.providerIds.length ||
        (intent.workspaceId !== null && !validId(intent.workspaceId)) ||
        (intent.projectLabel !== null && (typeof intent.projectLabel !== "string" || intent.projectLabel.length > 160))))) {
    throw new Error("Invalid application surface context");
  }
  // Structural TypeScript inputs can contain additional data. Project only
  // declared presentation fields into the separately bundled entry.
  return Object.freeze({
    ...(value.knownWorkspaceIds === undefined ? {} : { knownWorkspaceIds: Object.freeze([...value.knownWorkspaceIds]) }),
    revision: value.revision, visible: value.visible, projectLabel: value.projectLabel,
    onboarding: intent === null ? null : Object.freeze({
      version: 1 as const, intentId: intent.intentId,
      providerIds: Object.freeze([...intent.providerIds]),
      workspaceId: intent.workspaceId, projectLabel: intent.projectLabel,
    }),
  });
}
// Retired unresolved mounts retain their reservation. Replacing a component
// cannot accumulate unlimited late owners by creating fresh session objects.
const pending = new WeakMap<ApplicationRuntime, number>();
const MAX_PENDING = 8;
const MOUNT_MS = 10_000;

export class ApplicationSurfaceSession {
  #identity: Readonly<SurfaceIdentity>;
  #presentation: Readonly<SurfacePresentation>;
  #intent: string | null;
  #intentScope: string;
  #owner: SurfaceOwner | null = null;
  #target: HTMLElement | null = null;
  #controller: AbortController | null = null;
  #timer: ReturnType<typeof setTimeout> | null = null;
  #mounting = false;
  #closed = false;
  #keptReview: KeptReviewDomain | null = null;
  #accountPresentation: AccountPresentationServices | null = null;
  #modals = new Set<{ destroy(): void }>();
  status: SurfaceStatus;
  constructor(
    private readonly root: HTMLElement,
    private readonly kind: SurfaceKind,
    identity: SurfaceIdentity,
    presentation: SurfacePresentation,
    private readonly runtime: ApplicationRuntime,
    private readonly extension: ApplicationExtension | null,
    private readonly host: HostSurfaceActions,
    private readonly changed: (status: SurfaceStatus) => void = () => {},
  ) {
    if (identity.version !== 1 || !validId(identity.viewId) || !counter(identity.attemptId) ||
      (identity.workspaceId !== null && !validId(identity.workspaceId)) || runtime.version !== 1 ||
      !["account", "account-settings", "account-home", "cloud-projects", "cloud-setup", "kept-review"].includes(kind) ||
      (extension !== null && (extension.version !== 1 || extension.id !== "chimaera-pro"))) {
      throw new Error("Invalid application surface");
    }
    this.#identity = Object.freeze({
      version: 1 as const, viewId: identity.viewId, attemptId: identity.attemptId,
      workspaceId: identity.workspaceId,
    });
    this.#presentation = snapshot(presentation);
    this.#intent = presentation.onboarding?.intentId ?? null;
    this.#intentScope = JSON.stringify(presentation.onboarding === null ? null : {
      id: presentation.onboarding.intentId, workspace: presentation.onboarding.workspaceId, providers: presentation.onboarding.providerIds,
    });
    this.status = extension === null ? "absent" : "loading";
  }
  #set(status: SurfaceStatus): void { this.status = status; this.changed(status); }
  #current(target: HTMLElement): boolean {
    return !this.#closed && this.#target === target && !this.#controller?.signal.aborted && this.host.current();
  }
  #clear(): void {
    if (this.#timer !== null) clearTimeout(this.#timer);
    this.#timer = null;
    this.#controller?.abort();
    const target = this.#target;
    this.#target = null;
    target?.remove();
    for (const modal of this.#modals) { try { modal.destroy(); } catch { /* Continue exact cleanup. */ } }
    this.#modals.clear();
    const owner = this.#owner;
    this.#owner = null;
    try { owner?.dispose(); } catch { /* Detached UI cannot block host cleanup. */ }
    const account = this.#accountPresentation; this.#accountPresentation = null;
    try { account?.dispose(); } catch { /* Retire exact presentation only. */ }
    const kept = this.#keptReview; this.#keptReview = null;
    try { kept?.dispose(); } catch { /* Retire only this presentation domain. */ }
  }
  start(): void {
    if (this.#closed || this.extension === null || this.#mounting || this.#owner !== null) return;
    if (!this.host.current() || (pending.get(this.runtime) ?? 0) >= MAX_PENDING) { this.#set("failed"); return; }
    const target = this.root.ownerDocument.createElement("div");
    target.style.display = "contents";
    this.root.appendChild(target);
    this.#target = target;
    const controller = new AbortController();
    this.#controller = controller;
    this.#mounting = true;
    pending.set(this.runtime, (pending.get(this.runtime) ?? 0) + 1);
    this.#set("loading");
    const check = (): void => { if (!this.#current(target)) throw new Error("Application surface retired"); };
    const actions: SurfaceActions = {
      openFile: async (path) => {
        check();
        if (this.kind !== "kept-review" || this.#identity.workspaceId === null || !relativeFilePath(path)) {
          throw new Error("Invalid application file action");
        }
        await this.host.openFile(path);
        check();
      },
      completeOnboarding: (id) => {
        check();
        if (!validId(id) || id !== this.#intent) throw new Error("Application intent retired");
        this.host.completeOnboarding(id);
      },
      close: () => { check(); this.cancel(); },
      modal: (node) => {
        check();
        if (!target.contains(node)) throw new Error("Invalid application modal");
        const owned = this.host.modal(node);
        if (!this.#current(target)) { owned.destroy(); throw new Error("Application surface retired"); }
        let live = true;
        const handle = { destroy: () => {
          if (!live) return;
          live = false;
          this.#modals.delete(handle);
          owned.destroy();
        } };
        this.#modals.add(handle);
        return handle;
      },
    };
    this.#timer = setTimeout(() => { this.#clear(); if (!this.#closed) this.#set("failed"); }, MOUNT_MS);
    // Calling mount in a promise also accounts for synchronous module throws.
    void Promise.resolve().then(async () => {
      check();
      if (this.kind === "kept-review" && this.host.keptReview !== undefined) {
        const kept = this.host.keptReview();
        this.#keptReview = kept;
        if (kept.workspaceId !== this.#identity.workspaceId) throw new Error("Application review scope mismatch");
        check();
      }
      if (this.kind !== "kept-review" && this.host.accountPresentation !== undefined) {
        const account = await this.host.accountPresentation(actions, controller.signal);
        if (!this.#current(target)) { account.dispose(); throw new Error("Application surface retired"); }
        this.#accountPresentation = account;
      }
      check();
      return this.extension!.mount(this.kind, target, {
        identity: this.#identity, presentation: this.#presentation, runtime: this.runtime,
        actions, signal: controller.signal, keptReview: this.#keptReview, accountPresentation: this.#accountPresentation,
      });
    }).then((owner) => {
      if (!this.#current(target)) {
        try { owner.dispose(); } finally { target.remove(); }
        return;
      }
      if (this.#timer !== null) clearTimeout(this.#timer);
      this.#timer = null;
      this.#owner = owner;
      owner.update(this.#presentation);
      this.#set("ready");
    }).catch(() => { this.#clear(); if (!this.#closed) this.#set("failed"); }).finally(() => {
      this.#mounting = false;
      pending.set(this.runtime, Math.max(0, (pending.get(this.runtime) ?? 1) - 1));
    });
  }
  update(value: SurfacePresentation): void {
    if (this.#closed) return;
    let next: Readonly<SurfacePresentation>;
    try { next = snapshot(value); } catch { this.#clear(); this.#set("failed"); return; }
    const intentScope = JSON.stringify(next.onboarding === null ? null : {
      id: next.onboarding.intentId, workspace: next.onboarding.workspaceId, providers: next.onboarding.providerIds,
    });
    if (next.revision < this.#presentation.revision || intentScope !== this.#intentScope) {
      this.close();
      return;
    }
    this.#presentation = next;
    try { this.#owner?.update(next); } catch { this.#clear(); this.#set("failed"); }
  }
  retry(): void { if (this.status === "failed") this.start(); }
  cancel(): void {
    if (this.#closed) return;
    // A retired view may clean up itself, never close a successor host surface.
    try { if (this.host.current()) this.host.close(); } finally { this.close(); }
  }
  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#clear();
    this.#set("closed");
  }
}

import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApplicationSurfaceSession, relativeFilePath,
  type ApplicationExtension, type ApplicationRuntime, type HostSurfaceActions,
  type SurfaceMount, type SurfaceOwner, type SurfacePresentation,
} from "./application";

// A controlled DOM target proves ownership/removal without claiming renderer
// or focus behavior. The Svelte wrapper still requires a live browser gate.
class Target {
  parent: Target | null = null;
  children: Target[] = [];
  style = { display: "" };
  ownerDocument = { createElement: () => new Target() };
  appendChild(child: Target) { child.parent = this; this.children.push(child); }
  remove() { if (this.parent) this.parent.children = this.parent.children.filter((c) => c !== this); this.parent = null; }
  contains(node: Target): boolean { return node === this || this.children.some((c) => c.contains(node)); }
}
const dom = (node: Target): HTMLElement => node as unknown as HTMLElement;
const observable = { subscribe: vi.fn(() => () => {}) };
function runtime(): ApplicationRuntime {
  return { version: 1, account: observable, visibility: observable, kept: { observe: () => observable }, onboarding: observable } as unknown as ApplicationRuntime;
}
const presentation = (revision = 1): SurfacePresentation => ({
  revision, visible: true, projectLabel: "Project", onboarding: {
    version: 1, intentId: "original-intent", providerIds: ["github"], workspaceId: "workspace-one", projectLabel: "Project",
  },
});
function host(): HostSurfaceActions {
  return { current: () => true, openFile: vi.fn(async () => {}), completeOnboarding: vi.fn(), close: vi.fn(), modal: vi.fn(() => ({ destroy: vi.fn() })) };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const flush = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };
function session(root: Target, services: ApplicationRuntime, module: ApplicationExtension | null, actions = host(), attempt = 1) {
  return new ApplicationSurfaceSession(dom(root), "kept-review", { version: 1, viewId: "original-pane", attemptId: attempt, workspaceId: "workspace-one" }, presentation(), services, module, actions);
}
afterEach(() => { vi.useRealTimers(); vi.clearAllMocks(); });

describe("application surface original owner", () => {
  it("absent module creates no DOM, subscriptions or account work", () => {
    const root = new Target(); const owner = session(root, runtime(), null);
    owner.start(); owner.close();
    expect(root.children).toHaveLength(0); expect(observable.subscribe).not.toHaveBeenCalled();
  });
  it("projects only declared identity, presentation and intent fields", async () => {
    let mount!: SurfaceMount; const updates = vi.fn();
    const value = { ...presentation(), extraRootData: "must-not-escape", onboarding: {
      ...presentation().onboarding!, extraIntentData: "must-not-escape",
    } };
    const identity = { version: 1 as const, viewId: "original-pane", attemptId: 1,
      workspaceId: "workspace-one", extraIdentityData: "must-not-escape" };
    const owner = new ApplicationSurfaceSession(dom(new Target()), "kept-review", identity, value,
      runtime(), { version: 1, id: "chimaera-pro", mount: async (_, __, m) => {
        mount = m; return { update: updates, dispose: vi.fn() };
      } }, host());
    owner.start(); await flush();
    expect(mount.identity).toEqual({ version: 1, viewId: "original-pane", attemptId: 1, workspaceId: "workspace-one" });
    expect(mount.presentation).toEqual(presentation());
    owner.update({ ...value, revision: 2 });
    expect(updates).toHaveBeenLastCalledWith(presentation(2)); owner.close();
  });
  it("Cancel closes the captured host once and retires modal and late owner", async () => {
    const root = new Target(); const actions = host(); const destroy = vi.fn(); const wait = deferred<SurfaceOwner>();
    actions.modal = () => ({ destroy }); let mount!: SurfaceMount; let target!: HTMLElement;
    const owner = session(root, runtime(), { version: 1, id: "chimaera-pro", mount: (_, t, m) => {
      mount = m; target = t; return wait.promise;
    } }, actions);
    owner.start(); await flush(); mount.actions.modal(target);
    owner.cancel(); owner.cancel();
    expect(actions.close).toHaveBeenCalledTimes(1); expect(destroy).toHaveBeenCalledTimes(1);
    expect(root.children).toHaveLength(0); expect(mount.signal.aborted).toBe(true); expect(owner.status).toBe("closed");
    const dispose = vi.fn(); wait.resolve({ update: vi.fn(), dispose }); await flush();
    expect(dispose).toHaveBeenCalledTimes(1);
  });
  it("stale Cancel cannot close a successor and throwing host close still cleans up", async () => {
    const actions = host(); actions.current = () => false;
    const retired = session(new Target(), runtime(), null, actions); retired.cancel();
    expect(actions.close).not.toHaveBeenCalled(); expect(retired.status).toBe("closed");
    const root = new Target(); const current = host(); current.close = vi.fn(() => { throw new Error("host failure"); });
    const owner = session(root, runtime(), { version: 1, id: "chimaera-pro", mount: async () => ({ update: vi.fn(), dispose: vi.fn() }) }, current);
    owner.start(); await flush(); expect(() => owner.cancel()).toThrow("host failure");
    expect(root.children).toHaveLength(0); expect(owner.status).toBe("closed");
  });
  it("passes the same runtime to split surfaces and coalesces visibility during mount", async () => {
    const services = runtime(); const mounts: SurfaceMount[] = [];
    const waits = [deferred<SurfaceOwner>(), deferred<SurfaceOwner>()];
    const module: ApplicationExtension = { version: 1, id: "chimaera-pro", mount: async (_, __, args) => { mounts.push(args); return waits[mounts.length - 1].promise; } };
    const one = session(new Target(), services, module); const two = session(new Target(), services, module, host(), 2);
    one.start(); two.start(); await flush();
    one.update({ ...presentation(2), visible: false });
    const updates = vi.fn(); waits[0].resolve({ update: updates, dispose: vi.fn() }); waits[1].resolve({ update: vi.fn(), dispose: vi.fn() }); await flush();
    expect(mounts.map((m) => m.runtime)).toEqual([services, services]);
    expect(updates).toHaveBeenLastCalledWith(expect.objectContaining({ visible: false, revision: 2 }));
    expect(mounts).toHaveLength(2); one.close(); two.close();
  });
  it("detaches the old target before successor and disposes late throwing owner", async () => {
    const root = new Target(); const services = runtime(); const wait = deferred<SurfaceOwner>();
    const module: ApplicationExtension = { version: 1, id: "chimaera-pro", mount: () => wait.promise };
    const old = session(root, services, module); old.start(); await flush(); const originalTarget = root.children[0]; old.close();
    const successor = session(root, services, { version: 1, id: "chimaera-pro", mount: async () => ({ update: vi.fn(), dispose: vi.fn() }) }, host(), 2);
    successor.start(); await flush(); const newTarget = root.children[0];
    const dispose = vi.fn(() => { throw new Error("private failure"); }); wait.resolve({ update: vi.fn(), dispose }); await flush();
    expect(originalTarget.parent).toBeNull(); expect(newTarget).not.toBe(originalTarget);
    expect(root.children).toEqual([newTarget]); expect(dispose).toHaveBeenCalledTimes(1); successor.close();
  });
  it("retired pane and changed provider scope refuse actions before host effect", async () => {
    let mount!: SurfaceMount; const actions = host(); let current = true; actions.current = () => current;
    const owner = session(new Target(), runtime(), { version: 1, id: "chimaera-pro", mount: async (_, __, m) => { mount = m; return { update: vi.fn(), dispose: vi.fn() }; } }, actions);
    owner.start(); await flush();
    await mount.actions.openFile("folder/file.txt"); expect(actions.openFile).toHaveBeenCalledTimes(1);
    current = false; await expect(mount.actions.openFile("folder/second.txt")).rejects.toThrow("retired");
    current = true; owner.update({ ...presentation(2), onboarding: { ...presentation().onboarding!, providerIds: ["claude"] } });
    expect(owner.status).toBe("closed"); expect(() => mount.actions.completeOnboarding("original-intent")).toThrow("retired");
    expect(actions.completeOnboarding).not.toHaveBeenCalled(); expect(actions.openFile).toHaveBeenCalledTimes(1);
  });
  it("checks scope after async file action and destroys exact shared modal once", async () => {
    const root = new Target(); const gate = deferred<void>(); const destroy = vi.fn(); const actions = host(); actions.openFile = () => gate.promise; actions.modal = vi.fn(() => ({ destroy }));
    let mount!: SurfaceMount; let target!: HTMLElement;
    const owner = session(root, runtime(), { version: 1, id: "chimaera-pro", mount: async (_, t, m) => { mount = m; target = t; return { update: vi.fn(), dispose: vi.fn() }; } }, actions);
    owner.start(); await flush(); expect(() => mount.actions.modal(dom(new Target()))).toThrow("Invalid");
    const modal = mount.actions.modal(target); const running = mount.actions.openFile("file.txt"); owner.close(); gate.resolve();
    await expect(running).rejects.toThrow("retired"); modal.destroy(); owner.close(); expect(destroy).toHaveBeenCalledTimes(1);
  });
  it("timeout retires target without allowing repeated pending Retry", async () => {
    vi.useFakeTimers(); const root = new Target(); const wait = deferred<SurfaceOwner>(); const mount = vi.fn(() => wait.promise);
    const owner = session(root, runtime(), { version: 1, id: "chimaera-pro", mount }); owner.start(); await flush();
    vi.advanceTimersByTime(10_000); for (let i = 0; i < 32; i++) owner.retry();
    expect(owner.status).toBe("failed"); expect(root.children).toHaveLength(0); expect(mount).toHaveBeenCalledTimes(1);
    wait.reject(new Error("late refusal")); await flush(); owner.retry(); await flush(); expect(mount).toHaveBeenCalledTimes(2); owner.close();
  });
  it("replacing sessions cannot exceed eight unresolved window mounts", async () => {
    const services = runtime(); const wait = deferred<SurfaceOwner>(); const mount = vi.fn(() => wait.promise); const owners = [];
    for (let i = 1; i <= 16; i++) { const owner = session(new Target(), services, { version: 1, id: "chimaera-pro", mount }, host(), i); owner.start(); owners.push(owner); await flush(); owner.close(); }
    expect(mount).toHaveBeenCalledTimes(8); wait.resolve({ update: vi.fn(), dispose: vi.fn() }); await flush(); expect(owners.every((o) => o.status === "closed")).toBe(true);
  });
  it("rejects traversal and wrong intent without widening the file opener", async () => {
    let mount!: SurfaceMount; const actions = host(); const owner = session(new Target(), runtime(), { version: 1, id: "chimaera-pro", mount: async (_, __, m) => { mount = m; return { update: vi.fn(), dispose: vi.fn() }; } }, actions);
    owner.start(); await flush();
    for (const path of ["", "/root/file", "../file", "a/../b", "a//b", "file\0suffix", "a".repeat(4097)]) { expect(relativeFilePath(path)).toBe(false); await expect(mount.actions.openFile(path)).rejects.toThrow("Invalid"); }
    expect(relativeFilePath("linux\\filename")).toBe(true);
    expect(() => mount.actions.completeOnboarding("successor-intent")).toThrow("retired"); expect(actions.openFile).not.toHaveBeenCalled(); owner.close();
  });
});

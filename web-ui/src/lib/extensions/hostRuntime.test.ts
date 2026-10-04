import { describe, expect, it, vi } from "vitest";
vi.mock("../net/plan", async () => { const { readable } = await import("svelte/store"); return {
  accountPlan: readable("unavailable"), accountSignedOut: readable(false), proOffered: readable(false) }; });
vi.mock("../pro/keptReviews.svelte", () => ({ keptReviews: { byWorkspace: {} } }));
import { captureKeptHost, type KeptHostSnapshot } from "./hostRuntime.svelte";
function fixture() {
  const original: KeptHostSnapshot = { paneId: "pane-one", workspaceId: "workspace-one", root: "/project", tab: {}, route: { current: () => true } };
  let now: KeptHostSnapshot | null = original;
  const callbacks = { openFile: vi.fn(), openFolder: vi.fn(), close: vi.fn(), modal: vi.fn(() => ({ destroy() {} })) };
  return { original, callbacks, host: captureKeptHost(original, () => now, callbacks), update(value: KeptHostSnapshot | null) { now = value; } };
}
describe("original kept pane actions", () => {
  it("refuses a same-key reopened tab and a replacement route without successor effects", async () => {
    const f = fixture(); f.update({ ...f.original, tab: {} });
    expect(f.host.actions.current()).toBe(false);
    await expect(f.host.actions.openFile("notes.md")).rejects.toThrow("retired");
    expect(() => f.host.actions.close()).toThrow("retired"); expect(f.callbacks.close).not.toHaveBeenCalled();
    f.update({ ...f.original, route: { current: () => true } });
    expect(() => f.host.openFolder()).toThrow("retired"); expect(f.callbacks.openFolder).not.toHaveBeenCalled();
  });
  it("opens only bounded relative paths at the captured root and closes captured tab identity", async () => {
    const f = fixture();
    await f.host.actions.openFile("a\\b.md"); expect(f.callbacks.openFile).toHaveBeenCalledWith("pane-one", "/project/a\\b.md");
    for (const path of ["../outside", "/outside", "a//b", "a/./b", "a\0b"]) {
      await expect(f.host.actions.openFile(path)).rejects.toThrow();
    }
    f.host.openFolder(); f.host.actions.close();
    expect(f.callbacks.openFolder).toHaveBeenCalledWith("pane-one", "/project");
    expect(f.callbacks.close).toHaveBeenCalledWith(f.original.tab, "pane-one");
    expect(() => f.host.actions.completeOnboarding("anything")).toThrow();
  });
});

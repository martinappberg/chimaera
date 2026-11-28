import { afterEach, describe, expect, it, vi } from "vitest";
import { PendingCalls, dispatch, onWebViewsChanged, registerWebCallTarget, webViews, type WebCallTarget } from "./webCalls";

const target = (over: Partial<WebCallTarget> = {}): WebCallTarget => ({
  workspace: "w1",
  plugin: "gs",
  view: "data",
  id: "genome",
  file: "x.bam",
  call: async (_id, method, args) => (method === "fail" ? { ok: false, error: "it failed" } : { ok: true, value: { method, args } }),
  ...over,
});

const offs: (() => void)[] = [];
afterEach(() => {
  while (offs.length > 0) offs.pop()!();
  vi.useRealTimers();
});

describe("the shown web nodes", () => {
  it("are listed for the hello, deduped, and changes are heard", () => {
    const heard = vi.fn();
    offs.push(onWebViewsChanged(heard));
    offs.push(registerWebCallTarget(target()));
    offs.push(registerWebCallTarget(target()));
    expect(webViews()).toEqual([{ workspace: "w1", plugin: "gs", view: "data", id: "genome", file: "x.bam" }]);
    expect(heard).toHaveBeenCalledTimes(2);
  });

  it("answer a call by plugin, view, node and file", async () => {
    offs.push(registerWebCallTarget(target()));
    const f = { plugin: "gs", node_view: "data", node: "genome", call: "wc-1", method: "look", args: { w: 2 } };
    await expect(dispatch(f)).resolves.toEqual({ value: { method: "look", args: { w: 2 } } });
    await expect(dispatch({ ...f, file: "x.bam", workspace: "w1" })).resolves.toBeTruthy();
    // The page's failure is a result; another workspace's node is not this one.
    await expect(dispatch({ ...f, method: "fail" })).resolves.toEqual({ error: "it failed" });
    await expect(dispatch({ ...f, workspace: "w2" })).rejects.toThrow("no longer shows");
    await expect(dispatch({ ...f, file: "y.bam" })).rejects.toThrow("no longer shows");
    await expect(dispatch({ ...f, node: "other" })).rejects.toThrow("no longer shows");
    await expect(dispatch({ ...f, method: "" })).rejects.toThrow("malformed");
  });
});

describe("a node's pending calls", () => {
  it("settle only by an id it sent, once", async () => {
    const calls = new PendingCalls();
    const sent: string[] = [];
    const p = calls.start("wc-1", () => (sent.push("wc-1"), true));
    expect(sent).toEqual(["wc-1"]);
    expect(calls.answer("wc-forged", true, 1, null)).toBe(false);
    expect(calls.answer("wc-1", true, { region: "chr17" }, null)).toBe(true);
    expect(calls.answer("wc-1", true, 2, null)).toBe(false);
    await expect(p).resolves.toEqual({ ok: true, value: { region: "chr17" } });
    const q = calls.start("wc-2", () => true);
    calls.answer("wc-2", false, null, "no handler for look");
    await expect(q).resolves.toEqual({ ok: false, error: "no handler for look" });
  });

  it("refuse when the page can't be asked or goes away; a time-out is the page's failure", async () => {
    vi.useFakeTimers();
    const calls = new PendingCalls();
    await expect(calls.start("a", () => false)).rejects.toThrow("isn't running");
    const late = calls.start("b", () => true, 1000);
    vi.advanceTimersByTime(1001);
    await expect(late).resolves.toEqual({ ok: false, error: "the page didn't answer in time" });
    const gone = calls.start("c", () => true);
    calls.failAll("the page closed");
    await expect(gone).rejects.toThrow("the page closed");
  });
});

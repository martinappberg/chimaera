import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { filesLoading, loadingOwner, parseSessionState, sessionStates, sessionTransfer } from "./loading";

describe("the workspace loading signals", () => {
  it("start empty: nothing is loading until an owner says so", () => {
    expect(get(filesLoading).size).toBe(0);
    expect(get(sessionStates).size).toBe(0);
  });

  it("show what an owner says and clear all of it when it retires", () => {
    const owner = loadingOwner("w-1");
    owner.files(true);
    owner.session("s-1", { kind: "loading" });
    owner.session("s-2", { kind: "note", text: "One quiet line." });
    expect([...get(filesLoading)]).toEqual(["w-1"]);
    expect(get(sessionStates).get("s-1")).toEqual({ kind: "loading" });
    expect(get(sessionStates).get("s-2")).toEqual({ kind: "note", text: "One quiet line." });
    owner.files(false);
    owner.session("s-1", null);
    expect(get(filesLoading).size).toBe(0);
    expect(get(sessionStates).has("s-1")).toBe(false);
    owner.dispose();
    expect(get(sessionStates).size).toBe(0);
    // A retired owner's late calls change nothing.
    owner.files(true);
    owner.session("s-3", { kind: "loading" });
    expect(get(filesLoading).size).toBe(0);
    expect(get(sessionStates).size).toBe(0);
  });

  it("keeps two owners apart: one retiring leaves the other's placeholders", () => {
    const a = loadingOwner("w-1");
    const b = loadingOwner("w-2");
    a.files(true); b.files(true);
    a.dispose();
    expect([...get(filesLoading)]).toEqual(["w-2"]);
    b.dispose();
    expect(get(filesLoading).size).toBe(0);
  });

  it("admits only the two closed states and a bounded printable line", () => {
    expect(parseSessionState({ kind: "loading" })).toEqual({ kind: "loading" });
    expect(parseSessionState({ kind: "note", text: "  Fine.  " })).toEqual({ kind: "note", text: "Fine." });
    expect(parseSessionState({ kind: "note", text: "" })).toBeNull();
    expect(parseSessionState({ kind: "note", text: "x".repeat(241) })).toBeNull();
    expect(parseSessionState({ kind: "note", text: "a\nb" })).toBeNull();
    expect(parseSessionState({ kind: "cover" })).toBeNull();
    expect(parseSessionState(null)).toBeNull();
  });

  it("passes a session row's transfer field through as two closed codes", () => {
    expect(sessionTransfer({})).toBeNull();
    expect(sessionTransfer({ transfer: null })).toBeNull();
    expect(sessionTransfer({ transfer: { state: "arriving", reason: null } })).toEqual({ state: "arriving", reason: null });
    expect(sessionTransfer({ transfer: { state: "failed", reason: "record_damaged" } })).toEqual({ state: "failed", reason: "record_damaged" });
    expect(sessionTransfer({ transfer: { state: "failed", reason: "Not <a> code" } })).toEqual({ state: "failed", reason: null });
    expect(sessionTransfer({ transfer: { state: 7 } })).toBeNull();
  });
});

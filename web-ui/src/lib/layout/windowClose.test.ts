import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { dirtyFiles, setDirty, setEditingHost } from "../shared/editing";
import type { UnsavedReply } from "../net/native";
import { WindowCloseGuard } from "./windowClose.svelte";

const FILES = ["/w/b.md", "/w/a.md"];

/** Saves the test settles by hand; discards clean the file at once. */
function host() {
  const saves = new Map<string, (ok: boolean) => void>();
  const discarded: string[] = [];
  setEditingHost({
    save: (path) =>
      new Promise<boolean>((resolve) =>
        saves.set(path, (ok) => {
          if (ok) setDirty(path, false);
          resolve(ok);
        }),
      ),
    discard: (path) => {
      discarded.push(path);
      setDirty(path, false);
    },
  });
  return { saves, discarded };
}

function guard() {
  const replies: [number, UnsavedReply][] = [];
  const g = new WindowCloseGuard(async (id, reply) => {
    replies.push([id, reply]);
  }, 15_000);
  return { g, replies };
}

describe("the native window close / quit dialog", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    for (const p of FILES) setDirty(p, true);
  });
  afterEach(() => {
    vi.useRealTimers();
    for (const p of get(dirtyFiles)) setDirty(p, false);
  });

  it("acks at once and lists every unsaved file in the window", () => {
    host();
    const { g, replies } = guard();
    g.receive({ id: 7, reason: "close" });
    expect(replies).toEqual([[7, "shown"]]);
    expect(g.prompt).toEqual({ id: 7, reason: "close", paths: ["/w/a.md", "/w/b.md"] });
  });

  it("proceeds without a dialog when everything was saved meanwhile", () => {
    host();
    for (const p of FILES) setDirty(p, false);
    const { g, replies } = guard();
    g.receive({ id: 3, reason: "quit" });
    expect(replies).toEqual([[3, "proceed"]]);
    expect(g.prompt).toBeNull();
  });

  it("proceeds once every save landed", async () => {
    const { saves } = host();
    const { g, replies } = guard();
    g.receive({ id: 1, reason: "close" });
    const done = g.save();
    await vi.advanceTimersByTimeAsync(0);
    saves.get("/w/a.md")?.(true);
    await vi.advanceTimersByTimeAsync(0);
    saves.get("/w/b.md")?.(true);
    await done;
    expect(replies).toEqual([
      [1, "shown"],
      [1, "proceed"],
    ]);
    expect(g.prompt).toBeNull();
  });

  it("keeps the window when a save fails, naming the file", async () => {
    const { saves } = host();
    const { g, replies } = guard();
    g.receive({ id: 1, reason: "close" });
    const done = g.save();
    await vi.advanceTimersByTimeAsync(0);
    saves.get("/w/a.md")?.(true);
    await vi.advanceTimersByTimeAsync(0);
    saves.get("/w/b.md")?.(false);
    await done;
    expect(replies).toEqual([[1, "shown"]]);
    expect(g.prompt?.paths).toEqual(["/w/b.md"]);
    expect(g.error).toContain("“b.md”");
    expect(g.saving).toBe(false);
  });

  it("hands control back at the deadline when the link is dead", async () => {
    host();
    const { g, replies } = guard();
    g.receive({ id: 1, reason: "quit" });
    const done = g.save();
    await vi.advanceTimersByTimeAsync(15_000);
    await done;
    expect(replies).toEqual([[1, "shown"]]);
    expect(g.error).toContain("not saved");
    expect(g.prompt?.paths).toEqual(["/w/a.md", "/w/b.md"]);
  });

  it("don't save discards every unsaved file, then proceeds", () => {
    const { discarded } = host();
    const { g, replies } = guard();
    g.receive({ id: 2, reason: "quit" });
    g.discard();
    expect(discarded.sort()).toEqual(["/w/a.md", "/w/b.md"]);
    expect(replies.at(-1)).toEqual([2, "proceed"]);
    expect(get(dirtyFiles).size).toBe(0);
  });

  it("cancel keeps the window, even mid-save, and a late save changes nothing", async () => {
    const { saves } = host();
    const { g, replies } = guard();
    g.receive({ id: 4, reason: "close" });
    const done = g.save();
    await vi.advanceTimersByTimeAsync(0);
    g.cancel();
    expect(replies.at(-1)).toEqual([4, "cancel"]);
    saves.get("/w/a.md")?.(true);
    await vi.advanceTimersByTimeAsync(0);
    saves.get("/w/b.md")?.(true);
    await done;
    expect(replies).toEqual([
      [4, "shown"],
      [4, "cancel"],
    ]);
    expect(g.prompt).toBeNull();
  });

  it("a repeated ask re-acks and keeps the open dialog under the newest ask", () => {
    host();
    const { g, replies } = guard();
    g.receive({ id: 5, reason: "close" });
    g.receive({ id: 5, reason: "quit" });
    expect(replies).toEqual([
      [5, "shown"],
      [5, "shown"],
    ]);
    expect(g.prompt).toEqual({ id: 5, reason: "quit", paths: ["/w/a.md", "/w/b.md"] });
    g.cancel();
    expect(replies.at(-1)).toEqual([5, "cancel"]);
  });
});

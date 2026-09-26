import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { dirtyUnder, saveDirtyFiles, setDirty, setEditingHost, unsavedDeleteNote } from "./editing";

/** A host whose saves the test settles by hand. */
function host() {
  const saves = new Map<string, (ok: boolean) => void>();
  const asked: string[] = [];
  setEditingHost({
    save: (path) => {
      asked.push(path);
      return new Promise<boolean>((resolve) =>
        saves.set(path, (ok) => {
          if (ok) setDirty(path, false);
          resolve(ok);
        }),
      );
    },
    discard: () => {},
  });
  return { saves, asked };
}

describe("saveDirtyFiles (the close dialog's Save)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    for (const p of ["/w/a.md", "/w/b.md", "/w/c.md"]) setDirty(p, true);
  });
  afterEach(() => {
    vi.useRealTimers();
    for (const p of ["/w/a.md", "/w/b.md", "/w/c.md"]) setDirty(p, false);
  });

  it("saves each file in turn and reports which landed", async () => {
    const { saves } = host();
    const done = saveDirtyFiles(["/w/a.md", "/w/b.md"], { deadlineMs: 15_000 });
    await vi.advanceTimersByTimeAsync(0);
    saves.get("/w/a.md")?.(true);
    await vi.advanceTimersByTimeAsync(0);
    saves.get("/w/b.md")?.(false);
    expect(await done).toEqual({ saved: ["/w/a.md"], unsaved: ["/w/b.md"], timedOut: false });
  });

  it("gives control back at the deadline when a save never answers (a dead link)", async () => {
    const { saves, asked } = host();
    const done = saveDirtyFiles(["/w/a.md", "/w/b.md", "/w/c.md"], { deadlineMs: 15_000 });
    await vi.advanceTimersByTimeAsync(0);
    saves.get("/w/a.md")?.(true);
    // b hangs (waiting for the link): the dialog must not wait for ever.
    await vi.advanceTimersByTimeAsync(15_000);
    expect(await done).toEqual({
      saved: ["/w/a.md"],
      unsaved: ["/w/b.md", "/w/c.md"],
      timedOut: true,
    });
    // c was never started once the deadline passed.
    expect(asked).toEqual(["/w/a.md", "/w/b.md"]);
  });

  it("reports a save that left keys typed meanwhile as unsaved", async () => {
    setEditingHost({ save: async () => true, discard: () => {} }); // saved, still dirty
    const r = await saveDirtyFiles(["/w/a.md"], { deadlineMs: 15_000 });
    expect(r).toEqual({ saved: [], unsaved: ["/w/a.md"], timedOut: false });
  });

  it("touches nothing once cancelled (the save may still land in the background)", async () => {
    const { saves } = host();
    let cancelled = false;
    const done = saveDirtyFiles(["/w/a.md", "/w/b.md"], {
      deadlineMs: 15_000,
      cancelled: () => cancelled,
    });
    await vi.advanceTimersByTimeAsync(0);
    cancelled = true;
    saves.get("/w/a.md")?.(true);
    expect(await done).toBeNull();
  });
});

describe("a delete confirmation names the unsaved edits it discards", () => {
  const dirty = new Set(["/w/docs/a.md", "/w/docs/sub/b.md", "/w/docs2/c.md", "/w/top.md"]);

  it("finds the dirty files at or under the path (never a sibling sharing its prefix)", () => {
    expect(dirtyUnder("/w/docs", dirty)).toEqual(["/w/docs/a.md", "/w/docs/sub/b.md"]);
    expect(dirtyUnder("/w/top.md", dirty)).toEqual(["/w/top.md"]);
    expect(dirtyUnder("/w/other", dirty)).toEqual([]);
  });

  it("names them, relative to a deleted folder, and stays quiet when there are none", () => {
    expect(unsavedDeleteNote("/w/top.md", dirty)).toBe(" Unsaved edits in “top.md” will be discarded.");
    expect(unsavedDeleteNote("/w/docs", dirty)).toBe(
      " Unsaved edits in “a.md” and “sub/b.md” will be discarded.",
    );
    expect(unsavedDeleteNote("/w", dirty)).toBe(
      " Unsaved edits in “docs/a.md”, “docs/sub/b.md”, “docs2/c.md” and 1 more will be discarded.",
    );
    expect(unsavedDeleteNote("/w/other", dirty)).toBe("");
  });
});

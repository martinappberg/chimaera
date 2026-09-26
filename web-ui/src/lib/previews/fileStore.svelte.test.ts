import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  fsFile: vi.fn(),
  fsMarkdown: vi.fn(),
  fsRawTicket: vi.fn(),
  fsTable: vi.fn(),
}));

vi.mock("./files", () => mocks);

import { FileEntry } from "./fileStore.svelte";

describe("FileEntry.ensureRawUrl", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("makes concurrent consumers wait for the same raw ticket", async () => {
    let resolveTicket!: (t: { url: string; name: string | null }) => void;
    mocks.fsRawTicket.mockReturnValue(
      new Promise<{ url: string; name: string | null }>((resolve) => {
        resolveTicket = resolve;
      }),
    );

    const entry = new FileEntry("plots/umap.pdf");
    let firstFinished = false;
    let secondFinished = false;
    const first = entry.ensureRawUrl().then(() => {
      firstFinished = true;
    });
    const second = entry.ensureRawUrl().then(() => {
      secondFinished = true;
    });

    await Promise.resolve();
    expect(mocks.fsRawTicket).toHaveBeenCalledTimes(1);
    expect(firstFinished).toBe(false);
    expect(secondFinished).toBe(false);

    resolveTicket({ url: "/api/fs/raw/ticket", name: "umap.pdf" });
    await Promise.all([first, second]);

    expect(entry.rawUrl).toBe("/api/fs/raw/ticket");
    expect(entry.rawName).toBe("umap.pdf");
    expect(firstFinished).toBe(true);
    expect(secondFinished).toBe(true);
  });
});

describe("FileEntry.isOwnWrite", () => {
  it("tells this window's own save from a rewrite elsewhere", async () => {
    const entry = new FileEntry("docs/notes.md");
    expect(entry.isOwnWrite("m1")).toBe(false);
    // An in-app save lands: its token is the entry's, and ours.
    entry.noteWrite("m2");
    expect(entry.mtime).toBe("m2");
    expect(entry.isOwnWrite("m2")).toBe(true);
    // The daemon's watch reports another version: not ours.
    mocks.fsFile.mockResolvedValue({ bytes: new Uint8Array(0), size: 0, truncated: false, mtime: "m3", hash: null });
    await entry.revalidate();
    expect(entry.mtime).toBe("m3");
    expect(entry.isOwnWrite("m3")).toBe(false);
    // A save whose reply carried no token claims nothing.
    entry.noteWrite(null);
    expect(entry.isOwnWrite(null)).toBe(false);
    expect(entry.isOwnWrite("m2")).toBe(false);
  });
});

// `changes` is what FileView remounts the spreadsheet / PDF / binary views on:
// learning the first mtime must not count (a cold open would mount twice), and
// a real change must count only after the refreshed ticket is in place.
describe("FileEntry.changes", () => {
  it("does not count the first mtime it learns", async () => {
    mocks.fsFile.mockResolvedValue({ mtime: "t1" });
    const entry = new FileEntry("data/book.xlsx");
    await entry.ensureMtime();
    expect(entry.mtime).toBe("t1");
    expect(entry.changes).toBe(0);
  });

  it("counts a change on disk once, after the refreshed ticket lands", async () => {
    mocks.fsFile.mockResolvedValue({ mtime: "t1" });
    mocks.fsRawTicket.mockResolvedValueOnce({ url: "/raw/old", name: "umap.pdf" });
    const entry = new FileEntry("plots/umap.pdf");
    await entry.ensureMtime();
    await entry.ensureRawUrl();

    let resolveTicket!: (t: { url: string; name: string | null }) => void;
    mocks.fsRawTicket.mockReturnValue(
      new Promise<{ url: string; name: string | null }>((resolve) => {
        resolveTicket = resolve;
      }),
    );
    mocks.fsFile.mockResolvedValue({ mtime: "t2" });
    const pending = entry.revalidate();
    await vi.waitFor(() => expect(mocks.fsRawTicket).toHaveBeenCalledTimes(2));
    expect(entry.changes).toBe(0);

    resolveTicket({ url: "/raw/new", name: "umap.pdf" });
    await pending;
    expect(entry.rawUrl).toBe("/raw/new");
    expect(entry.changes).toBe(1);

    await entry.revalidate(); // the same token again: no change
    expect(entry.changes).toBe(1);
  });

  it("counts a change that lands before the first mtime was learned", async () => {
    // The seed probe failed (or is still in flight) when the disk changed: the
    // view may hold the old bytes, so the token revalidate publishes counts.
    mocks.fsFile.mockRejectedValueOnce(new Error("offline"));
    const entry = new FileEntry("plots/umap.pdf");
    await entry.ensureMtime();
    expect(entry.mtime).toBeNull();

    mocks.fsFile.mockResolvedValue({ mtime: "t2" });
    await entry.revalidate();
    expect(entry.mtime).toBe("t2");
    expect(entry.changes).toBe(1);
  });

  it("counts an in-app save after its payloads refresh, and not a first token", async () => {
    mocks.fsFile.mockResolvedValue({ mtime: "t1" });
    const entry = new FileEntry("notes/a.bin");
    await entry.ensureMtime();

    entry.noteWrite("t2");
    expect(entry.mtime).toBe("t2");
    await vi.waitFor(() => expect(entry.changes).toBe(1));

    const fresh = new FileEntry("notes/b.bin");
    fresh.noteWrite("t1");
    await new Promise((resolve) => setTimeout(resolve, 0)); // let its refresh settle
    expect(fresh.mtime).toBe("t1");
    expect(fresh.changes).toBe(0);
  });
});

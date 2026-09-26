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
  beforeEach(() => {
    vi.clearAllMocks();
  });

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

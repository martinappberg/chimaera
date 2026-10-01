import { beforeEach, describe, expect, it, vi } from "vitest";
import { transferBlock, transferInto } from "./fileTransfer";
import { clearClip, copyFile, cutFile, fileClip, pasteInto } from "./fileClipboard.svelte";
import { fsCopyOp, fsMoveOp } from "./fsEvents";
import { reportUploadError } from "../net/uploads";

vi.mock("./fsEvents", () => ({ fsCopyOp: vi.fn(), fsMoveOp: vi.fn() }));
vi.mock("../net/uploads", () => ({
  reportUploadError: vi.fn(),
  trackFileOp: async (_label: string, op: () => Promise<string>) => {
    try { return await op(); } catch { return null; }
  },
}));

beforeEach(() => { vi.clearAllMocks(); clearClip(); });

describe("file transfer destinations", () => {
  it("blocks self, descendant, and root-directory transfers without matching sibling prefixes", () => {
    const dir = { path: "/work/data", kind: "dir" } as const;
    for (const operation of ["copy", "move"] as const) {
      for (const to of ["/work/data", "/work/data/", "/work/data/sub"]) {
        expect(transferBlock(dir, to, operation)).not.toBeNull();
      }
      expect(transferBlock(dir, "/work/database", operation)).toBeNull();
      expect(transferBlock({ path: "/", kind: "dir" }, "/tmp", operation)).not.toBeNull();
    }
  });

  it("permits a same-folder copy while treating a move as a no-op", async () => {
    const source = { path: "/work/note.txt", kind: "file" } as const;
    expect(transferBlock(source, "/work/", "move")).toBe("Already in this folder");
    expect(transferBlock(source, "/work", "copy")).toBeNull();
    for (const dest of ["/work", "/work/"]) await transferInto(source, dest, "move");
    expect(fsMoveOp).not.toHaveBeenCalled();
    expect(reportUploadError).not.toHaveBeenCalled();
    vi.mocked(fsCopyOp).mockResolvedValue("/work/note copy.txt");
    expect(await transferInto(source, "/work", "copy")).toBe("/work/note copy.txt");
    expect(fsCopyOp).toHaveBeenCalledWith(source.path, source.path, "unique");
  });

  it("moves directory paths with trailing slashes using the directory's name", async () => {
    vi.mocked(fsMoveOp).mockResolvedValue("/dest/data");
    expect(await transferInto({ path: "/work/data/", kind: "dir" }, "/dest/", "move"))
      .toBe("/dest/data");
    expect(fsMoveOp).toHaveBeenCalledWith("/work/data", "/dest/data");
  });

  it("uses the move API without opting into collision replacement", async () => {
    vi.mocked(fsMoveOp).mockResolvedValue("/dest/note.txt");
    expect(await transferInto({ path: "/note.txt", kind: "file" }, "/dest", "move"))
      .toBe("/dest/note.txt");
    expect(fsMoveOp).toHaveBeenCalledWith("/note.txt", "/dest/note.txt");
    expect(fsCopyOp).not.toHaveBeenCalled();
  });

  it("refuses a recursive transfer before calling the daemon", async () => {
    await transferInto({ path: "/work", kind: "dir" }, "/work/nested", "copy");
    expect(reportUploadError).toHaveBeenCalled();
    expect(fsCopyOp).not.toHaveBeenCalled();
  });
});

describe("clipboard transfer lifecycle", () => {
  it("keeps a newer clipboard selection when a previous move finishes", async () => {
    let finish!: (path: string) => void;
    vi.mocked(fsMoveOp).mockReturnValue(new Promise((resolve) => { finish = resolve; }));
    cutFile("/source/first", "file");
    const moving = pasteInto("/dest");
    copyFile("/source/second", "file");
    finish("/dest/first");
    await moving;
    expect(fileClip()?.path).toBe("/source/second");
  });

  it("clears only a successful cut, keeping failed moves retryable", async () => {
    cutFile("/source/first", "file");
    vi.mocked(fsMoveOp).mockRejectedValueOnce(new Error("destination exists"));
    await pasteInto("/dest");
    expect(fileClip()?.path).toBe("/source/first");
    vi.mocked(fsMoveOp).mockResolvedValueOnce("/dest/first");
    await pasteInto("/dest");
    expect(fileClip()).toBeNull();
  });
});

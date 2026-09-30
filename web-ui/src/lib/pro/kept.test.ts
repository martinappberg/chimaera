import { describe, expect, it } from "vitest";
import {
  backNote,
  deletedNote,
  isKeptCopy,
  keptCopyHint,
  keptErrorLine,
  keptNoticeWorkspace,
  keptOriginal,
  sizeLabel,
  useCloudForAllBody,
  useCloudHint,
} from "./kept";

describe("kept copies", () => {
  it("recognises the daemon's names and nothing else", () => {
    // canonical::kept_copy_name's cases.
    for (const name of ["notes.md.mine-20260929-1412", "a.mine-20260928-1200-3", "x.tar.gz.mine-20260101-0000"]) {
      expect(isKeptCopy(name), name).toBe(true);
    }
    for (const name of [
      "notes.mine-",
      "a.mine-2026092-1200",
      "a.mine-20260928x1200",
      "a.mine-20260928-1200-",
      ".mine-20260929-1412",
      "notes.md",
      "notes.md.mine-20260929-1412.bak",
    ]) {
      expect(isKeptCopy(name), name).toBe(false);
    }
  });

  it("names the file a copy sits beside", () => {
    expect(keptOriginal("notes.md.mine-20260929-1412")).toBe("notes.md");
    expect(keptOriginal("a.mine-20260928-1200-3")).toBe("a");
    expect(keptOriginal("notes.md")).toBeNull();
    expect(keptCopyHint("notes.md.mine-20260929-1412", "this Mac")).toContain("This Mac's version of notes.md");
  });

  it("maps a notice key to its project only", () => {
    expect(keptNoticeWorkspace("kept-both-w-abc_1")).toBe("w-abc_1");
    expect(keptNoticeWorkspace("s-123")).toBeNull();
    expect(keptNoticeWorkspace("kept-both-")).toBeNull();
    expect(keptNoticeWorkspace("kept-both-../x")).toBeNull();
  });
});

describe("words", () => {
  it("says where the work came back and how many files both sides changed", () => {
    expect(backNote(3, "this Mac")).toBe(
      "Back on this Mac. The cloud and this Mac both changed 3 files while apart.",
    );
    expect(backNote(1, "your computer")).toBe(
      "Back on your computer. The cloud and your computer both changed 1 file while apart.",
    );
  });

  it("says a discarded copy moves to the Trash, and only says deleted when it is", () => {
    expect(useCloudHint(false, true, "this Mac")).toBe("The cloud's version stays; this Mac's copy moves to the Trash");
    expect(useCloudHint(true, true, "this Mac")).toBe("The cloud deleted this file; this Mac's copy moves to the Trash");
    expect(useCloudHint(false, false, "this computer")).toBe("The cloud's version stays; this computer's copy is deleted");
    expect(useCloudForAllBody(3, true, "this Mac")).toBe(
      "This Mac's versions of 3 files will be moved to the Trash. The cloud's versions stay.",
    );
    expect(useCloudForAllBody(1, false, "this Mac")).toBe(
      "This Mac's versions of 1 file will be deleted: this folder's drive has no Trash. The cloud's versions stay.",
    );
    expect(deletedNote(1, "this Mac")).toBe("The Trash couldn't take this Mac's copy, so it was deleted.");
    expect(deletedNote(2, "this Mac")).toBe("The Trash couldn't take 2 of this Mac's copies, so they were deleted.");
  });

  it("never shows a raw code", () => {
    for (const code of ["not_here", "busy", "gone", "not_kept", "unsafe_path", "folder_unavailable", "failed", "surprise"]) {
      const line = keptErrorLine(code);
      expect(line).not.toContain("_");
      expect(line.length).toBeGreaterThan(10);
    }
  });

  it("sizes read like sizes", () => {
    expect(sizeLabel(1)).toBe("1 byte");
    expect(sizeLabel(812)).toBe("812 bytes");
    expect(sizeLabel(4200)).toBe("4.2 KB");
    expect(sizeLabel(1_300_000)).toBe("1.3 MB");
    expect(sizeLabel(52_000_000)).toBe("52 MB");
  });
});

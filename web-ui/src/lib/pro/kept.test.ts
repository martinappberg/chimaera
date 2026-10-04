import { describe, expect, it } from "vitest";
import { backNote, isKeptCopy, keptCopyHint, keptErrorLine, keptNoticeWorkspace, keptOriginal } from "./kept";

describe("shared kept copy naming and recovery", () => {
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

it("says where the work came back and how many files both sides changed", () => {
    expect(backNote(3, "this Mac")).toBe(
      "Back on this Mac. Both copies changed 3 files while apart.",
    );
    expect(backNote(1, "your computer")).toBe(
      "Back on your computer. Both copies changed 1 file while apart.",
    );
  });

it("never shows a raw code", () => {
    for (const code of ["not_here", "busy", "gone", "not_kept", "unsafe_path", "folder_unavailable", "failed", "surprise"]) {
      const line = keptErrorLine(code);
      expect(line).not.toContain("_");
      expect(line.length).toBeGreaterThan(10);
    }
  });
});

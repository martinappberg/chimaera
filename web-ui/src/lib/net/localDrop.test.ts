import { describe, expect, it } from "vitest";
import { decodeDroppedPaths, pairDroppedPaths, routeDrop, type DroppedItem, type DroppedPath } from "./localDrop";

const file = (name: string, size: number): DroppedItem => ({ name, size, dir: false });
const at = (path: string, size: number, dir = false): DroppedPath => ({
  path,
  name: path.slice(path.lastIndexOf("/") + 1),
  size,
  dir,
});

describe("decodeDroppedPaths", () => {
  it("keeps well-formed entries and drops the rest", () => {
    const good = { path: "/home/u/a.png", name: "a.png", size: 10, dir: false };
    expect(decodeDroppedPaths([good, { path: "/x", name: "x" }, null, "str", { ...good, size: "10" }])).toEqual([good]);
  });

  it("reads anything that is not a list as nothing reported", () => {
    for (const raw of [null, undefined, "paths", { path: "/a" }, 3]) expect(decodeDroppedPaths(raw)).toEqual([]);
  });
});

describe("pairDroppedPaths", () => {
  it("pairs each dropped file with the reported path of the same name and size", () => {
    const reported = [at("/home/u/Downloads/b.csv", 20), at("/home/u/Downloads/a.png", 10)];
    expect(pairDroppedPaths([file("a.png", 10), file("b.csv", 20)], reported)).toEqual([reported[1], reported[0]]);
  });

  it("never pairs by position: a stale report from another drag leaves the drop to upload", () => {
    const reported = [at("/home/u/Desktop/other.pdf", 10)];
    expect(pairDroppedPaths([file("a.png", 10)], reported)).toEqual([null]);
  });

  it("requires the size to agree for a file", () => {
    // Same name, different bytes: an earlier drag of another a.png.
    expect(pairDroppedPaths([file("a.png", 10)], [at("/home/u/old/a.png", 11)])).toEqual([null]);
  });

  it("pairs a folder by name alone", () => {
    const dir = at("/home/u/data", 0, true);
    expect(pairDroppedPaths([{ name: "data", size: 4096, dir: true }], [dir])).toEqual([dir]);
  });

  it("does not pair a file with a folder of the same name", () => {
    expect(pairDroppedPaths([file("data", 0)], [at("/home/u/data", 0, true)])).toEqual([null]);
  });

  it("uses a reported path once, so same-named files from two folders each get their own", () => {
    const reported = [at("/a/notes.md", 5), at("/b/notes.md", 5)];
    expect(pairDroppedPaths([file("notes.md", 5), file("notes.md", 5), file("notes.md", 5)], reported)).toEqual([
      reported[0],
      reported[1],
      null,
    ]);
  });

  it("compares names by their composed form", () => {
    const reported = [at("/home/u/résumé.pdf", 7)];
    expect(pairDroppedPaths([file("résumé.pdf", 7)], reported)).toEqual([reported[0]]);
  });

  it("ignores a reported entry that is not an absolute path", () => {
    expect(pairDroppedPaths([file("a.png", 10)], [{ path: "a.png", name: "a.png", size: 10, dir: false }])).toEqual([
      null,
    ]);
  });
});

describe("routeDrop", () => {
  const here = at("/home/u/Downloads/a.png", 10);

  it("references a file in place on this computer's own daemon when its path is known", () => {
    expect(routeDrop(file("a.png", 10), here, true)).toEqual({
      how: "reference",
      path: "/home/u/Downloads/a.png",
      kind: "file",
    });
  });

  it("uploads on a remote window even when a path came along", () => {
    // The path names this computer's file; the session's host is another.
    expect(routeDrop(file("a.png", 10), here, false)).toEqual({ how: "upload" });
  });

  it("uploads when the path is hidden (a browser, a paste of image bytes)", () => {
    expect(routeDrop(file("a.png", 10), null, true)).toEqual({ how: "upload" });
  });

  it("references a local folder, and refuses one it cannot reference", () => {
    const item: DroppedItem = { name: "data", size: 0, dir: true };
    expect(routeDrop(item, at("/home/u/data", 0, true), true)).toEqual({
      how: "reference",
      path: "/home/u/data",
      kind: "dir",
    });
    expect(routeDrop(item, null, true)).toEqual({ how: "refuse" });
    expect(routeDrop(item, at("/home/u/data", 0, true), false)).toEqual({ how: "refuse" });
  });
});

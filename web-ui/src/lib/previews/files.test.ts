import { describe, expect, it } from "vitest";
import { basename, dirLabel, dirname, joinPath, parentName, resolveDocPath } from "./files";

describe("files path helpers on a Unix host", () => {
  it("keeps today's results", () => {
    expect(basename("/a/b.rs")).toBe("b.rs");
    expect(basename("/a/b/")).toBe("b");
    expect(basename("/")).toBe("");
    expect(dirLabel("/")).toBe("/");
    expect(dirLabel("/a/src")).toBe("src/");
    expect(dirname("/a/b.rs")).toBe("/a");
    expect(dirname("/a")).toBe("/");
    expect(dirname("/")).toBe("/");
    expect(dirname("rel")).toBe("/");
    expect(parentName("/a/b.rs")).toBe("a");
    expect(parentName("/b.rs")).toBe("/");
    expect(parentName("src/a.rs")).toBe("src");
    expect(parentName("a.rs")).toBe("/");
    expect(joinPath("/", "x")).toBe("/x");
    expect(joinPath("/a", "x")).toBe("/a/x");
    expect(joinPath("", "x")).toBe("/x");
  });

  it("resolves document-relative references", () => {
    expect(resolveDocPath("/a/b/doc.md", "figs/p.png")).toBe("/a/b/figs/p.png");
    expect(resolveDocPath("/a/b/doc.md", "../x.png")).toBe("/a/x.png");
    expect(resolveDocPath("/a/b/doc.md", "../../../x.png")).toBe("/x.png");
    expect(resolveDocPath("/a/b/doc.md", "/abs/x.png")).toBe("/abs/x.png");
    expect(resolveDocPath("/doc.md", "./x.png")).toBe("/x.png");
    expect(resolveDocPath("doc.md", "x.png")).toBe("/x.png");
    expect(resolveDocPath("/a/doc.md", "C:/x.png")).toBe("/a/C:/x.png");
  });
});

describe("files path helpers on a Windows host (wire form)", () => {
  it("keeps the drive root", () => {
    expect(basename("C:/Users/me/a.rs")).toBe("a.rs");
    expect(basename("C:/")).toBe("");
    expect(dirLabel("C:/")).toBe("C:/");
    expect(dirLabel("//srv/share/")).toBe("//srv/share/");
    expect(dirLabel("C:/Users")).toBe("Users/");
    expect(dirname("C:/Users/me")).toBe("C:/Users");
    expect(dirname("C:/Users")).toBe("C:/");
    expect(dirname("C:/")).toBe("C:/");
    expect(dirname("//srv/share/x")).toBe("//srv/share/");
    expect(parentName("C:/Users/a.rs")).toBe("Users");
    expect(parentName("C:/a.rs")).toBe("C:/");
    expect(joinPath("C:/", "x")).toBe("C:/x");
    expect(joinPath("C:/a", "x")).toBe("C:/a/x");
  });

  it("resolves document-relative references against the drive", () => {
    expect(resolveDocPath("C:/a/b.md", "../x.png")).toBe("C:/x.png");
    expect(resolveDocPath("C:/a/b/doc.md", "figs/p.png")).toBe("C:/a/b/figs/p.png");
    expect(resolveDocPath("C:/a/b.md", "../../../x.png")).toBe("C:/x.png");
    expect(resolveDocPath("C:/a/b.md", "/x.png")).toBe("C:/x.png");
    expect(resolveDocPath("C:/a/b.md", "D:/y/x.png")).toBe("D:/y/x.png");
    expect(resolveDocPath("C:/b.md", "x.png")).toBe("C:/x.png");
    expect(resolveDocPath("//srv/share/d/b.md", "../x.png")).toBe("//srv/share/x.png");
  });
});

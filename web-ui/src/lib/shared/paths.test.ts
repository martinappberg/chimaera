import { describe, expect, it } from "vitest";
import {
  ancestors,
  basename,
  dirname,
  isAbsolute,
  isRoot,
  isUnder,
  joinPath,
  relativeTo,
  rootOf,
  trimSlash,
} from "./paths";

describe("daemon wire paths", () => {
  it("finds the root of every host form", () => {
    expect(rootOf("/home/u/x")).toBe("/");
    expect(rootOf("C:/Users/me")).toBe("C:/");
    expect(rootOf("C:")).toBe("C:/");
    expect(rootOf("//srv/share/dir")).toBe("//srv/share/");
    expect(rootOf("//srv/share")).toBe("//srv/share/");
    expect(rootOf("rel/x")).toBe("");
    expect(isAbsolute("C:/x")).toBe(true);
    expect(isAbsolute("x/C:/y")).toBe(false);
  });

  it("treats roots as roots", () => {
    for (const r of ["/", "C:/", "C:", "//srv/share", "//srv/share/"]) expect(isRoot(r)).toBe(true);
    expect(isRoot("C:/x")).toBe(false);
    expect(basename("C:/")).toBe("");
    expect(dirname("C:/")).toBe("C:/");
    expect(dirname("/")).toBe("/");
    expect(trimSlash("C:/")).toBe("C:/");
    expect(trimSlash("/a/b/")).toBe("/a/b");
  });

  it("splits and joins without crossing a root", () => {
    expect(basename("/a/b.txt")).toBe("b.txt");
    expect(basename("C:/Users/me/")).toBe("me");
    expect(dirname("/a")).toBe("/");
    expect(dirname("C:/Users")).toBe("C:/");
    expect(dirname("C:/Users/me/x.rs")).toBe("C:/Users/me");
    expect(dirname("//srv/share/d/f")).toBe("//srv/share/d");
    expect(dirname("//srv/share/d")).toBe("//srv/share/");
    expect(joinPath("C:/", "x")).toBe("C:/x");
    expect(joinPath("/a", "b")).toBe("/a/b");
  });

  it("relates paths to a workspace root", () => {
    expect(isUnder("C:/ws/src/a.rs", "C:/ws")).toBe(true);
    expect(isUnder("C:/wsx/a.rs", "C:/ws")).toBe(false);
    expect(isUnder("C:/anything", "C:/")).toBe(true);
    expect(relativeTo("C:/ws/src/a.rs", "C:/ws/")).toBe("src/a.rs");
    expect(relativeTo("/ws", "/ws")).toBe(".");
    expect(relativeTo("/other", "/ws")).toBeNull();
    expect(relativeTo("/etc/x", "/")).toBe("etc/x");
  });

  it("lists breadcrumbs from the root", () => {
    expect(ancestors("C:/Users/me")).toEqual(["C:/", "C:/Users", "C:/Users/me"]);
    expect(ancestors("/a/b")).toEqual(["/", "/a", "/a/b"]);
    expect(ancestors("/")).toEqual(["/"]);
  });
});

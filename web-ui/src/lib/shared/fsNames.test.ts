import { describe, expect, it } from "vitest";
import { validateEntryName } from "./fsNames";

describe("validateEntryName", () => {
  it("keeps the Unix rules, with or without a Unix directory", () => {
    for (const dir of [undefined, "/home/u", "/"]) {
      const v = (name: string, allowSlashes = false) => validateEntryName(name, { allowSlashes, dir });
      expect(v("a.txt")).toBeNull();
      expect(v("a:b*?.txt")).toBeNull();
      expect(v("con")).toBeNull();
      expect(v("trailing.")).toBeNull();
      expect(v("back\\slash")).toBeNull();
      expect(v("")).toBe("name is empty");
      expect(v("a/b")).toBe("name cannot contain /");
      expect(v("a/b", true)).toBeNull();
      expect(v("/a", true)).toBe("name cannot start or end with /");
      expect(v("a//b", true)).toBe("name cannot contain //");
      expect(v("a/../b", true)).toBe("name cannot contain . or .. segments");
      expect(v("a\x01")).toBe("name contains control characters");
    }
  });

  it("adds Windows' rules when the directory is on a Windows host", () => {
    for (const dir of ["C:/Users/me", "C:/", "//srv/share/x"]) {
      const v = (name: string, allowSlashes = false) => validateEntryName(name, { allowSlashes, dir });
      expect(v("a.txt")).toBeNull();
      expect(v("sub/a.txt", true)).toBeNull();
      expect(v("console.log")).toBeNull();
      expect(v("com0")).toBeNull();
      for (const bad of ["a\\b", "a:b", "a*", "a?", 'a"', "a<", "a>", "a|"]) expect(v(bad)).not.toBeNull();
      expect(v("name.")).toBe("name cannot end with a dot or space");
      expect(v("name ")).toBe("name cannot end with a dot or space");
      for (const dev of ["CON", "con", "Prn", "aux.txt", "NUL.tar.gz", "COM1", "lpt9.log"]) {
        expect(v(dev)).toMatch(/reserved name/);
      }
      expect(v("ok/nul", true)).toMatch(/reserved name/);
      expect(v("a\x1f")).toBe("name contains control characters");
    }
  });
});

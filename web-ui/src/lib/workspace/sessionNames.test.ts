import { describe, expect, it } from "vitest";
import { baseName } from "./sessions";

describe("baseName", () => {
  it("names the last segment on Unix and Windows daemon paths", () => {
    expect(baseName("/home/me/proj")).toBe("proj");
    expect(baseName("/home/me/proj/")).toBe("proj");
    expect(baseName("C:/Users/me/proj")).toBe("proj");
    expect(baseName("feature/login")).toBe("login");
  });

  it("a root names itself", () => {
    expect(baseName("/")).toBe("/");
    expect(baseName("C:/")).toBe("C:/");
    expect(baseName("//srv/share/")).toBe("//srv/share/");
  });
});

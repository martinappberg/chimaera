import { describe, expect, it } from "vitest";
import { relPath } from "./dash";

describe("relPath", () => {
  it("is workspace-relative under the root, verbatim elsewhere (Unix)", () => {
    expect(relPath("/ws", "/ws/src/a.rs")).toBe("src/a.rs");
    expect(relPath("/ws", "/wsx/a.rs")).toBe("/wsx/a.rs");
    expect(relPath("/ws", "/ws")).toBe("/ws");
    expect(relPath(null, "/ws/a.rs")).toBe("/ws/a.rs");
  });

  it("does the same for a Windows daemon's drive paths", () => {
    expect(relPath("C:/Users/me/ws", "C:/Users/me/ws/src/a.rs")).toBe("src/a.rs");
    expect(relPath("C:/", "C:/a.rs")).toBe("a.rs");
    expect(relPath("C:/ws", "D:/ws/a.rs")).toBe("D:/ws/a.rs");
  });
});

import { describe, expect, it } from "vitest";
import { revealLabel } from "./native";

describe("revealLabel", () => {
  it("uses each platform's own words", () => {
    expect(revealLabel("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15")).toBe("Reveal in Finder");
    expect(revealLabel("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Edg/130.0")).toBe("Show in Explorer");
    expect(revealLabel("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15")).toBe("Show in File Manager");
  });
});

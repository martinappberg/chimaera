import { describe, expect, it } from "vitest";
import { thoughtPreview } from "./thoughtText";

describe("thoughtPreview", () => {
  it("reads a codex section title as plain text", () => {
    expect(thoughtPreview("**Checking the screen**\n\nI need to look at the pane.")).toBe("Checking the screen");
  });

  it("drops a still-streaming title's unclosed markers", () => {
    expect(thoughtPreview("**Writing test", true)).toBe("Writing test");
  });

  it("follows the newest section while live, the first once settled", () => {
    const text = "**Reading the diff**\n\nLooks fine.\n\n**Waiting for build completion**\n\nThe build";
    expect(thoughtPreview(text, true)).toBe("Waiting for build completion");
    expect(thoughtPreview(text)).toBe("Reading the diff");
  });

  it("keeps a plain first line, minus inline markdown", () => {
    expect(thoughtPreview("  The user wants `foo_bar` **fixed**.\nMore.")).toBe("The user wants foo_bar fixed.");
    expect(thoughtPreview("## Plan\n- a")).toBe("Plan");
    expect(thoughtPreview("snake_case_name stays")).toBe("snake_case_name stays");
  });

  it("caps a long line", () => {
    expect(thoughtPreview("x".repeat(200))).toBe(`${"x".repeat(160)}…`);
  });
});

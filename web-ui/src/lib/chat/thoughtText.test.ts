import { describe, expect, it } from "vitest";
import { thoughtPreview } from "./thoughtText";

describe("thoughtPreview", () => {
  it("reads a codex section title as plain text", () => {
    expect(thoughtPreview("**Checking the screen**\n\nI need to look at the pane.")).toBe("Checking the screen");
  });

  it("drops a still-streaming title's unclosed markers", () => {
    expect(thoughtPreview("**Writing test")).toBe("Writing test");
  });

  it("follows the newest section, before and after the row settles", () => {
    const text = "**Reading the diff**\n\nLooks fine.\n\n**Waiting for build completion**\r\n\r\nThe build";
    expect(thoughtPreview(text)).toBe("Waiting for build completion");
    // A newer title still streaming (no closing `**`) doesn't count yet.
    expect(thoughtPreview(`${text}\n\n**Writing te`)).toBe("Waiting for build completion");
  });

  it("keeps a plain first line, minus inline markdown", () => {
    expect(thoughtPreview("  The user wants `foo_bar` **fixed**.\nMore.")).toBe("The user wants foo_bar fixed.");
    expect(thoughtPreview("## Plan\n- a")).toBe("Plan");
    expect(thoughtPreview("snake_case_name stays")).toBe("snake_case_name stays");
  });

  it("leaves code spans' contents alone", () => {
    expect(thoughtPreview("Editing `__init__.py` next")).toBe("Editing __init__.py next");
    expect(thoughtPreview("**Handling `**kwargs` in `run`**\n\nbody")).toBe("Handling **kwargs in run");
    expect(thoughtPreview("Use **`foo`** here")).toBe("Use foo here");
  });

  it("caps a long line", () => {
    expect(thoughtPreview("x".repeat(200))).toBe(`${"x".repeat(160)}…`);
  });
});

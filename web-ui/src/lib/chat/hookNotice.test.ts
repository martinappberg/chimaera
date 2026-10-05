import { describe, expect, it } from "vitest";
import { hookNotice } from "./hookNotice";

describe("hookNotice", () => {
  it("says the hook once for a multi-line hook notice", () => {
    const text = [
      "Stop says: Doc-drift check (warning — not blocking):",
      "Stop says:   - server: code changed, but its map wasn't updated.",
      "Stop says: If the change is user-visible, update the named",
      "Stop says: page; otherwise it's fine to skip.",
    ].join("\n");
    expect(hookNotice(text)).toEqual({
      hook: "Stop",
      lines: [
        "Doc-drift check (warning — not blocking):",
        "  - server: code changed, but its map wasn't updated.",
        "If the change is user-visible, update the named",
        "page; otherwise it's fine to skip.",
      ],
    });
  });

  it("keeps a matcher in the hook's name and an empty line in its text", () => {
    expect(hookNotice("PreToolUse:Bash says: one\nPreToolUse:Bash says:\nPreToolUse:Bash says: two")).toEqual({
      hook: "PreToolUse:Bash",
      lines: ["one", "", "two"],
    });
  });

  it("takes the name as claude wrote it, up to the first says", () => {
    // What a hook matched is not always a tool: a server's or a file's name
    // can hold spaces and punctuation.
    expect(
      hookNotice("Elicitation:my docs (beta) says: one\nElicitation:my docs (beta) says: two"),
    ).toEqual({ hook: "Elicitation:my docs (beta)", lines: ["one", "two"] });
    expect(hookNotice("Stop says: Simon says: sit\nStop says: stand")).toEqual({
      hook: "Stop",
      lines: ["Simon says: sit", "stand"],
    });
  });

  it("reads a one-line hook notice whose name is one word", () => {
    expect(hookNotice("SessionStart:startup says: hello")).toEqual({ hook: "SessionStart:startup", lines: ["hello"] });
  });

  it("leaves everything else alone", () => {
    expect(hookNotice("The agent says: hello")).toBeNull();
    expect(hookNotice("Stop says:")).toBeNull();
    expect(hookNotice("Stop says: one\nand a line of something else")).toBeNull();
    expect(hookNotice("Stop says: one\nSubagentStop says: two")).toBeNull();
    expect(hookNotice("Stop says:one\nStop says:two")).toBeNull();
    expect(hookNotice("MCP server config skipped: a, b")).toBeNull();
  });
});

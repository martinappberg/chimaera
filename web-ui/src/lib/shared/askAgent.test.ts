import { describe, expect, it } from "vitest";

import { askAgent, askText, registerAskAgent } from "./askAgent";

describe("askAgent", () => {
  it("appends the place as file:line-end", () => {
    expect(askText({ text: "Fix it.", file: "todo/X.md", line: 4, end_line: 9 })).toBe("Fix it.\n\n(todo/X.md:4-9)");
    expect(askText({ text: "Fix it.", file: "a.md" })).toBe("Fix it.\n\n(a.md)");
    expect(askText({ text: "Fix it." })).toBe("Fix it.");
  });

  it("goes through the registered handler, null without one", () => {
    expect(askAgent({ text: "x" })).toBeNull();
    const off = registerAskAgent((t) => `got ${t}`);
    expect(askAgent({ text: "x" })).toBe("got x");
    off();
    expect(askAgent({ text: "x" })).toBeNull();
  });
});

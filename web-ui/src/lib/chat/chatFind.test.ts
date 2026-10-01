import { describe, expect, it } from "vitest";
import { findMessages, MESSAGE_MATCH_LIMIT } from "./chatFind";
import type { ChatBlock } from "./store.svelte";

const message = (uid: number, text: string, kind = "message") => ({ uid, text, kind }) as ChatBlock;
describe("conversation find", () => {
  it("searches messages outside the rendered tail and preserves stable ids after trimming", () => {
    const blocks = Array.from({ length: 300 }, (_, i) => message(i + 20, i === 5 ? "the needle" : "other"));
    expect(findMessages(blocks, "needle", false).map((m) => m.uid)).toEqual([25]);
    expect(findMessages(blocks.slice(4), "needle", false).map((m) => m.uid)).toEqual([25]);
    expect(findMessages(blocks.slice(6), "needle", false)).toEqual([]);
  });
  it("includes user and agent messages but excludes private thoughts and tool output", () => {
    const blocks = [message(1, "needle", "user"), message(2, "needle", "agent_message"),
      message(3, "needle", "thought"), message(4, "needle", "tool")];
    expect(findMessages(blocks, "needle", false).map((m) => m.uid)).toEqual([1, 2]);
  });
  it("counts messages, bounds result lists, and retains a useful excerpt", () => {
    expect(findMessages([message(1, "needle needle")], "needle", false)).toHaveLength(1);
    expect(findMessages(Array.from({ length: 800 }, (_, i) => message(i, "needle")), "needle", false)).toHaveLength(MESSAGE_MATCH_LIMIT);
    const result = findMessages([message(9, "a".repeat(1000) + "needle" + "z".repeat(1000))], "needle", false)[0];
    expect(result.excerpt).toContain("needle");
    expect(result.excerpt.length).toBeLessThan(200);
  });
});

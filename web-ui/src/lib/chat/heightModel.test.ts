import { describe, expect, it } from "vitest";
import { blockWeight, HistoryWeights } from "./heightModel";
import type { ChatBlock } from "./store.svelte";

const message = (text: string, uid = 1): ChatBlock =>
  ({ uid, kind: "message", text, turnId: "t", sentAtMs: 0, forkSeq: 0, nativeTurnComplete: false }) as ChatBlock;
const tool = (uid = 1): ChatBlock =>
  ({ uid, kind: "tool", id: `t${uid}`, tool: "Bash", title: "", locations: [], status: "completed", content: null, denied: false, allowed: false, streaming: false, crossTurn: false, summary: null, command: null }) as ChatBlock;
const turnEnd = (artifacts: string[]): ChatBlock =>
  ({ uid: 9, kind: "turn_end", costUsd: null, outputTokens: 0, durationMs: 0, artifacts }) as ChatBlock;

describe("block height model", () => {
  it("weighs prose by wrapped length and hard breaks", () => {
    expect(blockWeight(message("x".repeat(200)), null, 100)).toBe(3);
    expect(blockWeight(message("a\nb\nc\nd"), null, 100)).toBe(1 + 1.5 + 1);
    expect(blockWeight(message("x".repeat(1000)), null, 100)).toBeGreaterThan(
      blockWeight(message("x".repeat(100)), null, 100) * 5,
    );
  });

  it("puts a run of tool calls on one line and a bare turn end on none", () => {
    expect(blockWeight(tool(), null, 100)).toBe(1);
    expect(blockWeight(tool(2), tool(1), 100)).toBe(0);
    expect(blockWeight(turnEnd([]), null, 100)).toBe(0);
    expect(blockWeight(turnEnd(["/a.png"]), null, 100)).toBe(12);
  });
});

describe("history weights", () => {
  const blocks = [message("x".repeat(100), 1), tool(2), tool(3), message("x".repeat(300), 4)];

  it("sums prefixes incrementally and maps a weight back to its block", () => {
    const weights = new HistoryWeights();
    expect(weights.upTo(blocks, 2, 100, "g")).toBe(2 + 1);
    expect(weights.upTo(blocks, 4, 100, "g")).toBe(2 + 1 + 0 + 4);
    expect(weights.indexAt(0)).toBe(0);
    expect(weights.indexAt(2.5)).toBe(1);
    // Block 2 (a grouped tool) has no extent; weight 3 is where block 3 begins.
    expect(weights.indexAt(3)).toBe(3);
    expect(weights.indexAt(6.9)).toBe(3);
    expect(weights.indexAt(1e9)).toBe(3);
  });

  it("rebuilds when the array front moved or the measure changed", () => {
    const weights = new HistoryWeights();
    weights.upTo(blocks, 4, 100, "g1");
    expect(weights.upTo(blocks.slice(1), 3, 100, "g2")).toBe(1 + 0 + 4);
    expect(weights.upTo(blocks.slice(1), 3, 50, "g2")).toBe(1 + 0 + 7);
  });
});

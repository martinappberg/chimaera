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

  it("counts prose as rendered, without link targets or markup", () => {
    const path = "/home/user/projects/example/results/2026-01/summaries/NAMES.md";
    expect(blockWeight(message(`See **[NAMES.md](${path})** for \`names\`.`), null, 100)).toBe(
      blockWeight(message("See NAMES.md for names."), null, 100),
    );
  });

  it("weighs inline embeds as the cards they render", () => {
    const base = blockWeight(message("Here."), null, 100);
    expect(blockWeight(message("Here.\n\n![UMAP](figs/umap.png)"), null, 100) - base).toBeCloseTo(18 + 1, 0);
    expect(blockWeight(message("Here.\n\n![p2](paper.pdf#page=2)"), null, 100) - base).toBeCloseTo(26 + 1, 0);
    // A document embeds as an inline chip.
    expect(blockWeight(message("Here.\n\n![notes](docs/notes.md)"), null, 100) - base).toBeLessThanOrEqual(1);
  });

  it("puts a run of tool calls on one line and a bare turn end on none", () => {
    expect(blockWeight(tool(), null, 100)).toBe(1);
    expect(blockWeight(tool(2), tool(1), 100)).toBe(0);
    expect(blockWeight(turnEnd([]), null, 100)).toBe(0);
    expect(blockWeight(turnEnd(["/a.png"]), null, 100)).toBe(12);
  });

  it("puts a run of thoughts and tool calls on the one line its fold renders", () => {
    const thought = (uid = 7): ChatBlock => ({ uid, kind: "thought", text: "x".repeat(400) }) as ChatBlock;
    expect(blockWeight(thought(), message("hi"), 100)).toBe(1);
    expect(blockWeight(thought(), tool(), 100)).toBe(0);
    expect(blockWeight(tool(), thought(), 100)).toBe(0);
    // The live turn's trailing run is not folded yet.
    expect(blockWeight(thought(), tool(), 100, false)).toBe(1);
    expect(blockWeight(tool(), thought(), 100, false)).toBe(1);
    expect(blockWeight(tool(2), tool(1), 100, false)).toBe(0);
  });

  it("adds a picture row to a user message with saved images", () => {
    const user = (attachmentPaths: string[]): ChatBlock =>
      ({ uid: 5, kind: "user", text: "look", attachments: attachmentPaths.length, attachmentPaths, checkpoint: null, id: null, origin: null, forkSeq: 0 }) as ChatBlock;
    expect(blockWeight(user(["/u/image-1.png"]), null, 100) - blockWeight(user([]), null, 100)).toBe(5.5);
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

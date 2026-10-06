import { describe, expect, it } from "vitest";
import { conversationProposals, sameProposals } from "./proposals";

const tool = (nativeName: string, nativeInput: unknown) => ({ kind: "tool", nativeName, nativeInput });

describe("conversationProposals", () => {
  it("reads only the chimaera setup proposal tool, in order, exactly as sent", () => {
    expect(conversationProposals([
      { kind: "message" },
      tool("Bash", { command: "npm ci" }),
      tool("mcp__chimaera__update_cloud_profile", { expected_revision: "0".repeat(64), setup_command: "npm ci  # deps" }),
      tool("mcp__other__update_cloud_profile", { setup_command: "rm -rf /" }),
      tool("mcp__chimaera__update_cloud_profile", { setup_command: "make" }),
    ])).toEqual(["npm ci  # deps", "make"]);
  });

  it("moves a repeated proposal to its latest place", () => {
    expect(conversationProposals([
      tool("mcp__chimaera__update_cloud_profile", { setup_command: "a" }),
      tool("mcp__chimaera__update_cloud_profile", { setup_command: "b" }),
      tool("mcp__chimaera__update_cloud_profile", { setup_command: "a" }),
    ])).toEqual(["b", "a"]);
  });

  it("ignores clears, blanks, oversized and malformed input", () => {
    expect(conversationProposals([
      tool("mcp__chimaera__update_cloud_profile", { setup_command: null }),
      tool("mcp__chimaera__update_cloud_profile", { setup_command: "  " }),
      tool("mcp__chimaera__update_cloud_profile", { setup_command: "x".repeat(16 * 1024 + 1) }),
      tool("mcp__chimaera__update_cloud_profile", "npm ci"),
      { kind: "tool", nativeName: "mcp__chimaera__update_cloud_profile" },
    ])).toEqual([]);
  });

  it("compares lists by value", () => {
    expect(sameProposals(["a"], ["a"])).toBe(true);
    expect(sameProposals(["a"], ["a", "b"])).toBe(false);
  });
});

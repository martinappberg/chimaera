import { describe, expect, it } from "vitest";
import { isRealModel, modelChoice, customModelSelection } from "./modelPicker";

const models = [
  { id: "default", label: "Default", resolved: "claude-opus-5-5[1m]" },
  { id: "opus[1m]", label: "Opus (1M context)", resolved: "claude-opus-5-5[1m]" },
  { id: "sonnet", label: "Sonnet", resolved: "claude-sonnet-4-6" },
];
describe("model picker identity", () => {
  it("keeps an explicit Default choice but prefers the named model for served ids", () => {
    expect(modelChoice(models, "default")?.id).toBe("default");
    expect(modelChoice(models, "claude-opus-5-5[1m]")?.id).toBe("opus[1m]");
    expect(modelChoice(models, "claude-opus-5-5")?.id).toBe("opus[1m]");
    expect(modelChoice(models, "sonnet")?.id).toBe("sonnet");
  });
  it("never offers a synthetic model or guesses the first row", () => {
    expect(isRealModel("<synthetic>")).toBe(false);
    expect(isRealModel(" ")).toBe(false);
    expect(modelChoice(models, "<synthetic>")).toBeUndefined();
    expect(modelChoice(models, "unknown")).toBeUndefined();
    expect(modelChoice(models, null)).toBeUndefined();
  });
  it("does not merge custom bracket suffixes or namespaced IDs", () => {
    for (const id of ["custom[beta]", "claude-opus-5-5[preview]", "provider/claude-opus-5-5[1m]", "custom[1m]"]) {
      const choices = [{ id, label: id }];
      expect(modelChoice(choices, id)?.id).toBe(id);
      expect(modelChoice(choices, id.replace(/\[[^\]]+\]$/, ""))).toBeUndefined();
    }
  });
});

describe("custom model IDs", () => {
  it("preserves exact provider spelling after trimming the form input", () => {
    for (const id of ["MiniMax-M2.7", "vendor/Model:Latest", "arn:aws:bedrock:us-east-1:123:model/Example", "model[preview]", "local-model:latest", "provider/model@revision"]) {
      expect(customModelSelection(`  ${id}  `)).toEqual({ id, error: null });
    }
  });
  it("bounds bytes rather than UTF-16 length", () => {
    expect(customModelSelection("a".repeat(256)).id).not.toBeNull();
    expect(customModelSelection("a".repeat(257)).id).toBeNull();
    expect(customModelSelection("é".repeat(128)).error).toContain("letters");
    expect(customModelSelection("é".repeat(129)).error).toContain("bytes");
  });
  it("rejects empty IDs, embedded whitespace, control characters, flags and placeholders", () => {
    for (const raw of ["", "   ", "two models", "a\tb", "a\nb", "a\u0000b", "a\u007fb", "a\u0085b", "a\u200db", "--model=Example", "-m", "<model-id>", "<synthetic>", "模型", "model?query", "model;command"]) {
      expect(customModelSelection(raw)).toEqual({ id: null, error: expect.any(String) });
    }
  });
});

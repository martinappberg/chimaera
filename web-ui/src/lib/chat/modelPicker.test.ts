import { describe, expect, it } from "vitest";
import { isRealModel, modelChoice } from "./modelPicker";

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
});

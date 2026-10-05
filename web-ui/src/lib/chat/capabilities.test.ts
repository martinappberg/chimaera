import { describe, expect, it } from "vitest";
import { legacyCapabilities, parseCapabilities } from "./capabilities";
import { ChatStore } from "./store.svelte";

describe("chat capabilities", () => {
  it("does not give an unknown harness Claude or Codex controls", () => {
    expect(legacyCapabilities("pi")).toEqual({ commands: [], image_input: false, custom_model: false });
    expect(legacyCapabilities("claude").commands).toContain("set_thinking");
    expect(legacyCapabilities("codex").commands).not.toContain("set_thinking");
  });

  it("fails closed on malformed declarations and bounds the catalog", () => {
    expect(parseCapabilities(null)).toEqual({ commands: [], image_input: false, custom_model: false });
    expect(parseCapabilities({ commands: ["set_model", "set_model", null], image_input: "true" }))
      .toEqual({ commands: ["set_model"], image_input: false, custom_model: false });
    expect(parseCapabilities({ commands: Array.from({ length: 100 }, (_, i) => `command_${i}`) }).commands).toHaveLength(64);
  });

  it("requires an explicit capability for unlisted models", () => {
    expect(legacyCapabilities("codex").custom_model).toBe(false);
    expect(parseCapabilities({ commands: ["set_model"] }).custom_model).toBe(false);
    expect(parseCapabilities({ custom_model: "true" }).custom_model).toBe(false);
    expect(parseCapabilities({ custom_model: true }).custom_model).toBe(true);
  });

  it("replays replacements without retaining the previous harness's controls", () => {
    const store = new ChatStore();
    const entry = (seq: number, capabilities: unknown) => ({ seq, ts: seq, ev: { type: "capabilities", capabilities } });
    store.apply(entry(1, legacyCapabilities("claude")));
    store.apply(entry(2, { commands: ["send"], image_input: false }));
    store.apply(entry(1, legacyCapabilities("claude")));
    expect(store.capabilities).toEqual({ commands: ["send"], image_input: false, custom_model: false });
  });
});


describe("shared provider catalogs", () => {
  it("parses initialization and catalog refreshes identically, excluding malformed or duplicate ids", () => {
    const store = new ChatStore();
    expect(store.modelCatalogReceived).toBe(false);
    const fields = { models: [null, { id: "a", label: "A", efforts: ["high", "high", null] }, { id: "a", label: "duplicate" }], modes: [{ id: "default", label: "Default" }, { id: "default" }], slash_commands: [{ name: "help", description: "Help" }, { name: "help" }] };
    store.apply({ seq: 1, ts: 1, ev: { type: "init", ...fields } });
    const models = JSON.stringify(store.models);
    expect(store.models).toHaveLength(1);
    expect(store.models[0].efforts).toEqual(["high"]);
    expect(store.modes).toHaveLength(1);
    expect(store.slashCommands).toHaveLength(1);
    store.apply({ seq: 2, ts: 2, ev: { type: "catalog", ...fields } });
    expect(JSON.stringify(store.models)).toEqual(models);
    store.apply({ seq: 3, ts: 3, ev: { type: "catalog" } });
    expect(store.models).toEqual([]);
    expect(store.modelCatalogReceived).toBe(true);
  });
  it("clears readiness and capabilities when a replaced journal restarts replay", () => {
    const store = new ChatStore();
    store.apply({ seq: 10, ts: 10, ev: { type: "init" } });
    store.apply({ seq: 11, ts: 11, ev: { type: "capabilities", capabilities: legacyCapabilities("claude") } });
    store.onReady({ id: "s", agent: "grok", alive: true, exit_status: null, native_session_id: "native", model: null, current_mode: null, pending_permission: false }, 0, 0);
    expect(store.initialized).toBe(false);
    expect(store.capabilities).toBeNull();
    expect(store.modelCatalogReceived).toBe(false);
  });
});

import { describe, expect, it } from "vitest";

// Public fixed transport/host retain no older provider-terminal route.
// Private presentation policy is tested beside the extracted views.
const sources = import.meta.glob<string>([
  "/src/lib/extensions/accountDaemon.ts",
  "/src/lib/net/native.ts",
  "/src/App.svelte",
], { query: "?raw", import: "default", eager: true });
const transport = sources["/src/lib/extensions/accountDaemon.ts"];

describe("an older cloud's sign-in", () => {
  it("has no terminal action anywhere in the panel or its transport", () => {
    expect(transport).toBeTypeOf("string");
    for (const source of [transport]) {
      expect(source).not.toMatch(/terminal/i);
      expect(source).not.toMatch(/provider-terminal/);
    }
    expect(sources["/src/lib/net/native.ts"]).not.toMatch(/open_provider_terminal/);
    expect(sources["/src/App.svelte"]).not.toMatch(/provider-terminal/);
  });
});

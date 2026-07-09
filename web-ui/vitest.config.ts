import { defineConfig } from "vitest/config";

// Unit tests for the web UI's PURE logic (the layout tree, etc.). The client has
// no other automated tests, so this is the net that guards a lib/ reorg or a
// layout refactor. Node environment — these tests import only pure .ts (no DOM,
// no Svelte runes); rune/component tests would need the svelte plugin + jsdom.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});

import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vitest/config";

// Unit tests for the web UI's PURE logic: the layout tree, and the chat store's
// reducer (`store.svelte.ts`). The client has no component/DOM tests — this is
// the net that guards a lib/ reorg, a layout refactor, or a store-reducer change
// (e.g. the pending-send ordering). The svelte plugin compiles the `$state`
// runes in `.svelte.ts` so the reducer is testable headless; no jsdom is needed
// because the store holds plain reactive state (no components mount here).
export default defineConfig({
  plugins: [svelte()],
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    // A tiny `location` shim (no jsdom) so a store test can import the reducer,
    // which transitively touches net/api's `#token=` bootstrap at module load.
    setupFiles: ["./vitest.setup.ts"],
  },
});

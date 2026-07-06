import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

export default defineConfig({
  plugins: [svelte()],
  build: {
    outDir: "dist",
  },
  server: {
    proxy: {
      "/api": process.env.CHIMAERA_DEV_TARGET ?? "http://127.0.0.1:9700",
      "/ws": {
        target: process.env.CHIMAERA_DEV_TARGET ?? "http://127.0.0.1:9700",
        ws: true,
      },
      // Ticketed raw file bytes (iframes/images) live outside /api.
      "/raw": process.env.CHIMAERA_DEV_TARGET ?? "http://127.0.0.1:9700",
    },
  },
});

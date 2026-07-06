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
    },
  },
});

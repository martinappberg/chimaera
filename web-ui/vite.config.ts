import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { defineConfig, type Plugin } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

/**
 * Dev-only: serve the local daemon's manifest (port + token) at
 * /dev/manifest so the dev page can authenticate without a hand-copied
 * token (`fetch('/dev/manifest')` → set `#token=`). The dev server binds
 * localhost and this middleware never exists in a production build; the
 * manifest is already readable by every process of the same user.
 */
function devManifest(): Plugin {
  return {
    name: "chimaera-dev-manifest",
    configureServer(server) {
      server.middlewares.use("/dev/manifest", (_req, res) => {
        try {
          const raw = readFileSync(join(homedir(), ".chimaera", "manifest.json"), "utf8");
          res.setHeader("content-type", "application/json");
          res.end(raw);
        } catch {
          res.statusCode = 404;
          res.end("{}");
        }
      });
    },
  };
}

export default defineConfig({
  plugins: [svelte(), devManifest()],
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

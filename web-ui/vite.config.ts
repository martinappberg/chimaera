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

/** Keep the always-loaded application shell under Vite's 500 kB warning
 * threshold. Feature chunks are intentionally excluded: heavyweight previews
 * and workbench surfaces load only when the user opens them. */
function entryBundleBudget(): Plugin {
  const maxBytes = 500_000;
  return {
    name: "chimaera-entry-bundle-budget",
    generateBundle(_options, bundle) {
      for (const output of Object.values(bundle)) {
        if (output.type !== "chunk" || !output.isEntry) continue;
        const bytes = new TextEncoder().encode(output.code).byteLength;
        if (bytes > maxBytes) {
          const largest = Object.entries(output.modules)
            .sort(([, a], [, b]) => b.renderedLength - a.renderedLength)
            .slice(0, 8)
            .map(([id, module]) => `  ${(module.renderedLength / 1000).toFixed(1)} kB  ${id}`)
            .join("\n");
          this.error(
            `${output.fileName} is ${(bytes / 1000).toFixed(1)} kB; the always-loaded entry budget is ${maxBytes / 1000} kB\nlargest entry modules:\n${largest}`,
          );
        }
      }
    },
  };
}

export default defineConfig({
  plugins: [svelte(), devManifest(), entryBundleBudget()],
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
      // Ticketed downloads and the browser pane's reverse proxy (WS-capable).
      "/download": process.env.CHIMAERA_DEV_TARGET ?? "http://127.0.0.1:9700",
      "/proxy": {
        target: process.env.CHIMAERA_DEV_TARGET ?? "http://127.0.0.1:9700",
        ws: true,
      },
    },
  },
});

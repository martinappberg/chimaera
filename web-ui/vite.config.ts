import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
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

/** Marp pins an older KaTeX and loads it as CommonJS; hand it the app's own
 * ES build instead, so slides and the shared math chunk run one KaTeX (and
 * the chunk chat loads never grows a second copy). */
function marpSharesKatex(): Plugin {
  const esm = fileURLToPath(new URL("./node_modules/katex/dist/katex.mjs", import.meta.url));
  return {
    name: "chimaera-marp-katex",
    enforce: "pre",
    resolveId(source, importer) {
      if (source === "katex" && importer !== undefined && /[\\/]@marp-team[\\/]marp-core[\\/]/.test(importer)) {
        return esm;
      }
      return null;
    },
  };
}

/** highlight.js grammars the slides view keeps real (what decks in this
 * workbench show); the rest of Marp's ~190 resolve to a plain stub. */
const MARP_HLJS_KEEP = [
  "bash", "c", "cpp", "css", "diff", "dockerfile", "fortran", "go", "ini", "java",
  "javascript", "json", "julia", "julia-repl", "latex", "makefile", "markdown", "matlab",
  "perl", "plaintext", "python", "python-repl", "r", "ruby", "rust", "scala", "shell",
  "sql", "typescript", "xml", "yaml",
];

export default defineConfig({
  plugins: [svelte(), devManifest(), entryBundleBudget(), marpSharesKatex()],
  resolve: {
    // Marp (the slides view, its own lazy chunk) imports all of MathJax and
    // every highlight.js grammar up front: stubs for those — slides render
    // math with KaTeX (see SlidesView), uncommon languages as plain text.
    alias: [
      {
        find: /^mathjax-full\/js\/.*$/,
        replacement: fileURLToPath(new URL("./src/lib/previews/stubs/mathjax.cjs", import.meta.url)),
      },
      {
        find: new RegExp(`^highlight\\.js/lib/languages/(?!(?:${MARP_HLJS_KEEP.join("|")})$)[\\w-]+$`),
        replacement: fileURLToPath(new URL("./src/lib/previews/stubs/hljs-language.cjs", import.meta.url)),
      },
    ],
  },
  // The tab-switch perf harness (src/lib/perf) compiles in only on request;
  // every other build tree-shakes it out.
  define: { __CHIMAERA_PERF__: JSON.stringify(process.env.CHIMAERA_PERF === "1") },
  build: {
    outDir: "dist",
    rollupOptions: {
      output: {
        // KaTeX + DOMPurify + the math policy (shared/math.ts) ride ONE chunk:
        // chat imports it statically (loaded with chat, as before) and the
        // markdown previews import it on demand at the first equation. Without
        // the pin Rollup folds a module into its static importer's chunk, and
        // the previews' dynamic import would drag the whole chat bundle in.
        manualChunks(id) {
          // Top-level packages only: a dependency's own nested copy must not
          // ride into the chunk chat loads.
          const nested = id.indexOf("node_modules") !== id.lastIndexOf("node_modules");
          if (!nested && /[\\/]node_modules[\\/](katex|dompurify)[\\/]/.test(id)) return "math";
          if (/[\\/]lib[\\/]shared[\\/]math\.ts$/.test(id)) return "math";
          return undefined;
        },
      },
    },
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

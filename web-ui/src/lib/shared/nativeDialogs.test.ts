import { describe, expect, it } from "vitest";

// The native app's web view (wry on macOS) leaves WKWebView's JavaScript
// panels unimplemented: `confirm()` answers false without showing anything,
// and `alert()` / `prompt()` never show either — so an action gated on one
// silently does nothing in the app while the browser preview works. Ask with
// `ConfirmDialog.svelte` (or inline, where the action lives) instead.
const sources = import.meta.glob<string>(["/src/**/*.{svelte,ts}", "!/src/**/*.test.ts"], {
  query: "?raw",
  import: "default",
  eager: true,
});

// A bare or `window.`/`globalThis.`-qualified call; a method (`.confirm(`),
// a `javascript:alert(…)` string, or a name quoted in prose is not one.
const CALL = /(?<![\w$.:#'"`])(?:window\.|globalThis\.)?(confirm|alert|prompt)\s*\(/g;

function withoutComments(src: string): string {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/(^|\s)\/\/.*$/gm, "$1");
}

describe("native dialogs", () => {
  it("scans the sources", () => {
    expect(Object.keys(sources).length).toBeGreaterThan(100);
  });

  it("are never called — the native app never shows them", () => {
    const calls: string[] = [];
    for (const [file, raw] of Object.entries(sources)) {
      const src = withoutComments(raw);
      for (const m of src.matchAll(CALL)) {
        // A module's own function of that name (ArtifactGallery's `confirm`).
        if (new RegExp(`\\bfunction\\s+${m[1]}\\b`).test(src)) continue;
        calls.push(`${file}: ${m[0]}`);
      }
    }
    expect(calls).toEqual([]);
  });
});

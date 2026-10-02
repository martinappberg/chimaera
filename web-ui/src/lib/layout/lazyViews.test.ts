import { describe, expect, it } from "vitest";
import { failedAssetUrls } from "./lazyViews";

describe("lazy view asset recovery", () => {
  const base = "http://127.0.0.1:9700/#token=secret";

  it("extracts and deduplicates same-origin production styles", () => {
    const error = new Error(
      "Unable to preload CSS for http://127.0.0.1:9700/assets/ChatView-a1.css\n" +
        "at http://127.0.0.1:9700/assets/index-b2.js:1:2\n" +
        "again http://127.0.0.1:9700/assets/ChatView-a1.css",
    );
    expect(failedAssetUrls(error, "css", base).map((url) => url.pathname)).toEqual([
      "/assets/ChatView-a1.css",
    ]);
  });

  it("recovers bounded logical-project assets as well as legacy host assets", () => {
    const base = "https://example.test/workspace/w-one/";
    const error = [
      "/workspace/w-one/assets/Settings-a.css",
      "/app/h-one/assets/Settings-b.css",
      "/workspace/w-one/other/assets/Settings-c.css",
      `/workspace/${"w".repeat(129)}/assets/Settings-d.css`,
      "https://elsewhere.test/workspace/w-one/assets/Settings-e.css",
    ].join(" ");
    expect(failedAssetUrls(error, "css", base).map((url) => url.pathname)).toEqual([
      "/workspace/w-one/assets/Settings-a.css",
      "/app/h-one/assets/Settings-b.css",
    ]);
  });

  it("rejects cross-origin and non-asset URLs", () => {
    const error = [
      "https://example.com/assets/SettingsView-a.js",
      "http://127.0.0.1:9700/src/SettingsView.svelte.js",
      "http://127.0.0.1:9700/assets/SettingsView-good.js",
    ].join(" ");
    expect(failedAssetUrls(error, "js", base).map((url) => url.pathname)).toEqual([
      "/assets/SettingsView-good.js",
    ]);
  });
});

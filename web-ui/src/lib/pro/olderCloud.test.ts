import { describe, expect, it } from "vitest";

// An older cloud still answers GitHub's Connect with a login terminal. The app
// never opens it (no window ever shows the cloud's own page, in the native app
// or a browser); the row says the cloud is being updated, with a Try again
// that only looks (docs/features/pro.md, Cloud readiness). Components have no
// DOM tests here, so this pins the sources.
const sources = import.meta.glob<string>([
  "/src/lib/pro/ProviderConnections.svelte",
  "/src/lib/pro/cloudTransport.ts",
  "/src/lib/net/native.ts",
  "/src/App.svelte",
], { query: "?raw", import: "default", eager: true });
const panel = sources["/src/lib/pro/ProviderConnections.svelte"];
const transport = sources["/src/lib/pro/cloudTransport.ts"];

describe("an older cloud's sign-in", () => {
  it("has no terminal action anywhere in the panel or its transport", () => {
    expect(panel).toBeTypeOf("string");
    expect(transport).toBeTypeOf("string");
    for (const source of [panel, transport]) {
      expect(source).not.toMatch(/terminal/i);
      expect(source).not.toMatch(/provider-terminal/);
    }
    expect(sources["/src/lib/net/native.ts"]).not.toMatch(/open_provider_terminal/);
    expect(sources["/src/App.svelte"]).not.toMatch(/provider-terminal/);
  });
  it("says the cloud is being updated in the row, with a Try again that only looks", () => {
    // Agent cards and repository rows alike.
    expect(panel.match(/cloudUpdateLine\(provider\.label\)/g)).toHaveLength(2);
    // The row's own Connect steps aside only while its sign-in runs in the row.
    const tryAgain = /\{#if waitsForUpdate\}<button class="button secondary" disabled=\{catalogFlight\} onclick=\{\(\) => void load\(\)\}>\{catalogFlight \? "Checking…" : "Try again"\}<\/button>\{:else if provider\.state !== "signed_in" && !inProgress\(provider\)\}/g;
    expect(panel.match(tryAgain)).toHaveLength(2);
    // `load` is the passive catalog read: it never wakes the cloud.
    expect(panel).toMatch(/async function load\(signal\?: AbortSignal\): Promise<void> \{[\s\S]*?const result = await peekCatalog\(signal\);/);
  });
  it("ends the older attempt and waits for the cloud's update instead of showing it", () => {
    expect(panel.match(/if \(olderCloudSignIn\(result\.connection\)\) \{ awaitUpdate\(result\.connection\); return; \}/g)).toHaveLength(2);
    expect(panel).toMatch(/if \(current && pressedUpdate\.length\) pressedUpdate = stillAwaitingUpdate\(pressedUpdate, providers\);/);
  });
});

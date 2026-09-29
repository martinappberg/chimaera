import { describe, expect, it } from "vitest";
import { CALLOUT_LOOKS, CALLOUT_MARKER, calloutLook, calloutRules, defaultCalloutTitle } from "./callouts";

describe("callouts", () => {
  it("reads the marker: type, fold mark and the space before a title", () => {
    const m = CALLOUT_MARKER.exec("> [!faq]- Why weight?");
    expect(m?.[1]).toBe("faq");
    expect(m?.[2]).toBe("-");
    expect(m?.[0]).toBe("> [!faq]- ");
    expect(CALLOUT_MARKER.exec("> [!överblick]")?.[1]).toBe("överblick");
    // The quote's `> ` exactly, as comrak reads GitHub's alerts.
    expect(CALLOUT_MARKER.exec(">[!NOTE]")).toBeNull();
    expect(CALLOUT_MARKER.exec("> [! NOTE]")).toBeNull();
  });

  it("keeps GitHub's five as GitHub's, maps Obsidian's aliases, and draws the rest as notes", () => {
    expect(calloutLook("IMPORTANT")).toBe("important");
    expect(calloutLook("Caution")).toBe("caution");
    expect(calloutLook("tldr")).toBe("abstract");
    expect(calloutLook("done")).toBe("success");
    expect(calloutLook("missing")).toBe("failure");
    expect(calloutLook("definition")).toBe("note");
    // An inherited name is no family.
    expect(calloutLook("constructor")).toBe("note");
  });

  it("titles a bare callout with its type as written, capitalized", () => {
    expect(defaultCalloutTitle("NOTE")).toBe("Note");
    expect(defaultCalloutTitle("faq")).toBe("Faq");
    expect(defaultCalloutTitle("överblick")).toBe("Överblick");
  });

  it("writes the base rule first, then one rule per family", () => {
    const rules = calloutRules(".md-doc").split("\n");
    expect(rules[0]).toMatch(/^\.md-doc \.markdown-alert\{--md-alert:var\(--syn-func\);.*--md-fold-icon:url\(/);
    expect(rules).toHaveLength(CALLOUT_LOOKS.length + 1);
    for (const k of CALLOUT_LOOKS) expect(rules.some((r) => r.startsWith(`.md-doc .markdown-alert-${k}{`))).toBe(true);
    expect(calloutRules("")).toMatch(/^\.markdown-alert\{/);
  });
});

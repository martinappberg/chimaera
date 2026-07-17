import { describe, expect, it } from "vitest";

import { inlineMarkdown } from "./inlineMarkdown";

describe("inlineMarkdown", () => {
  it("renders bold, code, and strikethrough", () => {
    expect(inlineMarkdown("**Workflow 1** — done")).toBe("<strong>Workflow 1</strong> — done");
    expect(inlineMarkdown("__done__")).toBe("<strong>done</strong>");
    expect(inlineMarkdown("ran `git log` ok")).toBe("ran <code>git log</code> ok");
    expect(inlineMarkdown("~~old~~ new")).toBe("<del>old</del> new");
  });

  it("strips a single leading block marker", () => {
    expect(inlineMarkdown("## Heading")).toBe("Heading");
    expect(inlineMarkdown("- a bullet")).toBe("a bullet");
    expect(inlineMarkdown("> a quote")).toBe("a quote");
    expect(inlineMarkdown("1. first")).toBe("first");
  });

  it("leaves plain status lines and identifiers untouched", () => {
    expect(inlineMarkdown("editing foo.rs")).toBe("editing foo.rs");
    // Single underscores in identifiers must NOT become emphasis.
    expect(inlineMarkdown("wrote snake_case_name")).toBe("wrote snake_case_name");
    // A lone/truncated marker stays literal, not a dangling tag.
    expect(inlineMarkdown("**Workflow")).toBe("**Workflow");
  });

  it("escapes HTML in untrusted output (no injection)", () => {
    expect(inlineMarkdown("<img src=x onerror=alert(1)>")).toBe(
      "&lt;img src=x onerror=alert(1)&gt;",
    );
    expect(inlineMarkdown("**<b>x</b>**")).toBe("<strong>&lt;b&gt;x&lt;/b&gt;</strong>");
    expect(inlineMarkdown('a & b "c"')).toBe('a &amp; b "c"');
  });

  it("does not render markers inside a code span", () => {
    expect(inlineMarkdown("`**not bold**`")).toBe("<code>**not bold**</code>");
  });
});

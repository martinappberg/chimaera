import { Marked } from "marked";
import { describe, expect, it } from "vitest";

import { markdownTables } from "./tables";

const parser = new Marked(markdownTables);

describe("markdownTables", () => {
  it("hosts a GFM table in a scroll container, default markup intact", () => {
    const html = parser.parse("| a | b |\n|---|--:|\n| 1 | 2 |\n\nafter\n", {
      async: false,
    }) as string;

    expect(html).toMatch(/^<div class="md-table"><table>\n<thead>/);
    expect(html).toContain('<td align="right">2</td>');
    expect(html).toMatch(/<\/table>\n<\/div>\n<p>after<\/p>\n$/);
  });

  it("composes with a later cell override through `this`", () => {
    const composed = new Marked(markdownTables, {
      renderer: {
        tablecell(token) {
          return `<td data-x>${this.parser.parseInline(token.tokens)}</td>`;
        },
      },
    });
    const html = composed.parse("| a |\n|---|\n| 1 |\n", { async: false }) as string;

    expect(html).toMatch(/^<div class="md-table"><table>/);
    expect(html.match(/<td data-x>/g)).toHaveLength(2);
  });
});

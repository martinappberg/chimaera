import { describe, expect, it } from "vitest";
import { Marked } from "marked";
import { base64Url, headingSlug, notebookMath, pickMime, svgUrl } from "./notebook";

describe("pickMime", () => {
  it("draws the richest mime present", () => {
    expect(pickMime({ "text/plain": "x", "image/png": "AA" })).toBe("image/png");
    expect(pickMime({ "text/plain": "x", "text/html": "<b>" })).toBe("text/html");
    expect(pickMime({ "text/plain": "x" })).toBe("text/plain");
    expect(pickMime({})).toBeNull();
    expect(pickMime(undefined)).toBeNull();
  });
});

describe("output urls", () => {
  it("strips line breaks from base64 payloads", () => {
    expect(base64Url("image/png", "AAAA\nBBBB\n")).toBe("data:image/png;base64,AAAABBBB");
  });

  it("gives an SVG its namespace (an <img> needs it)", () => {
    const url = svgUrl('<svg width="1"></svg>');
    expect(decodeURIComponent(url)).toContain('<svg xmlns="http://www.w3.org/2000/svg" width="1">');
    const kept = svgUrl('<svg xmlns="http://www.w3.org/2000/svg"></svg>');
    expect(decodeURIComponent(kept).match(/xmlns=/g)?.length).toBe(1);
  });
});

describe("headingSlug", () => {
  it("follows Jupyter's anchors", () => {
    expect(headingSlug("  Data loading  ")).toBe("Data-loading");
  });
});

describe("notebook math", () => {
  const md = new Marked({ async: false });
  md.use(notebookMath);
  const render = (s: string) => md.parse(s) as string;

  it("marks inline and display math as placeholders", () => {
    const html = render("Energy $E = mc^2$ and\n\n$$\n\\int_0^1 x\\,dx\n$$\n");
    expect(html).toContain('<span class="nb-math" data-display="0">E = mc^2</span>');
    expect(html).toContain('<span class="nb-math" data-display="1">\\int_0^1 x\\,dx</span>');
  });

  it("takes \\( \\), \\[ \\] and environments", () => {
    expect(render("a \\(x_1\\) b")).toContain('data-display="0">x_1</span>');
    expect(render("\\[ y \\]")).toContain('data-display="1">y</span>');
    expect(render("\\begin{align}a&=b\\end{align}")).toContain(
      'data-display="1">\\begin{align}a&#38;=b\\end{align}</span>',
    );
  });

  it("leaves currency alone", () => {
    const html = render("It cost $5 and then $10.");
    expect(html).not.toContain("nb-math");
  });

  it("escapes the source it carries", () => {
    expect(render("$a<b$")).toContain('data-display="0">a&#60;b</span>');
  });
});

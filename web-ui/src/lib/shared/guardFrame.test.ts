import { describe, expect, it } from "vitest";
import { guardedPage } from "./guardFrame";

describe("guardedPage", () => {
  it("frames the page under frame-src 'self' and relays only its own frame and its parent", () => {
    const doc = guardedPage("/raw/t-1/a.html?bridge=1&x=\"<y>", 'a "title"');
    expect(doc).toContain(`<meta http-equiv="Content-Security-Policy" content="frame-src 'self'">`);
    expect(doc).toContain('sandbox="allow-scripts"');
    expect(doc).not.toContain("allow-same-origin");
    // Attributes are escaped: the URL and title can't end the element.
    expect(doc).toContain('src="/raw/t-1/a.html?bridge=1&amp;x=&quot;&lt;y&gt;"');
    expect(doc).toContain('title="a &quot;title&quot;"');
    expect(doc).toContain("e.source===f.contentWindow)parent.postMessage");
    expect(doc).toContain("e.source===parent&&f.contentWindow!==null)f.contentWindow.postMessage");
  });

  it("wears the window's colour scheme from its first paint, then follows it", () => {
    expect(guardedPage("/raw/t-1/a.html", "a", "dark")).toContain("<style>html{color-scheme:dark}");
    expect(guardedPage("/raw/t-1/a.html", "a", "light")).toContain("<style>html{color-scheme:light}");
    const doc = guardedPage("/raw/t-1/a.html", "a", "dark");
    expect(doc).toContain("getComputedStyle(r).colorScheme");
    expect(doc).toContain("addEventListener('pagehide',()=>o.disconnect())");
    // The background stays see-through: the card or pane paints behind it.
    expect(doc).toContain("background:transparent");
  });
});

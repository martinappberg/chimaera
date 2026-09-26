import { describe, expect, it } from "vitest";
import { attrFetches, cssUrls, cssValueFetches, isFollowableHref, isInlineSource, stripExternalRels } from "./officeSafety";

describe("office documents stay offline", () => {
  it("allows only in-page sources", () => {
    expect(isInlineSource("blob:http://127.0.0.1:9700/1a2b")).toBe(true);
    expect(isInlineSource("data:image/png;base64,AAAA")).toBe(true);
    expect(isInlineSource("data:font/woff2;base64,AAAA")).toBe(true);
    expect(isInlineSource("data:text/html,<script>")).toBe(false);
    expect(isInlineSource("https://example.org/a.png")).toBe(false);
    expect(isInlineSource("//example.org/a.png")).toBe(false);
    expect(isInlineSource("/raw/abc")).toBe(false);
  });

  it("lets a reader follow web, mail and in-document links only", () => {
    expect(isFollowableHref("https://example.org")).toBe(true);
    expect(isFollowableHref("mailto:a@b.c")).toBe(true);
    expect(isFollowableHref("#_Toc1")).toBe(true);
    expect(isFollowableHref("javascript:alert(1)")).toBe(false);
    expect(isFollowableHref("file:///etc/passwd")).toBe(false);
  });

  it("finds every url() in a CSS value", () => {
    expect(cssUrls(`url("a") url('b') url(c)`)).toEqual(["a", "b", "c"]);
    expect(cssUrls(`url( "x\\"y" )`)).toEqual(['x\\"y']);
  });

  it("flags CSS that would fetch", () => {
    expect(cssValueFetches(`url("https://evil.test/x")`)).toBe(true);
    expect(cssValueFetches(`url("blob:http://h/1") format("woff2")`)).toBe(false);
    expect(cssValueFetches(`url(#clip)`)).toBe(false);
    expect(cssValueFetches(`image-set("a.png" 1x)`)).toBe(true);
    expect(cssValueFetches(`Calibri, sans-serif`)).toBe(false);
    // Custom properties keep raw tokens: an escape could spell url( later.
    expect(cssValueFetches(`\\75 rl(https://evil.test)`, "--x")).toBe(true);
    expect(cssValueFetches(`Calibri`, "--docx-majorHAnsi-font")).toBe(false);
  });

  it("flags attributes that reference outside the document", () => {
    expect(attrFetches("url(#grad1)")).toBe(false);
    expect(attrFetches("url(https://evil.test/f.svg#p)")).toBe(true);
    expect(attrFetches("#abc")).toBe(false);
    expect(attrFetches("\\75 rl(x)")).toBe(true);
    expect(attrFetches("see url docs")).toBe(false);
  });

  it("removes external load targets from relationships, keeping links", () => {
    const rels = `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="https://evil.test/track.png" TargetMode="External"/>
<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.org" TargetMode="External"/>
<Relationship TargetMode='External' Id="rId4" Type="http://schemas.microsoft.com/office/2007/relationships/media" Target="https://evil.test/v.mp4"></Relationship>
</Relationships>`;
    const out = stripExternalRels(rels);
    expect(out.removed).toBe(2);
    expect(out.xml).toContain('Id="rId1"');
    expect(out.xml).toContain('Id="rId3"');
    expect(out.xml).not.toContain("evil.test/track.png");
    expect(out.xml).not.toContain('Id="rId4"');
  });
});

import { describe, expect, it } from "vitest";
import {
  collapseUploadMentions,
  expandUploadMentions,
  keepsTokens,
  settleEdit,
  snapRange,
  tokenSpans,
  type UploadTokens,
} from "./uploadTokens";

const PAD = "/Users/g/.chimaera/uploads/s-92d4fb0c";
const SHOT = `@${PAD}/Skärmavbild-2026-09-28-kl.16.55.20.png`;

describe("upload short forms", () => {
  it("shows a dropped file by its name, where it was referenced", () => {
    const tokens: UploadTokens = new Map();
    expect(collapseUploadMentions(`${SHOT} `, tokens)).toBe("@Skärmavbild-2026-09-28-kl.16.55.20.png ");
    expect(tokens.get("@Skärmavbild-2026-09-28-kl.16.55.20.png")).toBe(SHOT);
  });

  it("round-trips a sentence to exactly the text a drop used to type", () => {
    const sent = `hello look at @${PAD}/filex.xyz and ${SHOT}, then compare`;
    const tokens: UploadTokens = new Map();
    const shown = collapseUploadMentions(sent, tokens);
    expect(shown).toBe("hello look at @filex.xyz and @Skärmavbild-2026-09-28-kl.16.55.20.png, then compare");
    expect(expandUploadMentions(shown, tokens)).toBe(sent);
  });

  it("keeps the quoted forms on both sides", () => {
    const spacedHome = '@"/Users/a b/.chimaera/uploads/s-0a1b2c3d/plot.png"';
    const tokens: UploadTokens = new Map();
    expect(collapseUploadMentions(`${spacedHome} `, tokens)).toBe("@plot.png ");
    expect(expandUploadMentions("see @plot.png", tokens)).toBe(`see ${spacedHome}`);

    const nonAscii = `@"${PAD}/bild-å"`;
    const shown = collapseUploadMentions(`${nonAscii} `, tokens);
    expect(shown).toBe('@"bild-å" ');
    expect(expandUploadMentions(shown, tokens)).toBe(`${nonAscii} `);
  });

  it("never touches workspace mentions, paths with a suffix, or other text", () => {
    const tokens: UploadTokens = new Map();
    const text = `@src/lib/a.ts @${PAD}/x.py:12 /tmp/uploads/s-92d4fb0c/y.png plain`;
    expect(collapseUploadMentions(text, tokens)).toBe(text);
    expect(tokens.size).toBe(0);
    expect(expandUploadMentions(text, tokens)).toBe(text);
  });

  it("keeps a mention whole when its short form would be ambiguous", () => {
    const tokens: UploadTokens = new Map();
    // The draft already says @plot.png — about a workspace file.
    expect(collapseUploadMentions(`@${PAD}/plot.png `, tokens, "compare @plot.png with")).toBe(
      `@${PAD}/plot.png `,
    );
    // Another pad's file of the same name.
    collapseUploadMentions(`@${PAD}/notes.md `, tokens);
    const other = "@/Users/g/.chimaera/uploads/s-11112222/notes.md";
    expect(collapseUploadMentions(`${other} `, tokens)).toBe(`${other} `);
    // The same file again reads short again.
    expect(collapseUploadMentions(`@${PAD}/notes.md `, tokens)).toBe("@notes.md ");
  });

  it("reads a short form as one, unless letters continue it", () => {
    const tokens: UploadTokens = new Map([["@plot.png", `@${PAD}/plot.png`]]);
    expect(expandUploadMentions("@plot.png", tokens)).toBe(`@${PAD}/plot.png`);
    expect(expandUploadMentions("see @plot.png.\nnext", tokens)).toBe(`see @${PAD}/plot.png.\nnext`);
    expect(expandUploadMentions("(see @plot.png)", tokens)).toBe(`(see @${PAD}/plot.png)`);
    // Some other file, typed: never rewritten.
    expect(expandUploadMentions("@plot.png.bak @plot.pngs @plot.png/x", tokens)).toBe(
      "@plot.png.bak @plot.pngs @plot.png/x",
    );
    // Written against a word (its space deleted): still the file, and the
    // agent gets the space claude needs to read it.
    expect(expandUploadMentions("x@plot.png", tokens)).toBe(`x @${PAD}/plot.png`);
    // A copy of part of the draft carries the whole path.
    expect(expandUploadMentions("look at @plot.png", tokens)).toBe(`look at @${PAD}/plot.png`);
  });
});

describe("a short form is one unit", () => {
  const tokens: UploadTokens = new Map([
    ["@plot.png", `@${PAD}/plot.png`],
    ["@b.csv", `@${PAD}/b.csv`],
  ]);

  it("finds each short form where it stands", () => {
    expect(tokenSpans("see @plot.png and @b.csv.", tokens)).toEqual([
      { start: 4, end: 13, token: "@plot.png" },
      { start: 18, end: 24, token: "@b.csv" },
    ]);
  });

  it("keeps the caret out of it and a selection from cutting it", () => {
    const spans = tokenSpans("see @plot.png now", tokens); // 4..13
    expect(snapRange(spans, 6, 6)).toEqual([4, 4]);
    expect(snapRange(spans, 11, 11)).toEqual([13, 13]);
    expect(snapRange(spans, 2, 7)).toEqual([2, 13]);
    expect(snapRange(spans, 9, 16)).toEqual([4, 16]);
    expect(snapRange(spans, 1, 3)).toEqual([1, 3]);
    expect(snapRange(spans, 13, 13)).toEqual([13, 13]);
  });

  it("knows, before an edit, whether it would glue text onto one", () => {
    // Typing against its end glues; a space first keeps it apart.
    expect(keepsTokens("see @plot.png", 13, 13, "x", tokens)).toBe(false);
    expect(keepsTokens("see @plot.png", 13, 13, " x", tokens)).toBe(true);
    expect(keepsTokens("see @plot.png.", 14, 14, "x", tokens)).toBe(false);
    expect(keepsTokens("see @plot.png.", 14, 14, " x", tokens)).toBe(true);
    // Punctuation after it is fine; a new mention or a path step is not.
    expect(keepsTokens("see @plot.png", 13, 13, ",", tokens)).toBe(true);
    expect(keepsTokens("see @plot.png", 13, 13, "@", tokens)).toBe(false);
    expect(keepsTokens("see @plot.png", 13, 13, " @", tokens)).toBe(true);
    expect(keepsTokens("see @plot.png", 13, 13, "/", tokens)).toBe(false);
    // Deleting the space before the next word glues; a trailing one is free.
    expect(keepsTokens("@plot.png x", 9, 10, "", tokens)).toBe(false);
    expect(keepsTokens("@plot.png ", 9, 10, "", tokens)).toBe(true);
    // Replacing it, or writing before it, is not gluing.
    expect(keepsTokens("see @plot.png", 4, 13, "x", tokens)).toBe(true);
    expect(keepsTokens("see @plot.png", 3, 4, "", tokens)).toBe(true);
  });

  it("parts text typed or pasted against its end", () => {
    expect(settleEdit("see @plot.png", "see @plot.pngx", 14, tokens)).toEqual({
      text: "see @plot.png x",
      caret: 15,
    });
    expect(settleEdit("@plot.png", "@plot.pnghello world", 20, tokens)).toEqual({
      text: "@plot.png hello world",
      caret: 21,
    });
  });

  it("will not glue it to the next word by deleting the space between", () => {
    expect(settleEdit("@plot.png next", "@plot.pngnext", 9, tokens)).toEqual({
      text: "@plot.png next",
      caret: 9,
    });
  });

  it("takes it whole when an edit reaches into it", () => {
    // A word deletion that took only "png".
    expect(settleEdit("see @plot.png", "see @plot.", 10, tokens)).toEqual({ text: "see ", caret: 4 });
    // A selection from the text before it into its middle, replaced.
    expect(settleEdit("see @plot.png now", "sX.png now", 2, tokens)).toEqual({ text: "sX now", caret: 2 });
    // Text dropped into its middle keeps the text, not the broken name.
    expect(settleEdit("@plot.png", "@plxot.png", 4, tokens)).toEqual({ text: "x", caret: 1 });
  });

  it("leaves every other edit alone", () => {
    expect(settleEdit("hi @plot.png", "hi@plot.png", 2, tokens)).toBeNull();
    expect(settleEdit("see ", "see @plot.png", 13, tokens)).toBeNull();
    expect(settleEdit("see @plot.png ", "see @plot.png x", 15, tokens)).toBeNull();
    expect(settleEdit("see @plot.png", "see @plot.png.", 14, tokens)).toBeNull();
    expect(settleEdit("no files", "no files!", 9, tokens)).toBeNull();
  });
});

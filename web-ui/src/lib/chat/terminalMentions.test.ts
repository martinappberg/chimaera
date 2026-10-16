import { describe, expect, it } from "vitest";
import { collapseTerminalMentions, mentionedTerminal, terminalLabel, terminalMentions } from "./terminalMentions";
import { expandUploadMentions, snapRange, tokenSpans } from "./uploadTokens";

const shells = [
  { id: "s-1234abcd", name: "bash", cwd: "/work/a" },
  { id: "s-5678abcd", name: "bash", cwd: "/work/b" },
  { id: "s-aabbccdd", name: "gpu shell" },
];

describe("terminal mentions", () => {
  it("parses quoted names and preserves surrounding punctuation", () => {
    const text = `use (@term:s-1234abcd), then @term:"gpu shell" and @term:'ssh shell'!`;
    const refs = terminalMentions(text);
    expect(refs.map((r) => r.target)).toEqual(["s-1234abcd", "gpu shell", "ssh shell"]);
    expect(refs.map((r) => text.slice(r.start, r.end))).toEqual(["@term:s-1234abcd", '@term:"gpu shell"', "@term:'ssh shell'"]);
    expect(terminalMentions("@term: end")).toEqual([]);
  });
  it("keeps quotes within unquoted names instead of shortening an ID prefix", () => {
    const text = `@term:s-1234abcd's @term:s-5678abcd"notes" @term:"unterminated`;
    expect(terminalMentions(text).map((r) => r.target)).toEqual(["s-1234abcd's", 's-5678abcd"notes"']);
    expect(collapseTerminalMentions(text, shells, new Map())).toBe(text);
  });
  it("never guesses between duplicate names; exact identity wins", () => {
    expect(mentionedTerminal("bash", shells)).toBeUndefined();
    expect(mentionedTerminal("s-5678abcd", shells)).toBe(shells[1]);
    expect(mentionedTerminal("GPU SHELL", shells)).toBe(shells[2]);
    expect(terminalLabel(shells[0], shells)).toBe("bash · a");
    expect(terminalLabel(shells[1], shells)).toBe("bash · b");
    const sameDirectory = shells.map((t) => ({ ...t, cwd: "/work" }));
    expect(terminalLabel(sameDirectory[0], sameDirectory)).toBe("bash · 1234abcd");
  });
  it("uses distinct readable chips while send, save and copy retain exact IDs", () => {
    const tokens = new Map<string, string>();
    const text = "use @term:s-1234abcd and @term:s-5678abcd then @term:s-aabbccdd.";
    const shown = collapseTerminalMentions(text, shells, tokens);
    expect(shown).toContain('@term:"bash · a"');
    expect(shown).toContain('@term:"gpu shell"');
    expect(expandUploadMentions(shown, tokens)).toBe(text);
    expect(tokenSpans(shown, tokens)).toHaveLength(3);
    const chip = tokenSpans(shown, tokens)[0];
    expect(snapRange(tokenSpans(shown, tokens), chip.start + 1, chip.end - 1)).toEqual([chip.start, chip.end]);
    expect(collapseTerminalMentions(text, shells, new Map())).toBe(shown);
  });
  it("resolves legacy names against daemon names, not UI duplicate suffixes", () => {
    const numbered = [
      { id: "s-one", name: "bash", referenceName: "bash" },
      { id: "s-two", name: "bash · 2", referenceName: "bash" },
    ];
    expect(mentionedTerminal("bash", numbered)).toBeUndefined();
    expect(mentionedTerminal("bash · 2", numbered)).toBeUndefined();
    expect(mentionedTerminal("s-two", numbered)).toBe(numbered[1]);
  });
  it("keeps typed names and unavailable IDs unchanged", () => {
    const text = 'use @term:bash @term:"gpu shell" @term:s-gone';
    const tokens = new Map<string, string>();
    expect(collapseTerminalMentions(text, shells, tokens)).toBe(text);
    expect(tokens.size).toBe(0);
  });
  it("does not capture a typed name elsewhere in the draft", () => {
    const tokens = new Map<string, string>();
    const shown = collapseTerminalMentions("@term:s-aabbccdd", shells, tokens, '@term:"gpu shell"');
    expect(shown).not.toBe('@term:"gpu shell"');
    expect(expandUploadMentions(`@term:"gpu shell" ${shown}`, tokens)).toBe('@term:"gpu shell" @term:s-aabbccdd');
  });
  it("never reassigns an old chip after a terminal rename or replacement (undo)", () => {
    const tokens = new Map<string, string>();
    const first = collapseTerminalMentions("@term:s-aabbccdd", shells, tokens);
    const replacement = [{ id: "s-new", name: "gpu shell" }];
    const second = collapseTerminalMentions("@term:s-new", replacement, tokens);
    expect(first).not.toBe(second);
    expect(expandUploadMentions(first, tokens)).toBe("@term:s-aabbccdd");
    expect(expandUploadMentions(second, tokens)).toBe("@term:s-new");
  });
});

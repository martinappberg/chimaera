import { describe, expect, it } from "vitest";
import { collapseCarriageReturns, fold256, nearest16, parseAnsi, plainStyle, runClass, stripAnsi } from "./ansi";

const ESC = "\u001b";

describe("parseAnsi", () => {
  it("passes plain text through as one unstyled run", () => {
    expect(parseAnsi("hello world").runs).toEqual([{ text: "hello world", style: null }]);
  });

  it("styles runs by SGR and resets on 0", () => {
    const { runs } = parseAnsi(`a${ESC}[1;31mred${ESC}[0mb`);
    expect(runs.map((r) => r.text)).toEqual(["a", "red", "b"]);
    expect(runs[1].style).toMatchObject({ fg: 1, bold: true });
    expect(runs[0].style).toBeNull();
    expect(runs[2].style).toBeNull();
  });

  it("maps bright, background and default codes", () => {
    const { runs } = parseAnsi(`${ESC}[92;44mx${ESC}[39my${ESC}[49mz`);
    expect(runs[0].style).toMatchObject({ fg: 10, bg: 4 });
    expect(runs[1].style).toMatchObject({ fg: null, bg: 4 });
    expect(runs[2].style).toBeNull();
  });

  it("folds 256-color and truecolor to the 16-color palette", () => {
    expect(fold256(9)).toBe(9);
    expect(fold256(196)).toBe(9); // bright red in the cube
    expect(fold256(232)).toBe(0); // near-black gray
    expect(nearest16(250, 250, 250)).toBe(15);
    const { runs } = parseAnsi(`${ESC}[38;5;34mg${ESC}[38;2;0;0;230mb`);
    expect(runs[0].style?.fg).toBe(2);
    expect(runs[1].style?.fg).toBe(4);
  });

  it("carries the open style into the next line", () => {
    const first = parseAnsi(`${ESC}[33mwarn start`);
    const second = parseAnsi("still yellow", first.state);
    expect(second.runs[0].style).toMatchObject({ fg: 3 });
  });

  it("swaps colors for inverse, defaults included", () => {
    const { runs } = parseAnsi(`${ESC}[7mx${ESC}[27;7;31my`);
    expect(runs[0].style).toMatchObject({ fg: "bg", bg: "fg" });
    expect(runs[1].style).toMatchObject({ fg: "bg", bg: 1 });
  });

  it("drops cursor, erase, OSC and charset escapes without residue", () => {
    const text = `${ESC}[2K${ESC}[1Gprog${ESC}]8;;http://x${ESC}\\link${ESC}]8;;${ESC}\\${ESC}(Bdone${ESC}[?25l`;
    expect(parseAnsi(text).runs.map((r) => r.text).join("")).toBe("proglinkdone");
    expect(stripAnsi(text)).toBe("proglinkdone");
  });

  it("strips stray control characters but keeps tabs", () => {
    expect(stripAnsi("a\u0007b\tc\u0000")).toBe("ab\tc");
  });

  it("names run classes", () => {
    expect(runClass({ ...plainStyle(), fg: 1, bg: "fg", bold: true, underline: true })).toBe("af1 abfg a-b a-u");
  });
});

describe("collapseCarriageReturns", () => {
  it("keeps only the last rewrite of a progress line", () => {
    expect(collapseCarriageReturns("10%\r50%\r100% done")).toBe("100% done");
  });

  it("drops a CRLF's carriage return", () => {
    expect(collapseCarriageReturns("line\r")).toBe("line");
  });

  it("keeps style escapes from the overwritten part", () => {
    const out = collapseCarriageReturns(`${ESC}[32mstart\rend`);
    expect(parseAnsi(out).runs).toEqual([{ text: "end", style: expect.objectContaining({ fg: 2 }) }]);
  });
});

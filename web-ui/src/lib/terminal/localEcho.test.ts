import { describe, expect, it } from "vitest";
import { EchoMatcher, isPredictable } from "./localEcho";

const enc = new TextEncoder();

describe("EchoMatcher", () => {
  it("echoed printables consume ghosts one-for-one", () => {
    const m = new EchoMatcher();
    m.predict(3); // typed "abc"
    expect(m.output(enc.encode("a"))).toBe(2);
    expect(m.output(enc.encode("bc"))).toBe(0);
  });

  it("any control byte clears every ghost (prompt redraw, CR/LF)", () => {
    const m = new EchoMatcher();
    m.predict(4);
    expect(m.output(enc.encode("a\r\n$ "))).toBe(0);
  });

  it("an escape sequence clears every ghost (zsh highlight repaint)", () => {
    const m = new EchoMatcher();
    m.predict(2);
    expect(m.output(enc.encode("\x1b[32ma\x1b[0m"))).toBe(0);
  });

  it("more printables than ghosts over-clears, never goes negative", () => {
    const m = new EchoMatcher();
    m.predict(1);
    expect(m.output(enc.encode("abcdef"))).toBe(0);
    expect(m.count).toBe(0);
  });

  it("counts UTF-8 characters, not bytes", () => {
    const m = new EchoMatcher();
    m.predict(2);
    // One 3-byte character echoes: one ghost consumed, one remains.
    expect(m.output(enc.encode("é"))).toBe(1);
  });

  it("output with no ghosts pending is a no-op", () => {
    const m = new EchoMatcher();
    expect(m.output(enc.encode("unrelated output"))).toBe(0);
  });

  it("DEL clears like a control byte (readline backspace echo)", () => {
    const m = new EchoMatcher();
    m.predict(2);
    expect(m.output(new Uint8Array([0x61, 0x7f]))).toBe(0);
  });
});

describe("isPredictable", () => {
  it("accepts plain printable ASCII", () => {
    expect(isPredictable("a")).toBe(true);
    expect(isPredictable("ls -la")).toBe(true);
  });

  it("rejects control input and non-ASCII", () => {
    expect(isPredictable("\r")).toBe(false);
    expect(isPredictable("\x7f")).toBe(false);
    expect(isPredictable("\x1b[A")).toBe(false);
    expect(isPredictable("é")).toBe(false);
    expect(isPredictable("")).toBe(false);
  });

  it("rejects a paste torrent past the ghost cap", () => {
    expect(isPredictable("x".repeat(65))).toBe(false);
  });
});

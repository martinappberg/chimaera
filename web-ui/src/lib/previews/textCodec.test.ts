import { describe, expect, it } from "vitest";
import { StreamingDecoder, decodeText, detectLineEnding, encodeText } from "./textCodec";

const enc = (s: string) => new TextEncoder().encode(s);
const roundTrip = (bytes: Uint8Array) => {
  const d = decodeText(bytes);
  return encodeText(d.text, d.codec);
};

describe("decodeText / encodeText", () => {
  it("round-trips LF with and without a final newline", () => {
    for (const s of ["a\nb\n", "a\nb", "", "\n", "no breaks"]) {
      const d = decodeText(enc(s));
      expect(d.failure).toBeNull();
      expect(d.codec).toEqual({ bom: false, eol: "\n" });
      expect(roundTrip(enc(s))).toEqual(enc(s));
    }
  });

  it("keeps CRLF: editor text is LF, the save restores CRLF", () => {
    const d = decodeText(enc("a\r\nb\r\n"));
    expect(d.text).toBe("a\nb\n");
    expect(d.codec.eol).toBe("\r\n");
    expect(encodeText("a\nb\nc\r\n".replace(/\r\n/g, "\n"), d.codec)).toEqual(enc("a\r\nb\r\nc\r\n"));
    expect(roundTrip(enc("x\r\ny"))).toEqual(enc("x\r\ny"));
  });

  it("keeps lone-CR line endings", () => {
    const d = decodeText(enc("a\rb\r"));
    expect(d.text).toBe("a\nb\n");
    expect(d.codec.eol).toBe("\r");
    expect(roundTrip(enc("a\rb\r"))).toEqual(enc("a\rb\r"));
  });

  it("flags mixed line endings instead of normalizing them", () => {
    expect(decodeText(enc("a\r\nb\nc")).failure).toBe("mixed-eol");
    expect(decodeText(enc("a\rb\nc")).failure).toBe("mixed-eol");
    expect(detectLineEnding("a\r\nb\r\n")).toBe("\r\n");
    expect(detectLineEnding("a\r\nb\n")).toBeNull();
  });

  it("strips a UTF-8 BOM for the editor and restores it on save", () => {
    const bytes = new Uint8Array([0xef, 0xbb, 0xbf, ...enc("héllo\r\n")]);
    const d = decodeText(bytes);
    expect(d.text).toBe("héllo\n");
    expect(d.codec).toEqual({ bom: true, eol: "\r\n" });
    expect(roundTrip(bytes)).toEqual(bytes);
  });

  it("marks invalid UTF-8 (Latin-1) instead of decoding it lossily into an editable buffer", () => {
    const latin1 = new Uint8Array([0x63, 0x61, 0x66, 0xe9, 0x0a]); // "café\n" in Latin-1
    const d = decodeText(latin1);
    expect(d.failure).toBe("invalid-utf8");
    expect(d.text).toBe("caf�\n");
  });

  it("decodes multi-byte UTF-8 exactly", () => {
    const s = "λ → 日本語 🎉\n";
    expect(decodeText(enc(s)).text).toBe(s);
    expect(roundTrip(enc(s))).toEqual(enc(s));
  });
});

describe("StreamingDecoder", () => {
  it("does not double a CRLF split across a chunk seam", () => {
    const bytes = enc("a\r\nb\r\nc");
    const d = new StreamingDecoder();
    const text = d.push(bytes.subarray(0, 2)) + d.push(bytes.subarray(2, 5)) + d.push(bytes.subarray(5), true);
    expect(text).toBe("a\nb\nc");
  });

  it("carries a split multi-byte character across chunks", () => {
    const bytes = enc("x🎉y");
    const d = new StreamingDecoder();
    expect(d.push(bytes.subarray(0, 3)) + d.push(bytes.subarray(3), true)).toBe("x🎉y");
  });

  it("flushes a trailing lone CR on the final chunk", () => {
    const d = new StreamingDecoder();
    expect(d.push(enc("a\r")) + d.push(new Uint8Array(0), true)).toBe("a\n");
  });
});

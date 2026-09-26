/**
 * Byte-faithful text decoding for the editor. The rule (document-workbench
 * plan, "the file is the truth"): a file the user did not change must never
 * be rewritten differently, and one they did must keep everything they did
 * not touch — line endings, the UTF-8 BOM, the final newline.
 *
 * The editor always holds "\n"-joined text (CodeMirror's default split on
 * \r\n | \r | \n). A file whose line breaks are uniform is re-joined with its
 * own break on save, so the bytes round-trip exactly. We deliberately do NOT
 * set `EditorState.lineSeparator`: with it set, a paste of LF text into a CRLF
 * document keeps bare "\n" characters INSIDE lines and the save writes a
 * mixed file. Mixed endings cannot be reproduced after an edit, so those
 * files open read-only instead of being silently normalized.
 */

export type LineEnding = "\n" | "\r\n" | "\r";

export interface TextCodec {
  /** The file started with a UTF-8 BOM (stripped for the editor, re-added on save). */
  bom: boolean;
  /** The file's one line break; "\n" for a file with no breaks at all. */
  eol: LineEnding;
}

export type DecodeFailure = "invalid-utf8" | "mixed-eol";

export interface Decoded {
  /** Editor text: BOM stripped, breaks normalized to "\n". Always present
   *  (a lossy decode for an invalid file, so it can still be viewed). */
  text: string;
  codec: TextCodec;
  /** Set when the text cannot be edited without changing bytes the user did
   *  not touch. */
  failure: DecodeFailure | null;
}

export const DEFAULT_CODEC: TextCodec = { bom: false, eol: "\n" };

const BOM = [0xef, 0xbb, 0xbf] as const;

function hasBom(bytes: Uint8Array): boolean {
  return bytes.length >= 3 && bytes[0] === BOM[0] && bytes[1] === BOM[1] && bytes[2] === BOM[2];
}

/** Classify the file's line breaks; null = more than one kind. */
export function detectLineEnding(raw: string): LineEnding | null {
  let crlf = 0;
  let lf = 0;
  let cr = 0;
  for (let i = 0; i < raw.length; i++) {
    const c = raw.charCodeAt(i);
    if (c === 13) {
      if (raw.charCodeAt(i + 1) === 10) {
        crlf++;
        i++;
      } else {
        cr++;
      }
    } else if (c === 10) {
      lf++;
    }
  }
  const kinds = (crlf > 0 ? 1 : 0) + (lf > 0 ? 1 : 0) + (cr > 0 ? 1 : 0);
  if (kinds > 1) return null;
  if (crlf > 0) return "\r\n";
  if (cr > 0) return "\r";
  return "\n";
}

/** Normalize every break (\r\n, lone \r, \n) to "\n" — the editor's form. */
export function normalizeBreaks(raw: string): string {
  return raw.indexOf("\r") < 0 ? raw : raw.replace(/\r\n?/g, "\n");
}

/**
 * Decode a COMPLETE file's bytes. UTF-8 is decoded fatally so Latin-1/CP1252
 * never opens editable (a save would write U+FFFD over every such byte).
 */
export function decodeText(bytes: Uint8Array): Decoded {
  const bom = hasBom(bytes);
  const body = bom ? bytes.subarray(3) : bytes;
  let raw: string;
  let failure: DecodeFailure | null = null;
  try {
    raw = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(body);
  } catch {
    raw = new TextDecoder("utf-8", { fatal: false, ignoreBOM: true }).decode(body);
    failure = "invalid-utf8";
  }
  const eol = detectLineEnding(raw);
  if (eol === null && failure === null) failure = "mixed-eol";
  return { text: normalizeBreaks(raw), codec: { bom, eol: eol ?? "\n" }, failure };
}

/** Serialize editor text back to the file's bytes (its breaks, its BOM). */
export function encodeText(text: string, codec: TextCodec): Uint8Array {
  const joined = codec.eol === "\n" ? text : text.replace(/\n/g, codec.eol);
  const body = new TextEncoder().encode(joined);
  if (!codec.bom) return body;
  const out = new Uint8Array(body.length + 3);
  out.set(BOM, 0);
  out.set(body, 3);
  return out;
}

/**
 * Incremental decode for view-only paging (files past the edit cap). Carries a
 * split UTF-8 sequence AND a trailing "\r" across chunk seams: a CRLF split
 * between two chunks would otherwise normalize to two line breaks.
 */
export class StreamingDecoder {
  private readonly decoder = new TextDecoder("utf-8", { fatal: false });
  private pendingCr = false;

  push(bytes: Uint8Array, final = false): string {
    let raw = (this.pendingCr ? "\r" : "") + this.decoder.decode(bytes, { stream: !final });
    this.pendingCr = false;
    if (!final && raw.endsWith("\r")) {
      this.pendingCr = true;
      raw = raw.slice(0, -1);
    }
    return normalizeBreaks(raw);
  }
}

/**
 * The byte side of deck editing: read a part of the package as text that
 * re-encodes to exactly the bytes it came from, and hash the package.
 *
 * A part is edited only when `encode(decode(bytes))` gives `bytes` back:
 * UTF-8 with or without its BOM (kept as U+FEFF in the text), never UTF-16
 * or malformed bytes, whose round trip would rewrite the part. A part is
 * inflated under `PART_MAX` whatever its central directory claims — the
 * package is untrusted, the same rule as the notes reader in `pptxDeck.ts`.
 */

import JSZip from "jszip";
import { RECOMMENDED_ZIP_LIMITS } from "@aiden0z/pptx-renderer";
import type { PartReader } from "./pptxEdits";

/** Most bytes one edited part may inflate to (a slide is tens of KB). */
export const PART_MAX = 4 * 1024 * 1024;

export class PartUnreadableError extends Error {}

/** JSZip's streaming reader: public in its source, missing from its types. */
interface ZipStream {
  on(event: "data", fn: (chunk: Uint8Array) => void): ZipStream;
  on(event: "error", fn: (e: Error) => void): ZipStream;
  on(event: "end", fn: () => void): ZipStream;
  pause(): ZipStream;
  resume(): ZipStream;
}

function inflateCapped(file: JSZip.JSZipObject, cap: number): Promise<Uint8Array | null> {
  const stream = (file as unknown as { internalStream(type: "uint8array"): ZipStream }).internalStream("uint8array");
  return new Promise((resolve, reject) => {
    const parts: Uint8Array[] = [];
    let size = 0;
    let settled = false;
    stream
      .on("data", (chunk) => {
        if (settled) return;
        size += chunk.byteLength;
        if (size > cap) {
          settled = true;
          parts.length = 0;
          stream.pause();
          resolve(null);
          return;
        }
        parts.push(chunk);
      })
      .on("error", (e) => {
        if (settled) return;
        settled = true;
        reject(e);
      })
      .on("end", () => {
        if (settled) return;
        settled = true;
        const out = new Uint8Array(size);
        let at = 0;
        for (const p of parts) {
          out.set(p, at);
          at += p.byteLength;
        }
        resolve(out);
      })
      .resume();
  });
}

const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });
const encoder = new TextEncoder();

/** `bytes` as text, or null when the text wouldn't re-encode to them. */
export function faithfulText(bytes: Uint8Array): string | null {
  let text: string;
  try {
    text = decoder.decode(bytes);
  } catch {
    return null;
  }
  const back = encoder.encode(text);
  if (back.length !== bytes.length) return null;
  for (let k = 0; k < back.length; k++) if (back[k] !== bytes[k]) return null;
  return text;
}

export function encodePart(text: string): Uint8Array {
  return encoder.encode(text);
}

/** A reader over one version of the package's bytes. */
export async function packageReader(bytes: ArrayBuffer): Promise<PartReader> {
  const zip = await JSZip.loadAsync(bytes);
  if (Object.keys(zip.files).length > RECOMMENDED_ZIP_LIMITS.maxEntries) {
    throw new PartUnreadableError("this deck has too many parts to edit");
  }
  return async (name) => {
    const file = zip.file(name);
    if (file === null) throw new PartUnreadableError(`${name} is not in this deck`);
    const raw = await inflateCapped(file, PART_MAX);
    if (raw === null) throw new PartUnreadableError(`${name} is too large to edit here`);
    const text = faithfulText(raw);
    if (text === null) throw new PartUnreadableError(`${name} isn't UTF-8 text this editor can keep byte-for-byte`);
    return text;
  };
}

/** SHA-256 of `bytes`, lowercase hex — the daemon's `X-Content-Hash`. Null
 *  where the page has no Web Crypto (not a secure context): no editing then. */
export async function sha256Hex(bytes: ArrayBuffer): Promise<string | null> {
  const subtle = globalThis.crypto?.subtle;
  if (subtle === undefined) return null;
  const digest = new Uint8Array(await subtle.digest("SHA-256", bytes));
  return Array.from(digest, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** Base64 of `bytes`, chunked so a large part never overflows the call stack. */
export function toBase64(bytes: Uint8Array): string {
  let bin = "";
  const CHUNK = 0x8000;
  for (let k = 0; k < bytes.length; k += CHUNK) {
    bin += String.fromCharCode(...bytes.subarray(k, k + CHUNK));
  }
  return btoa(bin);
}

/**
 * Whole-file reads for the viewers that parse a file in the browser (Word,
 * PowerPoint, the diagram boards): one fresh `/raw` ticket, one streamed GET,
 * refused past a per-format cap before the body is read. The daemon streams
 * the bytes and holds none of them.
 */

import { fsRawUrl, humanSize } from "./files";

/** The file is past the viewer's cap; `size` is what the daemon reported. */
export class TooLargeError extends Error {
  readonly size: number;
  readonly cap: number;
  constructor(size: number, cap: number) {
    super(`this file is ${humanSize(size)}, over the ${humanSize(cap)} this viewer opens`);
    this.name = "TooLargeError";
    this.size = size;
    this.cap = cap;
  }
}

/** Read `path` whole, refusing it (without downloading) when over `cap` bytes. */
export async function fetchRawBytes(path: string, cap: number, signal?: AbortSignal): Promise<ArrayBuffer> {
  const url = await fsRawUrl(path);
  const res = await fetch(url, { signal });
  if (!res.ok) {
    await res.body?.cancel().catch(() => {});
    throw new Error(res.status === 404 ? "the file is gone" : `the file could not be read (${res.status})`);
  }
  const declared = Number(res.headers.get("content-length"));
  if (Number.isFinite(declared) && declared > cap) {
    await res.body?.cancel().catch(() => {});
    throw new TooLargeError(declared, cap);
  }
  const buf = await res.arrayBuffer();
  if (buf.byteLength > cap) throw new TooLargeError(buf.byteLength, cap);
  return buf;
}

/** Save `blob` as `name` through a transient link (object URL revoked later). */
export function saveBlob(blob: Blob, name: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.rel = "noopener";
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

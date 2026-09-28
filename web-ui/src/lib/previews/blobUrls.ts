/**
 * The `blob:` URLs a docx-preview document hands out for its images,
 * embedded fonts and numbering bullets. docx-preview mints them through
 * `WordDocument.blobToURL` and never revokes them, so every re-render of a
 * rewritten .docx kept the previous version's images alive for the life of
 * the page. `captureBlobUrls` records each one a document mints, so the view
 * can revoke a render's URLs once that render is replaced or unmounted.
 */

/** The internal method docx-preview mints through (pinned by a test). */
export const DOCX_BLOB_METHOD = "blobToURL";

/** Record every `blob:` URL `doc` mints from now on (its `blobToURL`,
 *  wrapped on the instance). Empty, and nothing wrapped, when the document
 *  has no such method. */
export function captureBlobUrls(doc: unknown): string[] {
  const urls: string[] = [];
  if (doc === null || typeof doc !== "object") return urls;
  const target = doc as Record<string, unknown>;
  const inner = target[DOCX_BLOB_METHOD];
  if (typeof inner !== "function") return urls;
  target[DOCX_BLOB_METHOD] = function (this: unknown, ...args: unknown[]): unknown {
    const out: unknown = inner.apply(this, args);
    if (typeof out === "string" && out.startsWith("blob:")) urls.push(out);
    return out;
  };
  return urls;
}

/** Revoke every URL in `urls` (and forget them). */
export function revokeAll(urls: string[]): void {
  for (const u of urls.splice(0)) URL.revokeObjectURL(u);
}

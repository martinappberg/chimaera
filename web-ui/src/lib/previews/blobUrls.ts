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

/**
 * Every object URL one view owns, revoked together: what it minted itself
 * (`own`) and what a library minted into a cache map it was handed
 * (`cache()`, e.g. pptx-renderer's `mediaUrlCache`). `revokeAll` frees them
 * all; `viewLifecycle.ownedObjectUrls` also runs it when the view goes.
 */
export class OwnedUrls {
  private loose: string[] = [];
  private caches: Map<string, string>[] = [];

  /** Take ownership of `url`; returns it. */
  own(url: string): string {
    this.loose.push(url);
    return url;
  }

  /** A fresh map for a library to mint into; its values are owned here. */
  cache(): Map<string, string> {
    const map = new Map<string, string>();
    this.caches.push(map);
    return map;
  }

  /** Revoke everything owned so far (the cache maps are emptied and dropped). */
  revokeAll(): void {
    revokeAll(this.loose);
    for (const map of this.caches.splice(0)) {
      for (const url of map.values()) URL.revokeObjectURL(url);
      map.clear();
    }
  }
}

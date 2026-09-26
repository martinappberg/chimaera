import { afterEach, describe, expect, it, vi } from "vitest";
// The ESM build the app bundles, as text (its package exports hide the path).
import src from "../../../node_modules/docx-preview/dist/docx-preview.mjs?raw";
import { captureBlobUrls, DOCX_BLOB_METHOD, revokeAll } from "./blobUrls";

/** docx-preview's own shape: loaders mint through `this.blobToURL`. */
class FakeWordDocument {
  blobToURL(blob: Blob): string {
    return URL.createObjectURL(blob);
  }
  async loadDocumentImage(): Promise<string> {
    return this.blobToURL(new Blob(["png"], { type: "image/png" }));
  }
  async loadFont(): Promise<string> {
    return this.blobToURL(new Blob(["font"]));
  }
}

describe("docx blob URLs", () => {
  afterEach(() => vi.restoreAllMocks());

  it("records what a document mints and revokes it all", async () => {
    const doc = new FakeWordDocument();
    const urls = captureBlobUrls(doc);
    const a = await doc.loadDocumentImage();
    const b = await doc.loadFont();
    expect(urls).toEqual([a, b]);
    expect(a.startsWith("blob:")).toBe(true);
    // Another document's URLs are not this one's.
    const other = new FakeWordDocument();
    await other.loadDocumentImage();
    expect(urls).toHaveLength(2);

    const revoke = vi.spyOn(URL, "revokeObjectURL");
    revokeAll(urls);
    expect(revoke.mock.calls.map((c) => c[0])).toEqual([a, b]);
    expect(urls).toEqual([]);
    revokeAll(urls);
    expect(revoke).toHaveBeenCalledTimes(2);
  });

  it("wraps nothing on a document without the method", () => {
    const doc = { other: 1 };
    expect(captureBlobUrls(doc)).toEqual([]);
    expect(doc).toEqual({ other: 1 });
    expect(captureBlobUrls(null)).toEqual([]);
  });

  it("matches the method docx-preview mints through", () => {
    // Pinned: an upgrade that renames it would leak again, silently.
    expect(src).toContain(`${DOCX_BLOB_METHOD}(blob, path) {`);
    for (const loader of ["loadDocumentImage", "loadNumberingImage", "loadFont"]) {
      const body = src.slice(src.indexOf(`async ${loader}(`), src.indexOf(`async ${loader}(`) + 400);
      expect(body).toContain(`this.${DOCX_BLOB_METHOD}(`);
    }
    expect((src.match(/URL\.createObjectURL/g) ?? []).length).toBe(1);
  });
});

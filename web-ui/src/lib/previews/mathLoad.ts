/**
 * On-demand access to the math policy (`shared/math`: KaTeX + DOMPurify) for
 * the markdown previews, so a document with no equation never loads them.
 * `mathNow()` is the module once it has arrived (renders are synchronous from
 * then on); `loadMath()` starts — or joins — the single in-flight load. Chat
 * imports the same module statically, so when a chat surface has already
 * been opened the load resolves from the module cache. A failed load (a
 * tunnel hiccup mid-fetch) is forgotten, so the next equation retries instead
 * of every equation for the rest of the window's life landing on one stale
 * rejection.
 */
type MathModule = typeof import("../shared/math");

let loaded: MathModule | null = null;
let pending: Promise<MathModule> | null = null;

export function mathNow(): MathModule | null {
  return loaded;
}

export function loadMath(): Promise<MathModule> {
  pending ??= import("../shared/math").then(
    (m) => (loaded = m),
    (err: unknown) => {
      pending = null;
      throw err;
    },
  );
  return pending;
}

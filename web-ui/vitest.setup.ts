// The chat store transitively imports `net/api.ts`, which reads a `#token=`
// bootstrap from the browser at module load (location + sessionStorage). Node
// has neither, and we deliberately avoid jsdom (see vitest.config.ts) — so
// provide the minimal globals that pure logic touches. A bare origin with no
// hash and an empty store make the bootstrap a no-op, which is what a test wants.
const g = globalThis as Record<string, unknown>;
if (typeof g.location === "undefined") {
  g.location = new URL("http://localhost/");
}
if (typeof g.sessionStorage === "undefined") {
  const store = new Map<string, string>();
  g.sessionStorage = {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
    clear: () => store.clear(),
  };
}

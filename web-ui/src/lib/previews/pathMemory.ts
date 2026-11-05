/**
 * A small per-file choice remembered in this browser: a least-recently-used
 * list of `[path, value]` pairs (newest last) under one storage key, capped
 * at `cap`. Storage can be missing or throw anywhere (private windows, a
 * full quota, a sandboxed webview): every access is guarded, and a failure
 * only forgets. A convenience, never state another viewer or the daemon
 * needs (the markdown view's mode, the editor's language override).
 */

export interface PathMemory<T> {
  get(path: string): T | null;
  set(path: string, value: T): void;
  forget(path: string): void;
}

export type PathStore = Pick<Storage, "getItem" | "setItem">;

export function createPathMemory<T>(
  storage: () => PathStore | null,
  key: string,
  isValue: (v: unknown) => v is T,
  cap = 300,
): PathMemory<T> {
  function load(): [string, T][] {
    try {
      const raw = storage()?.getItem(key) ?? null;
      if (raw === null) return [];
      const data: unknown = JSON.parse(raw);
      if (!Array.isArray(data)) return [];
      return data.filter(
        (e): e is [string, T] => Array.isArray(e) && e.length === 2 && typeof e[0] === "string" && isValue(e[1]),
      );
    } catch {
      return [];
    }
  }
  function save(list: [string, T][]): void {
    try {
      storage()?.setItem(key, JSON.stringify(list.slice(-cap)));
    } catch {
      // quota / unavailable: the memory is a convenience
    }
  }
  return {
    get(path) {
      const list = load();
      const i = list.findIndex((e) => e[0] === path);
      if (i === -1) return null;
      const entry = list[i];
      if (i !== list.length - 1) {
        list.splice(i, 1);
        list.push(entry);
        save(list);
      }
      return entry[1];
    },
    set(path, value) {
      const list = load().filter((e) => e[0] !== path);
      list.push([path, value]);
      save(list);
    },
    forget(path) {
      const list = load();
      const kept = list.filter((e) => e[0] !== path);
      if (kept.length !== list.length) save(kept);
    },
  };
}

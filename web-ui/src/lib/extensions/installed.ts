import type { ApplicationExtension } from "./application";
export type ApplicationEntryLoader = (() => Promise<{ default: unknown }>) | null;

/** Trusted assembly selects the entry. No module URL or host service selector
 * crosses this facade; its awaits remain inside the session's retained quota. */
export function installedExtension(load: ApplicationEntryLoader): ApplicationExtension | null {
  if (load === null) return null;
  let pending: Promise<ApplicationExtension> | null = null;
  const entry = (): Promise<ApplicationExtension> => {
    if (pending === null) {
      const original = Promise.resolve().then(load).then(({ default: value }) => {
        if (value === null || typeof value !== "object") throw new Error("Optional view unavailable");
        const candidate = value as Partial<ApplicationExtension>;
        if (candidate.version !== 1 || candidate.id !== "chimaera-pro" || typeof candidate.mount !== "function") {
          throw new Error("Optional view unavailable");
        }
        return candidate as ApplicationExtension;
      });
      pending = original;
      void original.catch(() => { if (pending === original) pending = null; });
    }
    return pending;
  };
  return Object.freeze({ version: 1 as const, id: "chimaera-pro" as const,
    async mount(kind, target, scope) {
      if (scope.signal.aborted) throw new Error("Optional view retired");
      const module = await entry();
      if (scope.signal.aborted) throw new Error("Optional view retired");
      return module.mount(kind, target, scope);
    },
  } satisfies ApplicationExtension);
}

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
    async bindAccountPresentation(scope) {
      if (scope.signal.aborted || !scope.current()) throw new Error("Optional view retired");
      const module = await entry();
      if (scope.signal.aborted || !scope.current()) throw new Error("Optional view retired");
      if (typeof module.bindAccountPresentation !== "function") throw new Error("Optional account unavailable");
      const owner = await module.bindAccountPresentation(scope);
      if (scope.signal.aborted || !scope.current()) { owner.dispose(); throw new Error("Optional view retired"); }
      return owner;
    },
    async bindAccountBranding(scope) {
      if (scope.signal.aborted) throw new Error("Optional account subscription ended");
      const original = new AbortController();
      const retire = (): void => original.abort();
      scope.signal.addEventListener("abort", retire, { once: true });
      const startupDeadline = scope.environment.gateway ? scope.startupDeadline ?? performance.now() + 10_000 : undefined;
      const timer = startupDeadline === undefined ? null : setTimeout(retire, Math.max(0, startupDeadline - performance.now()));
      let cancelled!: () => void;
      const ended = new Promise<never>((_, reject) => {
        cancelled = () => reject(new Error("Optional account subscription ended"));
        original.signal.addEventListener("abort", cancelled, { once: true });
      });
      if (scope.signal.aborted || (startupDeadline !== undefined && performance.now() >= startupDeadline)) retire();
      const registration = (async () => {
        if (original.signal.aborted) throw new Error("Optional account subscription ended");
        const module = await entry();
        if (original.signal.aborted || typeof module.bindAccountBranding !== "function") throw new Error("Optional account unavailable");
        const stop = await module.bindAccountBranding({ signal: original.signal, startupDeadline,
          environment: { native: scope.environment.native, gateway: scope.environment.gateway,
            local: scope.environment.local, workbench: scope.environment.workbench },
          publish: (state) => { if (!original.signal.aborted && !scope.signal.aborted) scope.publish({ plan: state.plan, offered: state.offered, signedOut: state.signedOut }); },
        });
        if (original.signal.aborted) { stop(); throw new Error("Optional account subscription ended"); }
        return stop;
      })();
      try {
        const stop = await Promise.race([registration, ended]);
        return () => { retire(); scope.signal.removeEventListener("abort", retire); stop(); };
      } catch (error) {
        retire(); scope.signal.removeEventListener("abort", retire); throw error;
      } finally {
        if (timer !== null) clearTimeout(timer);
        original.signal.removeEventListener("abort", cancelled);
      }
    },
    async mount(kind, target, scope) {
      if (scope.signal.aborted) throw new Error("Optional view retired");
      const module = await entry();
      if (scope.signal.aborted) throw new Error("Optional view retired");
      return module.mount(kind, target, scope);
    },
    async mountPlace(target, scope) {
      if (scope.signal.aborted) throw new Error("Optional view retired");
      const module = await entry();
      if (scope.signal.aborted) throw new Error("Optional view retired");
      if (typeof module.mountPlace !== "function") throw new Error("Optional place unavailable");
      const owner = await module.mountPlace(target, scope);
      if (scope.signal.aborted) { owner.dispose(); throw new Error("Optional view retired"); }
      return owner;
    },
  } satisfies ApplicationExtension);
}

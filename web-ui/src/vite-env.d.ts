/// <reference types="svelte" />
/// <reference types="vite/client" />

/** Build-time flag (vite.config.ts): the tab-switch perf harness is compiled in. */
declare const __CHIMAERA_PERF__: boolean;

/** Trusted build assembly only; free builds export null and require no package. */
declare module "virtual:chimaera-application-entry" {
  export const loadApplicationEntry: import("./lib/extensions/installed").ApplicationEntryLoader;
}

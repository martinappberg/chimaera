import type UpdateToast from "./UpdateToast.svelte";

export type ToastPresentation =
  | { phase: "loading" }
  | { phase: "ready"; component: typeof UpdateToast }
  | { phase: "failed" };

/** Import custody only: notices and update commands remain with their original owner. */
export function updateToastPresentation(
  load: () => Promise<{ default: typeof UpdateToast }>,
  publish: (state: ToastPresentation) => void,
): { retry(): void; dispose(): void } {
  let live = true, pending = false;
  const retry = (): void => {
    if (!live || pending) return;
    pending = true;
    publish({ phase: "loading" });
    void Promise.resolve().then(load).then(
      ({ default: component }) => { if (live) publish({ phase: "ready", component }); },
      () => { if (live) publish({ phase: "failed" }); },
    ).finally(() => { pending = false; });
  };
  retry();
  return { retry, dispose() { live = false; } };
}

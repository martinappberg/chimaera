/**
 * Turn an asynchronously-created teardown into a synchronous one.
 *
 * Tauri's event `listen()` resolves its unlisten callback asynchronously. A
 * Svelte component can unmount before that promise settles (workspace/home
 * switches do this routinely); simply pushing the eventual callback into an
 * array leaks the listener in that race. This wrapper either retains the
 * callback while mounted or runs it immediately after an early teardown.
 */
export function asyncDisposer(pending: Promise<() => void>): () => void {
  let disposed = false;
  let dispose: (() => void) | null = null;

  void pending.then(
    (next) => {
      if (disposed) next();
      else dispose = next;
    },
    () => {
      // A shell listener can fail while the native window is tearing down.
      // There is nothing left to unsubscribe, and teardown must stay quiet.
    },
  );

  return () => {
    if (disposed) return;
    disposed = true;
    dispose?.();
    dispose = null;
  };
}

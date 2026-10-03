import { asyncDisposer } from "../shared/asyncDisposer";
import { onProReturn, proTakeReturn } from "./native";

/** Register before consuming: the native window may be new or still booting. */
export function listenForProReturn(onReturn: () => void): () => void {
  let alive = true;
  let reading = false;
  let queued = false;
  async function receive(): Promise<void> {
    if (!alive) return;
    if (reading) { queued = true; return; }
    reading = true;
    do {
      queued = false;
      try { if (await proTakeReturn() && alive) onReturn(); }
      catch { /* Older shells have no pending-return command. */ }
    } while (alive && queued);
    reading = false;
  }
  const dispose = asyncDisposer(onProReturn(() => { void receive(); }).then((unlisten) => {
    if (alive) void receive();
    return unlisten;
  }));
  return () => { alive = false; dispose(); };
}

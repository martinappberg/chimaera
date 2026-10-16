/**
 * The window's asset navigation gate: the effects that carry out an
 * `assetTransition` (a build change, a moved connection, a failed chunk, a
 * manual reload) by navigating the document — but only when every piece of
 * local state survives the navigation, or the transition is forced.
 *
 * `installAssetGate()` is called once, during App's initialisation, at the
 * place where these effects were declared. Order matters:
 * - The navigation effect must be declared before App's `beforeunload`
 *   guard effect: a forced attempt with dirty files hands straight back to
 *   the gate after that prompt, and the guard re-reads the live dirty set.
 * - The timer-cleanup effect stays separate from the navigation effect: that
 *   effect re-runs on every dirty-state change and would drop the pending
 *   retry with it.
 */
import { fromStore } from "svelte/store";
import { untrack } from "svelte";
import { volatileChatDrafts } from "../chat/drafts";
import { dirtyFiles } from "../shared/editing";
import { pageVisible } from "../shared/visibility";
import {
  assetRearmDelayMs,
  assetTransition,
  noteChunkFailure,
  planAssetNavigation,
  rearmAssetNavigation,
} from "./assetTransition";
import { claimWindowReload } from "./windowReload";

export function installAssetGate(): void {
  const transitionStore = fromStore(assetTransition);
  const dirty = fromStore(dirtyFiles);
  const volatile = fromStore(volatileChatDrafts);
  const visible = fromStore(pageVisible);

  // Vite reports every failed production dynamic import here, including the
  // nested PDF/editor/spreadsheet chunks that never pass through Pane's
  // top-level loader. Keep the rejection flowing to its local error boundary
  // while one shared notice offers build-safe recovery.
  $effect(() => {
    const onPreloadError = (event: VitePreloadErrorEvent) => {
      console.error("interface chunk failed to load", event.payload);
      noteChunkFailure();
    };
    window.addEventListener("vite:preloadError", onPreloadError);
    return () => window.removeEventListener("vite:preloadError", onPreloadError);
  });

  // Build/connection transitions navigate automatically only when all local
  // state survives navigation. A blocked transition stays visible instead of
  // looping beforeunload prompts or silently dropping a memory-only draft.
  let handledAssetRevision = 0;
  let assetRearmTimer: ReturnType<typeof setTimeout> | null = null;
  /** A retry that came due while nobody could see the window; the return to
   *  visible is its catch-up. Plain, not `$state`: only the visibility flip
   *  re-runs the catch-up effect. */
  let deferredAssetRearm: { revision: number; keepForce: boolean } | null = null;
  $effect(() => {
    const transition = transitionStore.current;
    if (transition === null || !transition.requested) return;
    const blocked = dirty.current.size > 0 || volatile.current.size > 0;
    if (blocked && !transition.forced) return;
    if (transition.revision === handledAssetRevision) return;
    handledAssetRevision = transition.revision;
    // Only a forced attempt with dirty files meets the beforeunload prompt
    // (App's guard exists only then); it hands straight back to the gate.
    const prompted = transition.forced && dirty.current.size > 0;
    untrack(() => {
      const navigation = planAssetNavigation(location.href, transition.target);
      if (navigation.kind === "reload") {
        location.reload();
      } else if (navigation.kind === "replace") {
        location.replace(navigation.target);
      } else {
        history.replaceState(history.state, "", navigation.target);
        location.reload();
      }
      // Issue once, then wait: the document outliving the call proves nothing
      // (navigation is asynchronous) and a re-issue cancels the load in flight
      // (see assetRearmDelayMs). A retry that comes due while hidden holds
      // until the window is looked at — a window nobody can see must not push
      // a document load through the tunnel every half minute.
      if (assetRearmTimer !== null) clearTimeout(assetRearmTimer);
      assetRearmTimer = setTimeout(() => {
        assetRearmTimer = null;
        // Unprompted: no beforeunload could have consumed the force, so it
        // carries into the retry; a prompted hand-back never waits on visibility.
        const unprompted = !prompted;
        if (unprompted && document.visibilityState === "hidden") {
          deferredAssetRearm = { revision: transition.revision, keepForce: true };
          return;
        }
        rearmAssetNavigation(transition.revision, unprompted);
      }, assetRearmDelayMs(prompted, transition.attempts));
    });
  });
  $effect(() => {
    if (!visible.current || deferredAssetRearm === null) return;
    const { revision, keepForce } = deferredAssetRearm;
    deferredAssetRearm = null;
    rearmAssetNavigation(revision, keepForce);
  });
  // Deliberately not the navigation effect's own cleanup: that effect re-runs
  // on every dirty-state change and would drop the pending retry with it.
  $effect(() => () => {
    if (assetRearmTimer !== null) clearTimeout(assetRearmTimer);
  });
  // The native Reload Window (layout/windowReload.ts) reloads through the
  // gate above while it is mounted, not plainly.
  $effect(() => claimWindowReload());
}

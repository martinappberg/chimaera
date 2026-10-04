<script module lang="ts">
  import { selectedApplication as selected } from "./selected";
  let sequence = 0;
</script>
<script lang="ts">
  import { untrack } from "svelte";
  import ApplicationSurface from "./ApplicationSurface.svelte";
  import { keptApplicationRuntime, captureKeptHost, type KeptHostSnapshot, type KeptHostCallbacks } from "./hostRuntime.svelte";
  import { bindExistingKeptReview } from "./keptReview";
  import { apiOwner, captureApiGuard, notifyUnauthorized, type ApiGuard } from "../net/api";
  import { gatewayWorkspace } from "../net/base";
  import { placementOwner, readPlacement, PlacementError } from "../net/placement";
  import { getSetting } from "../settings/store.svelte";
  import { hereName } from "../pro/kept";
  import type { HostSurfaceActions, SurfaceIdentity } from "./application";

  let { wsId, wsRoot, visible, tab, paneId, currentTab, callbacks }: {
    wsId: string | null; wsRoot: string | null; visible: boolean; tab: object; paneId: string;
    currentTab(original: object, pane: string): boolean; callbacks: KeptHostCallbacks;
  } = $props();
  let owner = $state.raw<ReturnType<typeof prepare> | null>(null);
  let preparing = $state(false), failed = $state(false), retry = $state(0);
  let recovery = $state.raw<{ close(): void; openFolder(): void } | null>(null);
  const fontSize = $derived(getSetting("editor.fontSize"));
  const lineHeight = $derived(getSetting("editor.lineHeight"));
  const tabSize = $derived(getSetting("editor.tabSize"));
  const lineNumbers = $derived(getSetting("editor.lineNumbers"));

  function prepare(original: KeptHostSnapshot, guard: ApiGuard, isCurrent: () => boolean,
    editor: { fontSize: number; lineHeight: number; tabSize: number; lineNumbers: boolean }, capturedCallbacks: KeptHostCallbacks) {
    const host = captureKeptHost(original, () => isCurrent() ? original : null, capturedCallbacks);
    const scoped: ApiGuard = Object.freeze({ owner: guard.owner, current: () => guard.current() && host.actions.current(),
      ...(guard.placement === undefined ? {} : { placement: guard.placement }) });
    const actions: HostSurfaceActions = { ...host.actions, keptReview: () => bindExistingKeptReview({
      workspaceId: original.workspaceId, current: scoped.current, requireFresh: true,
      hereName: hereName() as "this Mac" | "this computer" | "your computer", editor,
    }, scoped) };
    if (sequence === Number.MAX_SAFE_INTEGER) throw new Error("Kept view unavailable");
    sequence += 1;
    const identity: SurfaceIdentity = Object.freeze({ version: 1, viewId: `kept_${sequence}`,
      attemptId: sequence, workspaceId: original.workspaceId });
    return { identity, actions, host };
  }
  $effect(() => {
    const workspaceId = wsId, root = wsRoot, originalTab = tab, originalPane = paneId;
    const auth = $apiOwner; const route = $placementOwner;
    const attempt = retry;
    const editor = { fontSize, lineHeight, tabSize, lineNumbers };
    let active = true;
    const capturedCallbacks = untrack(() => callbacks);
    const originalExists = () => active && wsId === workspaceId && wsRoot === root && tab === originalTab &&
      paneId === originalPane && currentTab(originalTab, originalPane);
    untrack(() => { recovery = {
      close: () => { if (originalExists()) capturedCallbacks.close(originalTab, originalPane); },
      openFolder: () => { if (originalExists() && root !== null && root.startsWith("/") && !root.includes("\0")) capturedCallbacks.openFolder(originalPane, root); },
    }; });
    untrack(() => { owner = null; preparing = false; failed = false; });
    if (selected === null || workspaceId === null || root === null || auth === null) return () => { active = false; };
    const isCurrent = () => active && wsId === workspaceId && wsRoot === root && tab === originalTab &&
      paneId === originalPane && currentTab(originalTab, originalPane) && auth.current();
    untrack(() => { preparing = true; });
    void (async () => {
      try {
        if (gatewayWorkspace() !== null && route === null) { await readPlacement(); return; }
        if (!isCurrent()) return;
        const guard = captureApiGuard(gatewayWorkspace() === null ? undefined : route ?? undefined);
        if (!guard.current()) return;
        const original = { paneId: originalPane, workspaceId, root, tab: originalTab, route: guard };
        const next = prepare(original, guard, isCurrent, editor, capturedCallbacks);
        if (!isCurrent()) return;
        owner = next;
      } catch (error) {
        if (isCurrent()) {
          if (error instanceof PlacementError && error.status === 401) notifyUnauthorized();
          else failed = true;
        }
      }
      finally { if (active) preparing = false; }
    })();
    void attempt;
    return () => { active = false; };
  });
  const presentation = $derived({ revision: 1, visible, projectLabel: null, onboarding: null });

</script>

{#if owner !== null}
  <ApplicationSurface kind="kept-review" identity={owner.identity} {presentation}
    runtime={keptApplicationRuntime} extension={selected} actions={owner.actions} />
{:else}
  <div class="kept-unavailable">
    <p role="status">{preparing ? "Opening the review…" : failed ? "The review couldn’t be opened." : "This review isn’t available here."}</p>
    <p>You can open the project folder to review the versions with your usual file and Git tools.</p>
    {#if failed}<button onclick={() => { retry += 1; }}>Retry</button>{/if}
    {#if wsRoot !== null}<button onclick={() => recovery?.openFolder()}>Open project folder</button>{/if}
    <button onclick={() => recovery?.close()}>Close</button>
  </div>
{/if}
<style>
  .kept-unavailable { padding: 24px; color: var(--fg); }
  p { color: var(--muted); max-width: 50em; }
  button { color: var(--fg); background: var(--bg); border: 1px solid var(--edge); border-radius: 6px; padding: 6px 10px; margin-right: 8px; }
  button:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; }
</style>

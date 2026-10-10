<script module lang="ts">
  import { selectedApplication as selected } from "./selected";
  export const accountExtensionSelected = selected !== null;
  let sequence = 0;
</script>
<script lang="ts">
  import { untrack } from "svelte";
  import ApplicationSurface from "./ApplicationSurface.svelte";
  import { keptApplicationRuntime, originalOnboardingIntent } from "./hostRuntime.svelte";
  import { cloudOnboarding } from "../pro/onboarding.svelte";
  import { accountSurfaceCurrent } from "./accountOwner";
  import { modalFocus } from "../shared/modalFocus";
  import type { Workspace } from "../workspace/sessions";
  import type { ApplicationRuntime, HostSurfaceActions, SurfaceKind, SurfaceIdentity, SurfacePresentation } from "./application";
  let { kind = "account", visible = true, workspaceId = null, projectLabel = null, knownIds = [],
    incarnation, current = () => true, onOpen = () => {}, onClose = () => {} }: {
    kind?: Exclude<SurfaceKind, "kept-review">; visible?: boolean; workspaceId?: string | null;
    projectLabel?: string | null; knownIds?: string[]; incarnation?: object; current?: () => boolean;
    onOpen?: (workspace: Workspace) => void; onClose?: () => void;
  } = $props();
  const originalComponent = {};
  let prepared = $state.raw<{ identity: SurfaceIdentity; actions: HostSurfaceActions; runtime: ApplicationRuntime; presentation: SurfacePresentation } | null>(null);
  let revision = 0;
  let accountRevision = $state(0);
  $effect(() => {
    const originalAccountRevision = accountRevision;
    const original = incarnation ?? originalComponent;
    const originalKind = kind;
    const originalWorkspace = workspaceId;
    const intent = originalKind === "account" || originalKind === "cloud-setup" ? cloudOnboarding.context : null;
    const callbacks = untrack(() => ({ current, onOpen, onClose }));
    let live = true;
    const check = accountSurfaceCurrent({ incarnation: original, workspaceId: originalWorkspace, intent },
      () => live && accountRevision === originalAccountRevision && kind === originalKind ? { incarnation: incarnation ?? originalComponent, workspaceId,
        intent: (originalKind === "account" || originalKind === "cloud-setup") ? cloudOnboarding.context : null } : null, callbacks.current);
    const id = ++sequence;
    const onboarding = originalOnboardingIntent(intent);
    const runtime = keptApplicationRuntime;
    const actions: HostSurfaceActions = {
      current: check, openFile: async () => { throw new Error("No file authority in account view"); },
      completeOnboarding: () => { throw new Error("Use original account completion"); },
      close: () => { if (check()) callbacks.onClose(); },
      modal: (node) => { const handle = modalFocus(node); return { destroy: () => handle?.destroy?.() }; },
      accountPresentation: async (guarded, signal) => {
        if (!check() || signal.aborted) throw new Error("Account view retired");
        const { accountHostScope } = await import("./accountHost");
        if (!check() || signal.aborted) throw new Error("Account view retired");
        if (selected?.bindAccountPresentation === undefined) throw new Error("Optional account unavailable");
        return selected.bindAccountPresentation(accountHostScope({ current: () => !signal.aborted && check(), signal, originalIntent: intent, modal: guarded.modal,
          retire: (preserveIntent) => {
            if (!check()) return;
            // A replacement account never inherits an original project handoff.
            if (!preserveIntent && intent !== null && cloudOnboarding.context === intent) cloudOnboarding.clear();
            accountRevision += 1;
          },
          openWorkspace: (workspace) => { if (!check()) throw new Error("Account view retired"); callbacks.onOpen(workspace); } }));
      },
    };
    const value = { identity: { version: 1 as const, viewId: `account-${id}`, attemptId: id, workspaceId: originalWorkspace }, actions, runtime,
      presentation: { revision: ++revision, visible: untrack(() => visible), projectLabel: untrack(() => projectLabel), onboarding, knownWorkspaceIds: untrack(() => knownIds) } };
    prepared = value;
    return () => { live = false; if (prepared === value) prepared = null; };
  });
  const presentation = $derived(prepared === null ? null : {
    ...prepared.presentation, visible, projectLabel, knownWorkspaceIds: knownIds,
  });
</script>
{#if selected !== null && prepared !== null && presentation !== null}
  <ApplicationSurface {kind} identity={prepared.identity} {presentation} runtime={prepared.runtime} extension={selected} actions={prepared.actions} />
{:else}
  <p class="unavailable" role="status">This optional account extension is not installed. Your local work and settings remain available.</p>
{/if}
<style>
  .unavailable { margin: 16px; color: var(--muted); font-size: var(--text-sm); line-height: 1.6; }
</style>

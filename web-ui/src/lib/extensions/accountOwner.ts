import type { CloudOnboardingContext } from "../pro/onboarding.svelte";
/** UI incarnation only. Account and operation authority stays in named owners. */
export function accountSurfaceCurrent(original: { incarnation: object; workspaceId: string | null; intent: CloudOnboardingContext | null },
  read: () => { incarnation: object; workspaceId: string | null; intent: CloudOnboardingContext | null } | null,
  hostCurrent: () => boolean): () => boolean {
  return () => {
    const value = read();
    return value !== null && value.incarnation === original.incarnation && value.workspaceId === original.workspaceId &&
      value.intent === original.intent && hostCurrent();
  };
}

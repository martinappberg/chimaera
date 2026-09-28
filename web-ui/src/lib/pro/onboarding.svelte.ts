/** Shared intent lets a handoff open the same connection flow as first setup. */
export interface CloudOnboardingContext {
  providerIds: string[];
  workspaceId?: string;
  workspaceName?: string;
}

function validId(value: unknown): value is string {
  return typeof value === "string" && /^[a-zA-Z0-9_-]{1,128}$/.test(value);
}

class CloudOnboarding {
  context = $state<CloudOnboardingContext | null>(null);

  set(value: unknown): boolean {
    if (!value || typeof value !== "object") return false;
    const input = value as Partial<CloudOnboardingContext>;
    if (!Array.isArray(input.providerIds) || input.providerIds.length > 16 || !input.providerIds.every(validId)) return false;
    if (input.workspaceId !== undefined && !validId(input.workspaceId)) return false;
    this.context = {
      providerIds: [...new Set(input.providerIds)],
      ...(input.workspaceId ? { workspaceId: input.workspaceId } : {}),
      ...(typeof input.workspaceName === "string" ? { workspaceName: input.workspaceName.slice(0,160) } : {}),
    };
    return true;
  }

  clear(): void { this.context = null; }

  request(context: CloudOnboardingContext): void {
    if (this.set(context)) {
      window.dispatchEvent(new CustomEvent("chimaera:connect-providers", { detail: this.context }));
    }
  }
}

export const cloudOnboarding = new CloudOnboarding();

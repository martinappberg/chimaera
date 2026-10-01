import { versionNumber, type AgentInfo } from "../workspace/launcher";

/** Unknown versions and failed checks must never become an up-to-date claim. */
export function agentUpdateStatus(a: AgentInfo): { text: string | null; current: boolean } {
  if (a.latestError) return { text: "Couldn't check for updates", current: false };
  if (a.updateAvailable && a.latestVersion) return { text: `${a.latestVersion} available`, current: false };
  if (!a.version || !/^\d+\.\d+\.\d+$/.test(versionNumber(a.version))) {
    return { text: "Version unknown", current: false };
  }
  if (!a.latestVersion || !/^\d+\.\d+\.\d+$/.test(a.latestVersion)) {
    return { text: null, current: false };
  }
  return { text: "Up to date", current: true };
}

export function installationResult(session: { running: boolean; exitStatus: number | null } | undefined): "running" | "done" | "failed" | "unknown" {
  if (!session) return "unknown";
  if (session.running) return "running";
  if (session.exitStatus === null) return "unknown";
  return session.exitStatus === 0 ? "done" : "failed";
}

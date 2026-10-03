import type { ProDevice } from "../net/native";

export interface DeviceGroup { id: string; name: string; current: boolean; lastSeen: string; signIns: ProDevice[] }
export interface DeviceGroups { devices: DeviceGroup[]; currentSignIn: ProDevice | null; otherSignIns: ProDevice[] }

/** Only the account's proof-backed installation binding identifies a computer. */
export function groupDevices(signIns: readonly ProDevice[]): DeviceGroups {
  const bound = new Map<string, ProDevice[]>();
  const otherSignIns: ProDevice[] = [];
  let currentSignIn: ProDevice | null = null;
  const seen = new Set<string>();
  for (const signIn of signIns) {
    if (seen.has(signIn.id)) continue;
    seen.add(signIn.id);
    const installation = signIn.installation_id;
    if (installation && /^i-[A-Za-z0-9_-]{1,125}$/.test(installation)) {
      bound.set(installation, [...(bound.get(installation) ?? []), signIn]);
    } else if (signIn.this) currentSignIn = signIn;
    else otherSignIns.push(signIn);
  }
  const time = (value: ProDevice) => Date.parse(value.last_seen) || 0;
  const devices = [...bound].map(([id, members]) => {
    const ordered = [...members].sort((a, b) => Number(b.this) - Number(a.this) || time(b) - time(a));
    return { id, name: ordered[0].name, current: ordered.some(value => value.this), lastSeen: [...members].sort((a, b) => time(b) - time(a))[0].last_seen, signIns: ordered };
  }).sort((a, b) => Number(b.current) - Number(a.current) || a.name.localeCompare(b.name));
  return { devices, currentSignIn, otherSignIns: otherSignIns.sort((a, b) => time(b) - time(a)) };
}

export function lastSeen(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? "Last used time unavailable" : `Last used ${date.toLocaleString()}`;
}

import { describe, expect, it } from "vitest";
import { groupDevices } from "./devices";
import type { ProDevice } from "../net/native";
const row = (id: string, name: string, installation_id?: string, current = false): ProDevice => ({ id, name, installation_id, this: current, last_seen: "2026-09-28T19:00:00Z" });
describe("account device identity", () => {
  it("never merges matching hostnames or treats old names as separate verified computers", () => {
    const result = groupDevices([row("current", "My Mac", "i-one", true), row("old", "My Mac"), row("network", "DNa1cd81c.SUNet"), row("other", "My Mac", "i-two")]);
    expect(result.devices.map(value => value.id)).toEqual(["i-one", "i-two"]);
    expect(result.otherSignIns.map(value => value.id)).toEqual(["old", "network"]);
  });
  it("groups only authenticated installation bindings while preserving every revocable sign-in", () => {
    const result = groupDevices([row("older", "Old name", "i-one"), row("current", "New name", "i-one", true)]);
    expect(result.devices).toHaveLength(1);
    expect(result.devices[0]).toMatchObject({ name: "New name", current: true });
    expect(result.devices[0].signIns.map(value => value.id)).toEqual(["current", "older"]);
  });
  it("keeps legacy current sign-in visible without inventing an installation", () => {
    const current = row("current", "Network hostname", undefined, true);
    const result = groupDevices([current, current, row("old", "Mac", "not-a-binding")]);
    expect(result.devices).toEqual([]);
    expect(result.currentSignIn?.id).toBe("current");
    expect(result.otherSignIns).toHaveLength(1);
  });
});

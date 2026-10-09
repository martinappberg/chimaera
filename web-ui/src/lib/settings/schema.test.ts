import { describe, expect, it } from "vitest";
import { SETTINGS, listed } from "./schema";

describe("settings listed", () => {
  it("lists Developer Tools only in a development build", () => {
    const dev = SETTINGS.find((d) => d.id === "developer.tools");
    expect(dev?.devOnly).toBe(true);
    expect(listed(dev!, false)).toBe(false);
    expect(listed(dev!, true)).toBe(true);
    for (const def of SETTINGS.filter((d) => !d.devOnly)) expect(listed(def, false)).toBe(true);
  });
});

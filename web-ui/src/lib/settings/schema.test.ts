import { describe, expect, it } from "vitest";
import { SETTINGS, completableSettings, listed } from "./schema";

describe("settings listed", () => {
  it("lists Developer Tools only in a development build", () => {
    const dev = SETTINGS.find((d) => d.id === "developer.tools");
    expect(dev?.devOnly).toBe(true);
    expect(listed(dev!, false)).toBe(false);
    expect(listed(dev!, true)).toBe(true);
    for (const def of SETTINGS.filter((d) => !d.devOnly)) expect(listed(def, false)).toBe(true);
  });

  it("never completes Developer Tools in the JSON editor of a release build", () => {
    const ids = (devBuild: boolean) => completableSettings(devBuild).map((d) => d.id);
    expect(ids(false)).not.toContain("developer.tools");
    expect(ids(true)).toContain("developer.tools");
    expect(ids(false)).toHaveLength(SETTINGS.filter((d) => !d.devOnly).length);
  });
});

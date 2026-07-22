import { describe, expect, it } from "vitest";
import { askpassBelongsToHost } from "./askpassScope";

describe("SSH authentication prompt scope", () => {
  const sherlock = { id: 1, alias: "Sherlock", prompt: "Passcode:" };
  const remote2 = { id: 2, alias: "remote-2", prompt: "Password:" };

  it("shows a remote window only its own host's prompt", () => {
    expect(askpassBelongsToHost(sherlock, "Sherlock")).toBe(true);
    expect(askpassBelongsToHost(remote2, "Sherlock")).toBe(false);
  });

  it("keeps the home and rolling-upgrade fallbacks answerable", () => {
    expect(askpassBelongsToHost(remote2, null)).toBe(true);
    const legacy = { id: 3, prompt: "Password:" };
    expect(askpassBelongsToHost(legacy, null)).toBe(true);
    expect(askpassBelongsToHost(legacy, "Sherlock")).toBe(false);
  });
});

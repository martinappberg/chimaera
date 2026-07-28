import { beforeEach, describe, expect, it } from "vitest";

import { isHomeHub, leaveHomeHub, reclaimHomeHub } from "./api";

describe("native Home hub identity", () => {
  beforeEach(() => sessionStorage.clear());

  it("can be reclaimed after a workspace promotion clears it", () => {
    reclaimHomeHub();
    expect(isHomeHub()).toBe(true);

    leaveHomeHub();
    expect(isHomeHub()).toBe(false);

    reclaimHomeHub();
    expect(isHomeHub()).toBe(true);
  });
});

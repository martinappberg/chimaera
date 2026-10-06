import { describe, expect, it } from "vitest";
import { nextInGroup } from "./sessionCycle";

describe("nextInGroup", () => {
  const ids = ["a", "b", "c"];

  it("steps to the following row in the rail's order", () => {
    expect(nextInGroup(ids, "a")).toBe("b");
    expect(nextInGroup(ids, "b")).toBe("c");
  });

  it("wraps from the last row to the first", () => {
    expect(nextInGroup(ids, "c")).toBe("a");
  });

  it("enters at the first row from outside the group", () => {
    expect(nextInGroup(ids, null)).toBe("a");
    expect(nextInGroup(ids, "someone-else")).toBe("a");
  });

  it("stays put in a group of one and finds nothing in an empty one", () => {
    expect(nextInGroup(["only"], "only")).toBe("only");
    expect(nextInGroup([], "a")).toBeNull();
    expect(nextInGroup([], null)).toBeNull();
  });
});

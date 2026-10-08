import { afterEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { MOVING_MAX_MS, movingSentence, nextMoving, restoreMoving, type ProjectMoving } from "./projectMoving";

const computer = { availability: "owned", route_host_id: "device-d-home" };
const cloud = { availability: "owned", route_host_id: "worker-wk" };
const asleep = { availability: "suspended", route_host_id: "worker-wk" };
const nobody = { availability: "unowned", route_host_id: null };

/** Successive placement reads, as `placement.ts` feeds them. */
function run(reads: { availability: string; route_host_id: string | null }[], start: "cloud" | "computer" | null = null): (ProjectMoving | null)[] {
  let now: ProjectMoving | null = null;
  let before = start;
  return reads.map((row, at) => {
    now = nextMoving(now, before, row, at);
    if (row.route_host_id?.startsWith("worker-")) before = "cloud";
    else if (row.route_host_id?.startsWith("device-")) before = "computer";
    return now;
  });
}
const words = (states: (ProjectMoving | null)[]) => states.map(state => state === null ? null : movingSentence(state));

describe("a project view while its project moves", () => {
  it("says each step of a move to the cloud in the user's words", () => {
    expect(words(run([computer, nobody, nobody, cloud, cloud]))).toEqual([
      null, "Moving to your cloud…", "Moving to your cloud…", "Starting in your cloud…", "Starting in your cloud…",
    ]);
  });
  it("says a return to the computer, from the release to the computer holding it", () => {
    expect(words(run([cloud, nobody, computer]))).toEqual([null, "Coming back to your computer…", "Coming back to your computer…"]);
    expect(words(run([asleep, nobody]))).toEqual([null, "Coming back to your computer…"]);
  });
  it("tells a change of machine with no release seen in between as arriving there", () => {
    expect(words(run([computer, cloud]))).toEqual([null, "Starting in your cloud…"]);
    expect(words(run([cloud, computer]))).toEqual([null, "Coming back to your computer…"]);
  });
  it("says nothing at rest, asleep, or for a move that did not happen", () => {
    expect(run([computer, computer])).toEqual([null, null]);
    expect(run([cloud, asleep, cloud])).toEqual([null, null, null]);
    // Released, then the computer took it again: the move did not happen.
    expect(run([computer, nobody, computer])[2]).toBeNull();
    // A view that never saw the project held cannot tell a direction.
    expect(run([nobody, nobody])).toEqual([null, null]);
    for (const availability of ["expired", "privacy_disabled"]) expect(run([computer, { availability, route_host_id: null }])).toEqual([null, null]);
  });
  it("keeps the start of the move while it goes on", () => {
    const states = run([computer, nobody, nobody, cloud]);
    expect(states.slice(1).map(state => state?.since)).toEqual([1, 1, 1]);
  });
  it("carries a young move across the one reload, for this project only", () => {
    const raw = JSON.stringify({ workspace: "w-one", to: "cloud", arriving: true, since: 1000 });
    expect(restoreMoving(raw, "w-one", 2000)).toEqual({ to: "cloud", arriving: true, since: 1000 });
    expect(restoreMoving(raw, "w-two", 2000)).toBeNull();
    expect(restoreMoving(raw, "w-one", 1000 + MOVING_MAX_MS)).toBeNull();
    expect(restoreMoving(raw, null, 2000)).toBeNull();
    for (const bad of ["{", "null", JSON.stringify({ workspace: "w-one", to: "mars", since: 1 }), JSON.stringify({ workspace: "w-one", to: "cloud", since: 9999 })])
      expect(restoreMoving(bad, "w-one", 2000)).toBeNull();
  });
});

describe("the view's state", () => {
  afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); vi.resetModules(); });
  it("is never set outside a project view", async () => {
    vi.stubGlobal("location", new URL("https://fixture.invalid/app/device-d/"));
    const moving = await import("./projectMoving");
    moving.noteMovingRead("computer", nobody);
    expect(get(moving.projectMoving)).toBeNull();
  });
  it("shows from a release, clears once the project is served, and never outlives the cap", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("location", new URL("https://fixture.invalid/workspace/w-one/"));
    const moving = await import("./projectMoving");
    moving.noteMovingRead("computer", nobody);
    expect(movingSentence(get(moving.projectMoving)!)).toBe("Moving to your cloud…");
    moving.noteMovingRead("computer", cloud);
    expect(movingSentence(get(moving.projectMoving)!)).toBe("Starting in your cloud…");
    moving.noteServed();
    expect(get(moving.projectMoving)).toBeNull();
    moving.noteMovingRead("cloud", nobody);
    expect(get(moving.projectMoving)).not.toBeNull();
    vi.advanceTimersByTime(MOVING_MAX_MS);
    expect(get(moving.projectMoving)).toBeNull();
  });
});

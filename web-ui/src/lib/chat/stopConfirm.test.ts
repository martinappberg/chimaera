import { describe, expect, it } from "vitest";
import {
  actionReach,
  actionVerb,
  armedKey,
  clickVia,
  confirmHint,
  pressStep,
  stopReach,
  stopReachText,
  type ActionContext,
  type StopLane,
} from "./stopConfirm";

function lane(taskType: string, description: string, over: Partial<StopLane> = {}): StopLane {
  return { taskType, description, ambient: false, ...over };
}
const text = (lanes: StopLane[]) => stopReachText(stopReach(lanes));

describe("stopReach", () => {
  it("stops at once when only the turn would end", () => {
    expect(text([])).toBeNull();
  });

  it("never names the step the turn is on", () => {
    // A foreground subagent the turn waits on is no lane until it is moved
    // to the background (Pass 44): stopping it is what Stop is for.
    expect(text([lane("local_bash", "npm test")])).toBeNull();
  });

  it("names background agents and workflows, which a claude Stop kills", () => {
    expect(text([lane("local_agent", "bg probe"), lane("local_agent", "second"), lane("local_workflow", "probe")])).toBe(
      "Also stops 2 subagents and 1 workflow",
    );
    expect(text([lane("local_workflow", "a"), lane("local_workflow", "b")])).toBe("Also stops 2 workflows");
    // A foreground agent moved to the background is a background agent now.
    expect(text([lane("local_agent", "audit")])).toBe("Also stops 1 subagent");
  });

  it("never names work a Stop leaves running", () => {
    // claude: run_in_background commands and Monitor watches ride local_bash
    // and survive the interrupt; remote agents are unprobed.
    expect(text([lane("local_bash", "npm test"), lane("local_bash", "watch log"), lane("remote_agent", "r")])).toBeNull();
  });

  it("skips housekeeping lanes", () => {
    expect(text([lane("local_agent", "tidy", { ambient: true })])).toBeNull();
  });
});

describe("armedKey", () => {
  const key = (k: string, defaultPrevented = false) => ({ key: k, defaultPrevented });

  it("confirms on a second Esc in this chat", () => {
    expect(armedKey(key("Escape"), true)).toBe("confirm");
  });

  it("leaves an Esc another surface handled alone", () => {
    // The composer's own Esc (it confirms itself), a card's, a popover's.
    expect(armedKey(key("Escape", true), true)).toBe("ignore");
  });

  it("lets the stop go on any other key or an Esc in another pane", () => {
    expect(armedKey(key("a"), true)).toBe("dismiss");
    expect(armedKey(key("Enter"), true)).toBe("dismiss");
    expect(armedKey(key("Escape"), false)).toBe("dismiss");
    // Esc confirms only a Stop; an armed Clear goal or Send now is cancelled.
    expect(armedKey(key("Escape"), true, false, false)).toBe("dismiss");
    expect(armedKey(key("Escape"), true, true, false)).toBe("dismiss");
  });

  it("leaves Enter or Space on a focused Stop to that Stop's own click", () => {
    expect(armedKey(key("Enter"), true, true)).toBe("ignore");
    expect(armedKey(key(" "), true, true)).toBe("ignore");
    expect(armedKey(key("a"), true, true)).toBe("dismiss");
  });

  it("swallows a held key's repeats, so one long press never answers itself", () => {
    const held = (k: string) => ({ key: k, defaultPrevented: false, repeat: true });
    expect(armedKey(held("Enter"), true, true)).toBe("hold");
    expect(armedKey(held(" "), true, true)).toBe("hold");
    expect(armedKey(held("Escape"), true)).toBe("hold");
    // Another pane's held Esc is that pane's, never swallowed here.
    expect(armedKey(held("Escape"), false)).toBe("dismiss");
    // The composer's own held Esc stays the composer's.
    expect(armedKey({ key: "Escape", defaultPrevented: true, repeat: true }, true)).toBe("ignore");
  });

  it("ignores a bare modifier", () => {
    for (const k of ["Shift", "Control", "Alt", "Meta"]) expect(armedKey(key(k), true)).toBe("ignore");
  });
});

describe("actionReach", () => {
  const ctx = (over: Partial<ActionContext> = {}): ActionContext => ({
    lanes: [],
    busy: true,
    running: true,
    goalClearStops: true,
    ...over,
  });
  const bg = [lane("local_agent", "bg probe")];

  it("asks for a Stop only while something runs", () => {
    expect(actionReach("stop", ctx({ lanes: bg }))).toBe("Also stops 1 subagent");
    expect(actionReach("stop", ctx({ lanes: bg, busy: false, running: false }))).toBeNull();
    expect(actionReach("stop", ctx())).toBeNull();
  });

  it("asks for claude's Clear goal as for a Stop, since it stops the turn first", () => {
    expect(actionReach("goal", ctx({ lanes: [lane("local_workflow", "wf")] }))).toBe("Also stops 1 workflow");
    expect(actionReach("goal", ctx({ lanes: [...bg, lane("local_agent", "two")] }))).toBe("Also stops 2 subagents");
    // Idle, it is only the /goal clear message.
    expect(actionReach("goal", ctx({ lanes: bg, running: false }))).toBeNull();
    // codex's clear_goal lets the turn finish.
    expect(actionReach("goal", ctx({ lanes: bg, goalClearStops: false }))).toBeNull();
  });
});

describe("pressStep", () => {
  it("asks on the first press and acts on the second press of the same control", () => {
    expect(pressStep(null, "stop", "Also stops 1 subagent")).toBe("arm");
    expect(pressStep("stop", "stop", "Also stops 1 subagent")).toBe("act");
    expect(pressStep("goal", "goal", "Also stops 1 subagent")).toBe("act");
  });

  it("acts at once when nothing else would end", () => {
    expect(pressStep(null, "goal", null)).toBe("act");
  });

  it("asks afresh when another control was waiting", () => {
    expect(pressStep("stop", "goal", "Also stops 1 subagent")).toBe("arm");
    expect(pressStep("goal", "stop", "Also stops 1 subagent")).toBe("arm");
  });
});

describe("the hint names the real gesture", () => {
  it("reads a keyboard-activated click as a key press", () => {
    expect(clickVia({ detail: 0 })).toBe("key");
    expect(clickVia({ detail: 1 })).toBe("pointer");
  });

  it("words Esc, a keyboard press and a click differently, and says the same aloud", () => {
    expect(confirmHint("stop", "esc")).toEqual({ how: "Esc again to stop", spoken: "Press Escape again to stop." });
    expect(confirmHint("stop", "key")).toEqual({ how: "Press again to stop", spoken: "Press again to stop." });
    expect(confirmHint("stop", "pointer")).toEqual({ how: "Click again to stop", spoken: "Click again to stop." });
  });

  it("ends with the action's own verb", () => {
    expect(actionVerb("stop")).toBe("stop");
    expect(actionVerb("goal")).toBe("clear");
    expect(actionVerb("stop")).toBe("stop");
  });
});

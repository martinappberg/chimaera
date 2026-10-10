import { describe, expect, it } from "vitest";

import { titleHost, windowTitle } from "./windowTitle";

describe("windowTitle", () => {
  it("leads with the workspace and wears a remote host", () => {
    expect(windowTitle({ workspace: "my-analysis", host: "hpc", needsYou: 0 })).toBe("my-analysis •hpc | chimaera");
    expect(windowTitle({ workspace: "my-analysis", host: null, needsYou: 0 })).toBe("my-analysis | chimaera");
    expect(windowTitle({ workspace: null, host: "hpc", needsYou: 0 })).toBe("hpc | chimaera");
    expect(windowTitle({ workspace: null, host: null, needsYou: 0 })).toBe("chimaera");
  });

  it("appends a compute node, counts what needs you, and lets a detached tab lead", () => {
    expect(windowTitle({ workspace: "my-analysis", host: "hpc", node: "node-044", needsYou: 0 })).toBe("my-analysis •hpc › node-044 | chimaera");
    expect(windowTitle({ workspace: "my-analysis", host: null, needsYou: 2 })).toBe("(2) my-analysis | chimaera");
    expect(windowTitle({ workspace: "my-analysis", host: null, tab: "claude (2)", needsYou: 0 })).toBe("claude (2) — my-analysis | chimaera");
    expect(windowTitle({ workspace: null, host: null, tab: "claude (2)", needsYou: 0 })).toBe("claude (2) | chimaera");
  });
});

describe("titleHost", () => {
  it("names where a project view runs, the strip's own label — never a placeholder", () => {
    const view = { projectView: true, hostAlias: "This project", remote: true };
    const cloud = titleHost({ ...view, projectLabel: "In the cloud" });
    expect(cloud).toBe("In the cloud");
    expect(windowTitle({ workspace: "project", host: cloud, needsYou: 0 })).toBe("project •In the cloud | chimaera");
    expect(titleHost({ ...view, projectLabel: "On your computer" })).toBe("On your computer");
    expect(titleHost({ ...view, projectLabel: "In the cloud · asleep" })).toBe("In the cloud · asleep");
    // Before the first placement read nothing is known: no host at all.
    const unknown = titleHost({ ...view, projectLabel: null });
    expect(unknown).toBeNull();
    expect(windowTitle({ workspace: "project", host: unknown, needsYou: 0 })).toBe("project | chimaera");
  });

  it("keeps the host alias for any other window, and none when local", () => {
    expect(titleHost({ projectView: false, projectLabel: null, hostAlias: "hpc", remote: true })).toBe("hpc");
    expect(titleHost({ projectView: false, projectLabel: null, hostAlias: "local", remote: false })).toBeNull();
  });
});

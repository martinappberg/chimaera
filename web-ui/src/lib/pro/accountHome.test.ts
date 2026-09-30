import { describe, expect, it } from "vitest";
import { projectHref, readBrowserAccount, readHomeProjects } from "./accountHome";
import { grantedPlan, paymentDue, returningUntil } from "./status";

describe("the web Home's project addresses", () => {
  it("follows only the two same-origin project forms the account builds", () => {
    expect(projectHref("/workspace/w-1/")).toBe("/workspace/w-1/");
    expect(projectHref("/app/worker-a_1/#ws=w-2")).toBe("/app/worker-a_1/#ws=w-2");
    for (const href of [
      "https://elsewhere.invalid/workspace/w-1/", "//elsewhere.invalid/workspace/w-1/", "/workspace/w-1",
      "/workspace/../account/billing/", "/app/worker/#ws=w-2&token=x", "/account/billing", "javascript:alert(1)", 7, null,
    ]) expect(projectHref(href)).toBeNull();
  });
});

describe("the web Home's project list", () => {
  it("keeps well-formed rows once each, named ones by name and unnamed last", () => {
    const list = readHomeProjects({ pending: true, projects: [
      { workspace_id: "w-b", name: "beta", href: "/workspace/w-b/", available: true },
      { workspace_id: "w-z", name: null, href: "/workspace/w-z/", available: false },
      { workspace_id: "w-a", name: "Alpha", href: "/app/worker-x/#ws=w-a", available: true },
      { workspace_id: "w-b", name: "again", href: "/workspace/w-b/", available: true },
      { workspace_id: "bad id", name: "x", href: "/workspace/w-c/" },
      { workspace_id: "w-d", name: "Elsewhere", href: "https://elsewhere.invalid/" },
      "not a row",
    ] });
    expect(list?.pending).toBe(true);
    expect(list?.projects.map((row) => row.workspace_id)).toEqual(["w-a", "w-b", "w-z"]);
    expect(list?.projects[2]).toMatchObject({ name: null, available: false });
  });
  it("reads nothing from an unreadable answer and treats a missing flag as settled", () => {
    expect(readHomeProjects(null)).toBeNull();
    expect(readHomeProjects({ projects: "x" })).toBeNull();
    expect(readHomeProjects({ projects: [] })).toEqual({ projects: [], pending: false });
  });
});

describe("Settings → Chimaera Pro's browser account", () => {
  it("reads as a signed-in status the desktop's readers understand", () => {
    const account = readBrowserAccount({
      email: "person@example.invalid", plan: "pro", payment_due: false, returning_until: null,
      limits: { cloud_hours: 2, storage_bytes: 4 }, usage: { cloud_hours: 1, storage_bytes: 1 },
    });
    expect(account).not.toBeNull();
    expect(grantedPlan(account)).toBe("pro");
    expect(paymentDue(account)).toBe(false);
    expect(account?.usage).toEqual({ cloud_hours: 1, storage_bytes: 1 });
  });
  it("keeps payment and ended-plan state, and refuses an unknown plan", () => {
    const due = readBrowserAccount({ plan: "max", payment_due: true });
    expect(paymentDue(due)).toBe(true);
    const ended = readBrowserAccount({ plan: "pro", returning_until: "2030-01-01T00:00:00Z" });
    expect(returningUntil(ended)).toBe("2030-01-01T00:00:00Z");
    expect(grantedPlan(ended)).toBe("none");
    expect(readBrowserAccount({ plan: "enterprise" })).toBeNull();
    expect(readBrowserAccount({ plan: "pro", usage: { cloud_hours: "1" } })?.usage).toBeNull();
  });
});

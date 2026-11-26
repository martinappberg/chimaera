import { describe, expect, it } from "vitest";
import { approvalSubject, orderedOptions, type ApprovalTool } from "./approval";

const row = (over: Partial<ApprovalTool>): ApprovalTool => ({
  tool: "other",
  title: "",
  command: null,
  locations: [],
  content: null,
  ...over,
});

describe("approvalSubject", () => {
  it("reads a claude Bash request from its arguments", () => {
    expect(
      approvalSubject({ command: "cargo test", description: "Run the tests" }, row({ tool: "execute", title: "cargo test" })),
    ).toEqual({ kind: "command", command: "cargo test", description: "Run the tests" });
  });

  it("prefers the row's whole command over a clipped title, and reads Antigravity's names", () => {
    expect(
      approvalSubject(
        { CommandLine: "ls -la", Cwd: "/w", toolSummary: "List workspace files" },
        row({ tool: "execute", title: "ls…", command: "ls -la" }),
      ),
    ).toEqual({ kind: "command", command: "ls -la", description: "List workspace files" });
  });

  it("shows the diff an ACP row carries rather than its arguments", () => {
    const diff = { kind: "diff" as const, path: "/w/calc.py", old_text: "a", new_text: "b" };
    expect(approvalSubject({ TargetFile: "/w/calc.py" }, row({ tool: "edit", content: diff }))).toEqual({
      kind: "edit",
      path: "/w/calc.py",
      diffs: [diff],
    });
  });

  it("builds the change from claude Edit, MultiEdit and Write arguments", () => {
    expect(approvalSubject({ file_path: "a.rs", old_string: "x", new_string: "y" }, row({ tool: "edit" }))).toEqual({
      kind: "edit",
      path: "a.rs",
      diffs: [{ kind: "diff", path: "a.rs", old_text: "x", new_text: "y" }],
    });
    const multi = approvalSubject(
      { file_path: "a.rs", edits: [{ old_string: "1", new_string: "2" }, { old_string: "3", new_string: "4" }] },
      row({ tool: "edit" }),
    );
    expect(multi.kind === "edit" && multi.diffs.length).toBe(2);
    expect(approvalSubject({ file_path: "new.md", content: "# hi\n" }, row({ tool: "edit" }))).toEqual({
      kind: "edit",
      path: "new.md",
      diffs: [{ kind: "diff", path: "new.md", old_text: "", new_text: "# hi\n" }],
    });
  });

  it("leaves anything else to the raw details", () => {
    expect(approvalSubject({ text: "hello" }, row({ tool: "other", title: "chimaera_notify" }))).toEqual({ kind: "other" });
    // A read's `path` argument is not an edit.
    expect(approvalSubject({ path: "/etc/hosts" }, row({ tool: "read" }))).toEqual({ kind: "other" });
    expect(approvalSubject(null, null)).toEqual({ kind: "other" });
  });
});

describe("orderedOptions", () => {
  it("puts allow-once first whatever order the agent sent", () => {
    const ordered = orderedOptions([
      { id: "allow_always", label: "Allow Always (risky)", kind: "allow_always" },
      { id: "allow", label: "Allow", kind: "allow_once" },
      { id: "deny", label: "Deny", kind: "reject_once" },
    ]);
    expect(ordered.map((o) => o.id)).toEqual(["allow", "allow_always", "deny"]);
  });
});

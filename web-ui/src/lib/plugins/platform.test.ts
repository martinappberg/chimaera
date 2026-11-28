import { describe, expect, it } from "vitest";
import type { WorkspacePlugin } from "./store";
import {
  actionsFor,
  claimsFor,
  downloadWords,
  fileActionTarget,
  progressWords,
  EMPTY_PLATFORM,
  matchesPattern,
  normalizePlatform,
  plainRelative,
  resolvePlace,
  offersFor,
  pluginViewIcon,
  pluginViewTitle,
  sizeWords,
  viewClaim,
  viewsIn,
  workspaceRelative,
} from "./platform";

const platform = normalizePlatform({
  views: [
    { id: "doc", title: "Document", slot: "file" },
    { id: "board", title: "Board", slot: "tab", icon: "grid" },
    { id: "chip", title: "Status", slot: "status" },
    { id: "bad", title: "No slot", slot: "sidebar" },
    { title: "No id", slot: "tab" },
  ],
  files: [
    { match: ["*.tex"], view: "doc", label: "LaTeX" },
    { match: [], view: "doc", label: "none" },
  ],
  actions: [{ match: ["*.md"], label: "Export PDF", action: "export-pdf" }],
  settings: [
    {
      key: "engine",
      type: "enum",
      options: ["pdflatex", "xelatex"],
      default: "pdflatex",
      label: "Engine",
    },
    { key: "odd", type: "color", default: "red", label: "Not a type" },
  ],
});

function plugin(id: string, active = true, tables = platform): WorkspacePlugin {
  return {
    id,
    name: id,
    active,
    platform: tables,
  } as unknown as WorkspacePlugin;
}

describe("the 0.2 tables on the wire", () => {
  it("keeps well-formed rows and drops the rest", () => {
    expect(platform.views.map((v) => v.id)).toEqual(["doc", "board", "chip"]);
    expect(platform.views[1].icon).toBe("grid");
    expect(platform.files).toHaveLength(1);
    expect(platform.actions[0].action).toBe("export-pdf");
    expect(platform.settings.map((s) => s.key)).toEqual(["engine"]);
    expect(platform.settings[0].scope).toBe("workspace");
    expect(normalizePlatform(undefined)).toEqual(EMPTY_PLATFORM);
  });
});

describe("programs and tools on the wire", () => {
  const tables = normalizePlatform({
    programs: ["latexmk", 3],
    tools: [
      {
        id: "tinytex",
        name: "TeX Live",
        version: "2026.09",
        programs: ["latexmk"],
        download: { host: "github.com", size: 159_000_000 },
      },
      {
        id: "nobuild",
        name: "Other",
        version: "1.0",
        programs: [],
        download: null,
      },
      { id: "broken", version: "1.0" },
    ],
  });

  it("keeps the programs and well-formed tools", () => {
    expect(tables.programs).toEqual(["latexmk"]);
    expect(tables.tools.map((t) => t.id)).toEqual(["tinytex", "nobuild"]);
    expect(normalizePlatform(undefined)).toEqual(EMPTY_PLATFORM);
  });

  it("says what Install would download here", () => {
    expect(downloadWords(tables.tools[0])).toBe(
      "TeX Live 2026.09 · 152\u00a0MB from github.com",
    );
    expect(downloadWords(tables.tools[1])).toBe(
      "Other 1.0 · no build for this computer",
    );
  });
});

describe("file patterns (the daemon's rules)", () => {
  it("matches names anywhere, and paths component by component", () => {
    expect(matchesPattern("*.tex", "main.tex")).toBe(true);
    expect(matchesPattern("*.tex", "chapters/intro.tex")).toBe(true);
    expect(matchesPattern("*.tex", "main.texx")).toBe(false);
    expect(matchesPattern("docs/*.md", "docs/a.md")).toBe(true);
    expect(matchesPattern("docs/*.md", "docs/sub/a.md")).toBe(false);
    expect(matchesPattern("docs/**/*.md", "docs/a.md")).toBe(true);
    expect(matchesPattern("docs/**/*.md", "docs/x/y/a.md")).toBe(true);
    expect(matchesPattern("ma?n.typ", "main.typ")).toBe(true);
    expect(matchesPattern("a*b", "a/b")).toBe(false);
    expect(matchesPattern("docs/**", "docs/a/b.md")).toBe(true);
    expect(matchesPattern("a/**/**/b", "a/b")).toBe(true);
    expect(matchesPattern("a/**/b", "a/x/c")).toBe(false);
  });

  it("costs no more with many `**`s", () => {
    const many = `${"**/".repeat(41)}x`;
    const started = performance.now();
    expect(matchesPattern(many, "a/b/c/d/e/f/g")).toBe(false);
    expect(matchesPattern(many, "a/b/c/d/e/f/x")).toBe(true);
    expect(performance.now() - started).toBeLessThan(100);
  });

  it("resolves a screen's paths only inside the workspace or the output folder", async () => {
    expect(plainRelative("a/b.tex")).toBe(true);
    expect(plainRelative("../x")).toBe(false);
    expect(plainRelative("a/../../x")).toBe(false);
    expect(plainRelative("/etc/passwd")).toBe(false);
    expect(plainRelative("")).toBe(false);
    await expect(resolvePlace("w", "/w/p", "demo", "a/b.tex")).resolves.toBe("/w/p/a/b.tex");
    await expect(resolvePlace("w", "/w/p", "demo", "/Users/me/.ssh/id_rsa")).rejects.toThrow();
    await expect(resolvePlace("w", "/w/p", "demo", "../x")).rejects.toThrow();
    await expect(resolvePlace("w", "/w/p", "demo", "output:../../x")).rejects.toThrow();
    await expect(resolvePlace("w", null, "demo", "a.tex")).rejects.toThrow();
  });

  it("is relative to the workspace root", () => {
    expect(workspaceRelative("/w/p", "/w/p/a/b.tex")).toBe("a/b.tex");
    expect(workspaceRelative("/w/p/", "/w/p/a.tex")).toBe("a.tex");
    expect(workspaceRelative("/w/p", "/w/pp/a.tex")).toBeNull();
    expect(workspaceRelative(null, "/w/p/a.tex")).toBeNull();
  });
});

describe("what a plugin draws where", () => {
  it("claims files only while active, first claimant first", () => {
    const all = [plugin("latex"), plugin("off", false), plugin("other")];
    expect(claimsFor(all, "a/main.tex").map((c) => c.plugin.id)).toEqual([
      "latex",
      "other",
    ]);
    expect(claimsFor(all, "a/main.tex")[0].view.title).toBe("Document");
    expect(claimsFor(all, "notes.md")).toEqual([]);
    expect(actionsFor(all, "notes.md").map((a) => a.action.label)).toEqual([
      "Export PDF",
      "Export PDF",
    ]);
    expect(
      viewsIn(all, "tab").map((v) => `${v.plugin.id}/${v.view.id}`),
    ).toEqual(["latex/board", "other/board"]);
  });

  it("offers Turn on only where a switch alone would make it open the file", () => {
    const off = (id: string, over: Partial<WorkspacePlugin> = {}): WorkspacePlugin =>
      ({
        ...plugin(id, false),
        installed: true,
        on: false,
        detected: true,
        hold: null,
        fault: null,
        ...over,
      }) as WorkspacePlugin;
    const all = [
      plugin("active"),
      off("latex"),
      off("available", { installed: false }),
      off("held", { hold: { kind: "untrusted" } }),
      off("faulted", { fault: "needs a newer chimaera" }),
      off("elsewhere", { detected: false }),
      off("typst", { platform: EMPTY_PLATFORM }),
    ];
    expect(offersFor(all, "ch/one.tex").map((o) => o.plugin.id)).toEqual(["latex"]);
    expect(offersFor(all, "ch/one.tex")[0].kind.label).toBe("LaTeX");
    expect(offersFor(all, "notes.md")).toEqual([]);
  });

  it("names a tab by its view's title", () => {
    expect(pluginViewTitle([plugin("latex")], "latex", "board")).toBe("Board");
    expect(pluginViewTitle([], "latex", "board")).toBe("board");
  });

  it("gives a tab the icon its view names, and nothing for a name outside the set", () => {
    expect(pluginViewIcon([plugin("latex")], "latex", "board")).toEqual(expect.arrayContaining([expect.stringMatching(/^M/)]));
    expect(pluginViewIcon([plugin("latex")], "latex", "doc")).toBeNull();
    expect(pluginViewIcon([plugin("latex", true, normalizePlatform({ views: [{ id: "v", title: "V", slot: "tab", icon: "toString" }] }))], "latex", "v")).toBeNull();
    expect(pluginViewIcon(undefined, "latex", "board")).toBeNull();
  });

  it("says sizes in words", () => {
    expect(sizeWords(512)).toBe("512 B");
    expect(sizeWords(1536)).toBe("1.5 KB");
    expect(sizeWords(12 * 1024 * 1024)).toBe("12 MB");
  });
});

describe("install progress in words", () => {
  it("says the stage, and the bytes while downloading", () => {
    expect(progressWords({ stage: "downloading", done: 50 * 1024 * 1024, total: 150 * 1024 * 1024 })).toEqual({
      text: "Downloading · 50 MB of 150 MB",
      fraction: 1 / 3,
    });
    expect(progressWords({ stage: "unpacking", done: 0, total: 0 })).toEqual({ text: "Unpacking", fraction: null });
    expect(progressWords(null)).toEqual({ text: "Starting", fraction: null });
  });
});

describe("railViews", () => {
  it("lists the tab views of active plugins that ask for a sidebar row", async () => {
    const { railViews, normalizePlatform } = await import("./platform");
    const platform = normalizePlatform({
      views: [
        { id: "board", title: "Web board", slot: "tab", icon: "grid", rail: true },
        { id: "side", title: "Side", slot: "panel", rail: true },
        { id: "plain", title: "Plain", slot: "tab" },
      ],
    });
    const on = { id: "test-web", active: true, platform } as unknown as WorkspacePlugin;
    const off = { ...on, id: "other", active: false } as typeof on;
    const rows = railViews([on, off]);
    expect(rows.map((r) => [r.plugin, r.view, r.title])).toEqual([["test-web", "board", "Web board"]]);
    expect(rows[0].icon).not.toBeNull();
    expect(railViews(undefined)).toEqual([]);
  });
});

describe("a file action's open", () => {
  const all = [plugin("latex"), plugin("off", false)];
  it("opens a tab view, or a file view on the named file or the action's own", () => {
    expect(fileActionTarget(all, "latex", { view: "board", file: null }, "a.md")).toEqual({ kind: "tab", view: "board" });
    expect(fileActionTarget(all, "latex", { view: "doc", file: "b/c.tex" }, "a.md")).toEqual({ kind: "file", view: "doc", file: "b/c.tex" });
    expect(fileActionTarget(all, "latex", { view: "doc", file: null }, "a.md")).toEqual({ kind: "file", view: "doc", file: "a.md" });
  });
  it("opens nothing on a file that isn't a plain workspace path", () => {
    for (const bad of ["../x", "/etc/passwd", "a/../b", "a//b", "./a", "a\\b"]) {
      expect(fileActionTarget(all, "latex", { view: "doc", file: bad }, "a.md")).toBeNull();
    }
  });
  it("opens nothing for a view the plugin lacks or that has no place to open", () => {
    expect(fileActionTarget(all, "latex", { view: "nope", file: null }, "a.md")).toBeNull();
    expect(fileActionTarget(all, "latex", { view: "chip", file: null }, "a.md")).toBeNull();
    expect(fileActionTarget(all, "ghost", { view: "doc", file: null }, "a.md")).toBeNull();
  });
  it("shows a file view on a file its pattern doesn't claim, only while the plugin is on", () => {
    const c = viewClaim(all, "latex", "doc");
    expect(c?.view.id).toBe("doc");
    expect(c?.kind.match).toEqual([]);
    expect(viewClaim(all, "latex", "board")).toBeNull();
    expect(viewClaim(all, "off", "doc")).toBeNull();
  });
});

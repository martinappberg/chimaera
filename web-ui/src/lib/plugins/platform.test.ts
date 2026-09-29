import { describe, expect, it } from "vitest";
import type { WorkspacePlugin } from "./store";
import {
  actionsFor,
  claimsFor,
  downloadWords,
  progressWords,
  EMPTY_PLATFORM,
  matchesPattern,
  normalizePlatform,
  pluginViewTitle,
  sizeWords,
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

  it("names a tab by its view's title", () => {
    expect(pluginViewTitle([plugin("latex")], "latex", "board")).toBe("Board");
    expect(pluginViewTitle([], "latex", "board")).toBe("board");
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

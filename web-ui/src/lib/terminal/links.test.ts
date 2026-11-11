import { describe, expect, it } from "vitest";
import {
  bareLinkable,
  extractCandidates,
  nativeToWire,
  windowsHost,
  wrappedRefs,
  wrappedTokens,
} from "./links";

const raws = (s: string) => extractCandidates(s).map((c) => c.raw);
const bareRaws = (s: string) => extractCandidates(s, true).map((c) => c.raw);

describe("extractCandidates", () => {
  it("finds the path shapes terminals print", () => {
    expect(raws("see results/qc/report.html now")).toEqual(["results/qc/report.html"]);
    expect(raws("cat /etc/hosts")).toEqual(["/etc/hosts"]);
    expect(raws("ls ~/data and ./main.rs and ../up.txt")).toEqual(["~/data", "./main.rs", "../up.txt"]);
    expect(raws("open haiku.txt please")).toEqual(["haiku.txt"]);
    expect(raws("ls -la .claude .env here")).toEqual([".claude", ".env"]);
    expect(raws("cat .config/nvim/init.lua")).toEqual([".config/nvim/init.lua"]);
    expect(raws("dir results/ listed")).toEqual(["results/"]);
  });
  it("skips prose, versions, URLs, flags and ellipses", () => {
    expect(raws("plain words never qualify")).toEqual([]);
    expect(raws("versions 1.2.3 and 4.8 skip")).toEqual([]);
    expect(raws("https://support.claude.com/en/a-b")).toEqual([]);
    expect(raws("wait... then go")).toEqual([]);
    expect(raws("--color=always -la")).toEqual([]);
  });
  it("trims sentence punctuation and wrappers", () => {
    expect(raws("see main.rs.")).toEqual(["main.rs"]);
    expect(raws('saved ("results/plot.png").')).toEqual(["results/plot.png"]);
  });
  it("keeps the line and underlines the suffix", () => {
    const [c] = extractCandidates("err at src/lib.rs:42 here");
    expect(c.raw).toBe("src/lib.rs");
    expect(c.line).toBe(42);
    expect(c.length).toBe("src/lib.rs:42".length);
    const [anchor] = extractCandidates("see src/lib.rs#L42-L50");
    expect(anchor.ref).toEqual({ path: "src/lib.rs", line: 42, endLine: 50 });
    expect(anchor.length).toBe("src/lib.rs#L42-L50".length);
  });
  it("reads mentions, diff sides, abbreviations and Unicode names", () => {
    expect(raws("@src/x.ts changed")).toEqual(["src/x.ts"]);
    expect(raws("--- a/src/x.rs")).toEqual(["a/src/x.rs"]);
    expect(raws("wrote …/figs/plot.png")).toEqual(["figs/plot.png"]);
    expect(raws("the résumé.pdf and données/x.csv")).toEqual(["résumé.pdf", "données/x.csv"]);
    expect(raws("⏺ Update(web-ui/src/App.svelte)")).toEqual(["web-ui/src/App.svelte"]);
  });

  // Bare mode (hover only): single-segment names — a directory like `crates`,
  // an extensionless file like `justfile` — become candidates. The prefetch
  // never uses it, so whole screens of prose are never mass-validated.
  it("admits bare names only on hover", () => {
    expect(bareRaws("cd crates")).toEqual(["cd", "crates"]);
    expect(bareRaws("run justfile")).toEqual(["run", "justfile"]);
    expect(bareRaws("bump to 1.2.3 or 4.8")).toEqual(["bump", "to", "or"]);
    expect(bareRaws("-la --color")).toEqual([]);
    expect(bareRaws("plain words here")).toHaveLength(3);
    expect(raws("plain words here")).toHaveLength(0);
  });
});

describe("bareLinkable", () => {
  // Being a candidate is not enough: a bare name only LINKS on a line shape
  // prose never has. `has(...)` stands in for "the daemon confirmed this path".
  const has =
    (...names: string[]) =>
    (w: string) =>
      names.includes(w);
  const bare = (s: string, r: (w: string) => boolean) => [...bareLinkable(s, r)].join();
  it("links every word of a listing line", () => {
    expect(bare("Cargo.lock  crates  target", has("Cargo.lock", "crates", "target"))).toBe(
      "Cargo.lock,crates,target",
    );
    expect(bare("crates", has("crates"))).toBe("crates");
  });
  it("links the name of an ls -l entry", () => {
    expect(bare("drwxr-xr-x 5 me staff 160 Jul 7 18:04 crates", has("crates"))).toBe("crates");
    expect(bare("drwxr-xr-x 5 me staff 160 Jul 7 18:04 gone", has("crates"))).toBe("");
  });
  it("never links bare words in prose or commands", () => {
    expect(bare("update the docs now", has("docs"))).toBe("");
    expect(bare("cd crates", has("crates"))).toBe("");
    expect(bare("me@host chimaera % ls", has("chimaera"))).toBe("");
  });
});

describe("hard-wrapped paths", () => {
  /** Rows as xterm renders them: padded with spaces to the terminal width. */
  const pad = (rows: string[], width = 40) => rows.map((r) => r.padEnd(width, " "));

  it("re-joins a path a TUI broke at its box width", () => {
    const rows = pad(["  ⎿  Wrote to /home/u/proj/results/fi", "     gs/plot.png:3 (12 lines)"]);
    const [w] = wrappedRefs(rows);
    expect(w.ref).toEqual({ path: "/home/u/proj/results/figs/plot.png", line: 3 });
    expect(w.first).toEqual({ row: 0, index: rows[0].indexOf("/home") });
    expect(w.last).toEqual({ row: 1, index: rows[1].indexOf(":3") + 1 });
  });

  it("follows a continuation that fills its row onto the next", () => {
    const rows = pad(
      ["Saved to the output dir /data/a", "/very/long/directory/name/that/fills", "/x.csv done"],
      36,
    );
    const texts = wrappedTokens(rows).map((w) => w.text);
    expect(texts).toContain("/data/a/very/long/directory/name/that/fills/x.csv");
    const refs = wrappedRefs(rows).map((w) => w.ref.path);
    expect(refs).toContain("/data/a/very/long/directory/name/that/fills/x.csv");
  });

  it("leaves a token that ended well short of the edge alone", () => {
    expect(wrappedTokens(pad(["see src/x.rs", "and then more"], 80))).toEqual([]);
  });

  it("drops a join that is not a path", () => {
    expect(wrappedRefs(pad(["1.2.3", "4.5"], 5))).toEqual([]);
  });
});

describe("Windows shells", () => {
  const winRaws = (s: string) => extractCandidates(s, false, true).map((c) => c.raw);

  it("knows a Windows host by its directories' root", () => {
    expect(windowsHost({ cwd: "C:/Users/me/proj", root: "C:/Users/me/proj", workspaceId: null })).toBe(true);
    expect(windowsHost({ cwd: null, spawnCwd: null, root: "//srv/share/proj", workspaceId: null })).toBe(true);
    expect(windowsHost({ cwd: "/home/u", root: "/home/u", workspaceId: null })).toBe(false);
    expect(windowsHost({ cwd: null, root: null, workspaceId: null })).toBe(false);
  });

  it("converts native paths index for index", () => {
    const line = "  --> c:\\Users\\me\\proj\\src\\a.rs:12:5";
    const wire = nativeToWire(line);
    expect(wire).toBe("  --> C:/Users/me/proj/src/a.rs:12:5");
    expect(wire.length).toBe(line.length);
    expect(nativeToWire("no separators")).toBe("no separators");
  });

  it("links PowerShell's native paths in wire form", () => {
    const [c] = extractCandidates("error at C:\\Users\\me\\proj\\src\\a.rs:12 here", false, true);
    expect(c.raw).toBe("C:/Users/me/proj/src/a.rs");
    expect(c.line).toBe(12);
    expect(c.start).toBe("error at ".length);
    expect(c.length).toBe("C:\\Users\\me\\proj\\src\\a.rs:12".length);
    expect(winRaws("see .\\src\\a.rs and ..\\up\\b.txt")).toEqual(["./src/a.rs", "../up/b.txt"]);
    expect(winRaws("  --> src\\main.rs:2:5")).toEqual(["src/main.rs"]);
    expect(winRaws("Directory: C:\\Users\\me")).toEqual(["C:/Users/me"]);
    expect(winRaws("wrote C:/Users/me/out.png")).toEqual(["C:/Users/me/out.png"]);
  });

  it("leaves a Unix host's backslashes alone", () => {
    expect(raws("C:\\Users\\me\\x.txt and .\\src\\a.rs")).toEqual([]);
    expect(raws("printf 'a\\nb' > out/x.txt")).toEqual(["out/x.txt"]);
  });
});

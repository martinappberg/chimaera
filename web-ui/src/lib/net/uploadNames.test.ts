import { describe, expect, it } from "vitest";
import { extractFileRefs } from "../shared/fileRef";
import { composeAgentPathReference, composeProvenanceSuffix, composeSelectionReference } from "../shared/reference";
import fixture from "./uploadNames.fixture.json";

// The @mention typed after an upload must name the whole landed path, for the
// agent and for this UI's own link parser. The daemon picks the name
// (upload.rs mention_safe_name, pinned by the same fixture); these tests read
// it back the way each consumer does.

/** A landing pad the way the daemon lays it out (`<data>/uploads/<session>/`). */
const PAD = "/home/me/.chimaera/uploads/s-1a2b3c4d/";

/** claude's @-mention extractor, copied from 2.1.283 (the prompt-attachment
 *  pass that reads mentioned files in the TUI and over stream-json alike):
 *  quoted `@"…"` mentions, then bare `@path` up to an ASCII word boundary,
 *  each with a `#L…` anchor split off. Re-copy it when claude's changes. */
function claudeMentions(text: string): string[] {
  const quoted = /(^|[\s。、？！])@"([^"]+)"/g;
  const bare = /(^|[\s。、？！])@([^\s]+)\b/g;
  const found: string[] = [];
  for (const m of text.matchAll(quoted)) if (m[2] && !m[2].endsWith(" (agent)")) found.push(m[2]);
  for (const m of text.match(bare) ?? []) {
    const p = m.slice(m.indexOf("@") + 1);
    if (!p.startsWith('"')) found.push(p);
  }
  return [...new Set(found)].map((p) => /^([^#]+)(?:#L(\d+)(?:-(\d+))?)?(?:#[^#]*)?$/.exec(p)?.[1] ?? p);
}

/** claude's MCP-resource mention (`@server:uri`), which a path must never look like. */
const CLAUDE_RESOURCE = /(^|[\s。、？！])@([^\s]+:[^\s]+)\b/;

describe("uploadNames.fixture.json: a landed name's mention reads whole", () => {
  it("has cases", () => expect(fixture.cases.length).toBeGreaterThan(0));
  for (const c of fixture.cases) {
    it(`${c.note}: ${JSON.stringify(c.safe)}`, () => {
      const path = `${PAD}${c.safe}`;
      const typed = composeAgentPathReference(path);
      expect(typed).toBe(`@${path} `);

      expect(claudeMentions(typed)).toEqual([path]);
      expect(CLAUDE_RESOURCE.test(typed)).toBe(false);

      const refs = extractFileRefs(typed);
      expect(refs.map((f) => f.ref.path)).toEqual([path]);
      expect(typed.slice(refs[0].start, refs[0].end)).toBe(`@${path}`);
    });
  }
});

describe("the mention an upload types", () => {
  it("a raw spaced name would break both parsers (why the daemon renames)", () => {
    const typed = `@${PAD}Screenshot 2026-09-26 at 12.30.png `;
    expect(claudeMentions(typed)).toEqual([`${PAD}Screenshot`]);
    expect(extractFileRefs(typed).map((f) => f.ref.path)).not.toContain(
      `${PAD}Screenshot 2026-09-26 at 12.30.png`,
    );
  });

  it("a spaced directory above the pad falls back to claude's quoted form", () => {
    const path = "/Users/Jo Smith/.chimaera/uploads/s-1/Screenshot-2026-09-26-at-12.30.png";
    const typed = composeAgentPathReference(path);
    expect(typed).toBe(`@"${path}" `);
    expect(claudeMentions(typed)).toEqual([path]);

    const refs = extractFileRefs(typed);
    expect(refs.map((f) => f.ref.path)).toEqual([path]);
    expect(typed.slice(refs[0].start, refs[0].end)).toBe(`@"${path}"`);
  });

  it("a name ending in a non-ASCII character quotes (claude's \\b would cut it)", () => {
    const path = `${PAD}\u5831\u544a`;
    expect(claudeMentions(`@${path} `)).not.toEqual([path]);
    const typed = composeAgentPathReference(path);
    expect(typed).toBe(`@"${path}" `);
    expect(claudeMentions(typed)).toEqual([path]);
    expect(extractFileRefs(typed).map((f) => f.ref.path)).toEqual([path]);
  });

  it("a spaced selection reference and provenance keep their lines inside the quotes", () => {
    const sel = { kind: "file" as const, path: "/w/raw data/qc.tsv", startLine: 3, endLine: 9, text: "x" };
    const selection = composeSelectionReference("raw data/qc.tsv", sel, "terminal");
    const provenance = `pasted${composeProvenanceSuffix(sel, "raw data/qc.tsv", null)}`;
    expect(claudeMentions(selection)).toEqual(["raw data/qc.tsv"]);
    expect(claudeMentions(provenance)).toEqual(["raw data/qc.tsv"]);
    for (const typed of [selection, provenance]) {
      expect(extractFileRefs(typed).map((f) => ({ path: f.ref.path, line: f.ref.line, endLine: f.ref.endLine }))).toEqual([
        { path: "raw data/qc.tsv", line: 3, endLine: 9 },
      ]);
    }
  });

  it("a folder mention quotes with its trailing slash inside", () => {
    const typed = composeAgentPathReference("raw data/");
    expect(typed).toBe('@"raw data/" ');
    expect(claudeMentions(typed)).toEqual(["raw data/"]);
    expect(extractFileRefs(typed).map((f) => f.ref.path)).toEqual(["raw data/"]);
  });
});

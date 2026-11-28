import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { describeManifest } from "./plugin-caps.mjs";
import {
  capabilityChange,
  capabilityLines,
  code,
  commandMessage,
  manifestFields,
  md,
  newer,
  outputs,
  parseLock,
  parseSums,
  pullRequest,
  review,
  setLockValues,
  toolArtifacts,
} from "./plugin-lock.mjs";

const LOCK = `# comment
[[plugin]]
id = "latex"
name = "LaTeX"
summary = "Build .tex to PDF."
category = "writing"
version = "0.1.3"
repo = "martinappberg/chimaera-plugin-latex"
sha256_wasm = "${"a".repeat(64)}"
sha256_toml = "${"b".repeat(64)}"
tier = "privileged"
caps = "${"1".repeat(64)}"

[[plugin]]
id = "mycelium"
name = "Mycelium"
summary = "Project memory — findings, decisions, learnings."   # the card's line
category = "knowledge"
version = "0.1.2"
repo = "martinappberg/chimaera-plugin-mycelium"
sha256_wasm = "${"c".repeat(64)}"
sha256_toml = "${"d".repeat(64)}"
tier = "sandboxed"
caps = "${"2".repeat(64)}"
`;

test("the repository's own lock parses", () => {
  const entries = parseLock(readFileSync(new URL("../../plugins/plugins.lock", import.meta.url), "utf8"));
  assert.ok(entries.length >= 1);
  for (const e of entries) assert.match(e.version, /^\d+\.\d+\.\d+$/);
});

test("a bump rewrites only that entry's values, comments and layout kept", () => {
  const [, mycelium] = parseLock(LOCK);
  assert.equal(mycelium.id, "mycelium");
  const next = setLockValues(LOCK, mycelium, {
    version: "0.1.3",
    sha256_wasm: "e".repeat(64),
    sha256_toml: "f".repeat(64),
    name: "Mycelium",
    summary: "Project memory — findings, decisions, learnings.",
  });
  assert.equal(next.split("\n").length, LOCK.split("\n").length);
  const [latex, bumped] = parseLock(next);
  assert.equal(latex.version, "0.1.3");
  assert.equal(latex.sha256_wasm, "a".repeat(64));
  assert.equal(bumped.version, "0.1.3");
  assert.equal(bumped.sha256_wasm, "e".repeat(64));
  assert.match(next, /learnings\."   # the card's line/);
});

test("a value the lock can't hold is refused, not written", () => {
  const [entry] = parseLock(LOCK);
  assert.throws(() => setLockValues(LOCK, entry, { summary: 'says "hi"' }), /can't store/);
  assert.throws(() => setLockValues(LOCK, entry, { summary: "two\nlines" }), /can't store/);
});

test("the lock grammar is strict", () => {
  assert.throws(() => parseLock(`[[plugin]]\nid = "x"\n`), /has no name/);
  assert.throws(() => parseLock(`${LOCK}\nextra = 1\n`), /not a \[\[plugin\]\] header/);
  assert.throws(() => parseLock(LOCK.replace('version = "0.1.2"', 'version = "0.1.2"\nversion = "0.1.3"')), /set twice/);
  assert.throws(() => parseLock(LOCK.replace('version = "0.1.2"', 'version = "latest"')), /has version/);
});

test("a manifest's lock fields: top level before the first table, [release] github", () => {
  const toml = `# Mycelium
id = "mycelium"
name = "Mycelium"
version = "0.1.3"
summary = "Project memory — findings."
description = "Long \\"quoted\\" prose"
api = "0.1"

[detect]
any = [".living/INDEX.md"]

[recommends]
summary = "not the plugin's summary"

[release]
github = "martinappberg/chimaera-plugin-mycelium"
`;
  assert.deepEqual(manifestFields(toml), {
    id: "mycelium",
    name: "Mycelium",
    summary: "Project memory — findings.",
    version: "0.1.3",
    github: "martinappberg/chimaera-plugin-mycelium",
  });
  assert.equal(manifestFields('name = "a \\"b\\""\n').name, undefined, "an escaped string reads as absent");
});

test("SHA256SUMS in sha256sum's forms", () => {
  const sums = parseSums(`${"A".repeat(64)}  plugin.wasm\n${"b".repeat(64)} *./plugin.toml\nnot a line\n`);
  assert.equal(sums.get("plugin.wasm"), "a".repeat(64));
  assert.equal(sums.get("plugin.toml"), "b".repeat(64));
  assert.equal(sums.size, 2);
});

test("versions compare numerically", () => {
  assert.equal(newer("0.1.10", "0.1.9"), true);
  assert.equal(newer("0.2.0", "0.1.99"), true);
  assert.equal(newer("0.1.2", "0.1.2"), false);
  assert.equal(newer("0.1.1", "0.1.2"), false);
});

test("the pull request is a fix: (a patch release ships the new lock)", () => {
  const bump = (id, name, to) => ({
    id, name, to, from: "0.1.0", repo: `o/${id}`, url: "https://x", compare: "https://y", sha256_wasm: "a", sha256_toml: "b", wasm_bytes: 1,
    tier: "sandboxed", from_tier: "sandboxed", caps: "1".repeat(64), from_caps: "1".repeat(64), can: [], added: [], removed: [],
    unreadable: null, lock_drift: null, automerge: true, why: null,
  });
  const one = pullRequest([bump("mycelium", "Mycelium", "0.1.3")]);
  assert.equal(one.title, "fix: update Mycelium to 0.1.3");
  assert.equal(one.branch, "plugin-lock/mycelium-0.1.3");
  const two = pullRequest([bump("latex", "LaTeX", "0.1.4"), bump("mycelium", "Mycelium", "0.1.3")]);
  assert.equal(two.title, "fix: update first-party plugins (LaTeX 0.1.4, Mycelium 0.1.3)");
  assert.equal(two.branch, "plugin-lock/latex-0.1.4+mycelium-0.1.3");
  assert.match(two.body, /\| Mycelium \(`mycelium`\) \| 0\.1\.0 \| 0\.1\.3 \|/);
  assert.equal(two.automerge, true);
  assert.match(two.body, /Auto-merge \(squash\) is on/);
  const grown = pullRequest([{ ...bump("mycelium", "Mycelium", "0.2.0"), automerge: false, why: "its capability lines changed: it may ask for more" }]);
  assert.equal(grown.automerge, false, "asking for more waits for a person");
  assert.match(grown.body, /no auto-merge/);
  assert.match(grown.body, /`mycelium` 0\.2\.0: its capability lines changed/);
});

test("capability lines: api and the tables that say what it can do, not the card's words", () => {
  const base = `# a comment
id = "x"
name = "X"
version = "0.1.0"
summary = "One line."
api = "0.1"

[provides]
mcp_tools = ["a", "b"]

[adds]
ui = ["words"]

[release]
github = "o/x"
`;
  assert.deepEqual(capabilityLines(base), ['api = "0.1"', "[provides]", 'mcp_tools = ["a", "b"]']);
  const reworded = base.replace('summary = "One line."', 'summary = "Other words."').replace('ui = ["words"]', 'ui = ["new words"]').replace('version = "0.1.0"', 'version = "0.2.0"');
  assert.deepEqual(capabilityLines(reworded), capabilityLines(base), "new words and a new version ask for nothing");
  const grown = base.replace('mcp_tools = ["a", "b"]', 'mcp_tools = ["a", "b", "c"]');
  assert.notDeepEqual(capabilityLines(grown), capabilityLines(base));
  const access = `${base}\n[access]\ntimeline = "none"\n`;
  assert.notDeepEqual(capabilityLines(access), capabilityLines(base), "even a narrowing is reviewed");
  assert.notDeepEqual(capabilityLines(base.replace('api = "0.1"', 'api = "0.2"')), capabilityLines(base), "the API decides the defaults");
});

test("tool downloads: every [[tools.artifacts]] with its tool, read by the TOML parse", () => {
  const toml = [
    'id = "latex"',
    "[[tools]]",
    'id = "tinytex"',
    'version = "2026.09"',
    "[[tools.artifacts]]",
    'platform = "linux-x86_64"',
    'url = "https://github.com/rstudio/tinytex-releases/releases/download/v2026.09/TinyTeX-1.tar.xz"',
    `sha256 = "${"a".repeat(64)}"`,
    "size = 159_000_000",
    "[[tools.artifacts]]",
    'platform = "macos-aarch64"',
    "url = 'not-https'",
    "size = -1",
    "[[tools.setup]]",
    'program = "tlmgr"',
    "[[tools]]",
    'id = "other"',
    "[[tools.artifacts]]",
    'platform = "linux-x86_64"',
    'url = "https://example.org/other.zip"',
    `sha256 = "${"b".repeat(64)}"`,
  ].join("\n");
  assert.deepEqual(toolArtifacts(toml), [
    {
      tool: "tinytex",
      platform: "linux-x86_64",
      url: "https://github.com/rstudio/tinytex-releases/releases/download/v2026.09/TinyTeX-1.tar.xz",
      sha256: "a".repeat(64),
      size: 159000000,
    },
    { tool: "tinytex", platform: "macos-aarch64", url: "not-https", sha256: null, size: null },
    { tool: "other", platform: "linux-x86_64", url: "https://example.org/other.zip", sha256: "b".repeat(64), size: null },
  ]);
  assert.deepEqual(toolArtifacts('id = "notes"\n[adds]\nui = ["x"]\n'), []);
  assert.throws(() => toolArtifacts("x = 1979-05-27\n"), /dates and times/, "TOML it can't read is refused");
});

test("tool downloads written inline are the same list as [[tools.artifacts]] tables", () => {
  const a = "a".repeat(64);
  const b = "b".repeat(64);
  const tables = [
    'api = "0.2"',
    "[[programs]]",
    'name = "typst"',
    "[[tools]]",
    'id = "typst"',
    'name = "Typst"',
    'version = "0.14.2"',
    'programs = ["typst"]',
    "[[tools.artifacts]]",
    'platform = "linux-x86_64"',
    'url = "https://example.org/typst.tar.xz"',
    `sha256 = "${a}"`,
    "size = 1_000",
    'unpack = "tar.xz"',
    "[[tools.artifacts]]",
    'platform = "macos-aarch64"',
    'url = "https://example.org/typst.zip"',
    `sha256 = "${b}"`,
    "size = 2000",
    'unpack = "zip"',
  ].join("\n");
  const inline = [
    'api = "0.2"',
    'programs = [{ name = "typst" }]',
    "[[tools]]",
    'id = "typst"',
    'name = "Typst"',
    'version = "0.14.2"',
    'programs = ["typst"]',
    "artifacts = [",
    `  { platform = "linux-x86_64", url = "https://example.org/typst.tar.xz", sha256 = "${a}", size = 1000, unpack = "tar.xz" },`,
    `  { platform = "macos-aarch64", url = 'https://example.org/typst.zip', sha256 = "${b}", size = 2_000, unpack = "zip" },`,
    "]",
  ].join("\n");
  const want = [
    { tool: "typst", platform: "linux-x86_64", url: "https://example.org/typst.tar.xz", sha256: a, size: 1000 },
    { tool: "typst", platform: "macos-aarch64", url: "https://example.org/typst.zip", sha256: b, size: 2000 },
  ];
  assert.deepEqual(toolArtifacts(tables), want);
  assert.deepEqual(toolArtifacts(inline), want);
  // The capability parse sees the same downloads in both forms.
  const atoms = (t) => describeManifest(`id = "d"\nname = "D"\nversion = "0.1.0"\nsummary = "s"\n${t}`).atoms;
  assert.deepEqual(atoms(inline), atoms(tables));
});

// --- A bump that needs review arrives ready: tier, caps and the change in words.

/** A release's plugin.toml for the bump tests. */
const release = (version, extra = "", name = "Notes") =>
  [
    'id = "notes"',
    `name = "${name}"`,
    `version = "${version}"`,
    'summary = "Notes for agents."',
    'api = "0.2"',
    "",
    "[access]",
    'files = "read"',
    "",
    "[provides]",
    'mcp_tools = ["notes_read"]',
    extra,
    "",
    "[adds]",
    'agents = ["notes_read"]',
    "",
    "[release]",
    'github = "o/notes"',
    "",
  ].join("\n");

/** What `check` returns for a bump from `pinned` to `next`, without the
 *  network: the same functions, the same order. */
function bumpOf(pinned, next, { tier } = {}) {
  const before = describeManifest(pinned);
  const entry = { id: "notes", tier: tier ?? before.tier, caps: before.caps };
  const m = manifestFields(next);
  return {
    id: "notes",
    name: m.name,
    summary: m.summary,
    repo: "o/notes",
    from: before.version,
    to: m.version,
    url: `https://github.com/o/notes/releases/tag/v${m.version}`,
    compare: `https://github.com/o/notes/compare/v${before.version}...v${m.version}`,
    sha256_wasm: "e".repeat(64),
    sha256_toml: "f".repeat(64),
    wasm_bytes: 1,
    tools_checked: 0,
    ...capabilityChange(entry, pinned, next),
    ...review(entry, pinned, next),
  };
}

/** The lock entry for a pinned release, as the lock records it. */
const entryOf = (pinned, over = {}) => {
  const d = describeManifest(pinned);
  return { id: "notes", tier: d.tier, caps: d.caps, ...over };
};

test("auto-merge needs a plugin that is and stays sandboxed with the approved digest (the safeguard)", () => {
  const pinned = release("0.1.0");
  const reworded = release("0.1.1").replace("Notes for agents.", "Other words.");
  const grown = release("0.1.1", '[[programs]]\nname = "pandoc"');
  const sandboxed = entryOf(pinned);
  // Every case of the rule, spelled out: loosening any of them must fail here.
  assert.deepEqual(review(sandboxed, pinned, reworded), { automerge: true, why: null });
  assert.equal(review(sandboxed, pinned, grown).automerge, false, "new capabilities wait for a person");
  assert.equal(review({ ...sandboxed, tier: "privileged" }, pinned, reworded).automerge, false, "a privileged plugin always waits, even unchanged");
  assert.equal(review({ ...sandboxed, tier: "privileged" }, pinned, grown).automerge, false);
  assert.equal(review({ ...sandboxed, tier: "" }, pinned, reworded).automerge, false, "only the exact word sandboxed merges by itself");
  assert.equal(review({ ...sandboxed, tier: "Sandboxed" }, pinned, reworded).automerge, false);
  // The line comparison stays as a stricter extra: lines that differ only in
  // a way the digest ignores (an inline comment) still wait.
  const commented = pinned.replace('files = "read"', 'files = "read" # still read');
  assert.equal(describeManifest(commented).caps, describeManifest(pinned).caps);
  assert.deepEqual(review(sandboxed, pinned, commented), { automerge: false, why: "its capability lines changed" });
  // The pull request follows the bumps: one that waits holds back all.
  const ok = bumpOf(pinned, reworded);
  const waits = { ...bumpOf(pinned, grown), id: "other", to: "0.2.0" };
  assert.equal(pullRequest([ok]).automerge, true);
  assert.equal(pullRequest([ok, waits]).automerge, false);
  assert.equal(outputs([ok], []), "bumped=true\nautomerge=true\nproblems=0\n");
  assert.equal(outputs([ok, waits], []), "bumped=true\nautomerge=false\nproblems=0\n");
  assert.equal(outputs([], ["x"]), "bumped=false\nautomerge=false\nproblems=1\n");
});

/** `release` without its [provides] table: capability tables written as
 *  top-level keys then precede every table. */
const noProvides = (version, top = "") => release(version).replace('[provides]\nmcp_tools = ["notes_read"]\n', "").replace('api = "0.2"', `api = "0.2"${top}`);

test("capability changes the line heuristic can't see never merge by themselves", () => {
  // Each of these has the pinned release's capability lines (the old,
  // line-based rule said "unchanged" and would have merged it).
  const pinned = release("0.1.0");
  const topLevelProgram = release("0.1.1").replace('api = "0.2"', 'api = "0.2"\nprograms = [{ name = "bash" }]');
  const commentedHeader = `${release("0.1.1")}[[programs]] # x\nname = "bash"\n`;
  const dottedPinned = noProvides("0.1.0");
  const dottedTool = noProvides("0.1.1", '\nprovides.mcp_tools = ["evil"]');
  for (const [p, r] of [[pinned, topLevelProgram], [pinned, commentedHeader], [dottedPinned, dottedTool]]) {
    assert.deepEqual(capabilityLines(r), capabilityLines(p), "the line heuristic alone is blind to it");
  }
  assert.deepEqual(review(entryOf(pinned), pinned, topLevelProgram), {
    automerge: false,
    why: "the release is privileged: it runs or downloads programs; its capability digest is not the approved one: it may ask for more",
  });
  assert.equal(describeManifest(topLevelProgram).tier, "privileged");
  assert.deepEqual(review(entryOf(pinned), pinned, commentedHeader), {
    automerge: false,
    why: "the release is privileged: it runs or downloads programs; its capability digest is not the approved one: it may ask for more",
  });
  assert.equal(describeManifest(dottedTool).tier, "sandboxed");
  assert.deepEqual(review(entryOf(dottedPinned), dottedPinned, dottedTool), {
    automerge: false,
    why: "its capability digest is not the approved one: it may ask for more",
  });
  // And the PR they open says so, and writes the release's own tier and caps.
  const bump = bumpOf(pinned, topLevelProgram);
  assert.equal(bump.tier, "privileged");
  const { body, automerge } = pullRequest([bump]);
  assert.equal(automerge, false);
  assert.match(body, /`notes` 0\.1\.1: the release is privileged: it runs or downloads programs; its capability digest is not the approved one/);
  assert.match(body, /- Tier: sandboxed → \*\*privileged\*\*/);
  assert.match(body, /Runs bash: this plugin can run any command on this host/);
});

test("each auto-merge condition, false in turn, holds the bump for a person", () => {
  const pinned = release("0.1.0");
  const same = release("0.1.1");
  const base = entryOf(pinned);
  assert.deepEqual(review(base, pinned, same), { automerge: true, why: null }, "all conditions hold");
  const cases = [
    ["the lock has it as privileged", { ...base, tier: "privileged" }, pinned, same, /the lock has it as privileged/],
    ["the lock's caps were edited by hand", { ...base, caps: "0".repeat(64) }, pinned, same, /the pinned release doesn't read as the tier and caps the lock records/],
    ["the lock's tier disagrees with the pinned release", { ...base, tier: "sandboxed " }, pinned, same, /the pinned release doesn't read as/],
    ["the pinned release can't be read", base, "not toml [", same, /the pinned release doesn't read as/],
    ["the release is privileged", base, pinned, release("0.1.1").replace('api = "0.2"', 'api = "0.2"\nprograms = [{ name = "pandoc" }]'), /the release is privileged/],
    ["the release can't be read", base, pinned, "when = 1979-05-27\n", /the release is privileged/],
    ["the release's digest changed", base, pinned, release("0.1.1").replace('mcp_tools = ["notes_read"]', 'mcp_tools = ["notes_read", "x"]'), /capability digest is not the approved one/],
    ["the release's digest shrank", base, pinned, release("0.1.1").replace('files = "read"', 'files = "none"'), /capability digest is not the approved one/],
    ["only the capability lines changed", base, pinned, release("0.1.1").replace('files = "read"', "files = 'read'"), /^its capability lines changed$/],
  ];
  for (const [what, entry, p, r, reason] of cases) {
    const got = review(entry, p, r);
    assert.equal(got.automerge, false, what);
    assert.match(got.why, reason, what);
  }
});

test("over every pair of parity-fixture manifests, auto-merge only when both are sandboxed with equal digests", () => {
  const fixture = JSON.parse(readFileSync(new URL("../../web-ui/src/lib/net/fixtures/wire/plugin-caps.json", import.meta.url), "utf8"));
  const manifests = fixture.cases.map((c) => c.frame.manifest);
  let merged = 0;
  for (const p of manifests) {
    const before = describeManifest(p);
    for (const r of manifests) {
      const after = describeManifest(r);
      for (const tier of ["sandboxed", "privileged"]) {
        const { automerge } = review({ id: "demo", tier, caps: before.caps }, p, r);
        if (!automerge) continue;
        merged++;
        assert.equal(tier, "sandboxed");
        assert.equal(before.tier, "sandboxed");
        assert.equal(after.tier, "sandboxed");
        assert.equal(after.caps, before.caps);
      }
    }
    // A sandboxed release against itself is the one bump that merges.
    const self = review({ id: "demo", tier: before.tier, caps: before.caps }, p, p).automerge;
    assert.equal(self, before.tier === "sandboxed", "an unchanged sandboxed release merges by itself");
  }
  assert.ok(merged > 0);
});

test("a bump whose capabilities are unchanged says so in one line, and keeps tier and caps", () => {
  const pinned = release("0.1.0");
  const bump = bumpOf(pinned, release("0.1.1"));
  assert.equal(bump.automerge, true);
  assert.equal(bump.tier, "sandboxed");
  assert.equal(bump.caps, describeManifest(pinned).caps);
  assert.deepEqual([bump.added, bump.removed], [[], []]);
  const { body } = pullRequest([bump]);
  assert.match(body, /- Tier: sandboxed \(unchanged\)\n- Capabilities unchanged: `caps` stays `[0-9a-f]{12}`\.\n/);
  assert.doesNotMatch(body, /Adds:|Drops:/);
  assert.match(body, /Auto-merge \(squash\) is on/);
  assert.match(body, /\[Changes v0\.1\.0\.\.\.v0\.1\.1\]\(https:\/\/github\.com\/o\/notes\/compare\/v0\.1\.0\.\.\.v0\.1\.1\)/);
});

test("a bump that adds a tool waits, carries the new caps, and says what it adds", () => {
  const pinned = release("0.1.0");
  const next = release("0.2.0").replace('mcp_tools = ["notes_read"]', 'mcp_tools = ["notes_read", "notes_write"]');
  const bump = bumpOf(pinned, next);
  assert.equal(bump.automerge, false);
  assert.equal(bump.why, "its capability digest is not the approved one: it may ask for more; its capability lines changed");
  assert.equal(bump.tier, "sandboxed");
  assert.equal(bump.caps, describeManifest(next).caps);
  assert.notEqual(bump.caps, bump.from_caps);
  assert.deepEqual(bump.added.map((l) => l.text), ["Gives agents 1 tool: notes_write"]);
  assert.deepEqual(bump.removed, []);
  const { body, automerge } = pullRequest([bump]);
  assert.equal(automerge, false);
  assert.match(body, /- Tier: sandboxed \(unchanged\)\n- `caps`: `[0-9a-f]{12}` → `[0-9a-f]{12}`\n- Adds:\n  - ` Gives agents 1 tool: notes_write `\n/);
  assert.match(body, /no auto-merge/);
  assert.match(body, /The lock is written with each release's own `tier` and `caps`/);
  assert.match(body, /a green check here is not an approval/);
  assert.doesNotMatch(body, /set its `tier` and `caps`/, "no hand edit is asked for");
});

test("a bump that becomes privileged says the tier changed and what it runs; one that drops a capability says so", () => {
  const pinned = release("0.1.0");
  const next = release("0.2.0", '[[programs]]\nname = "bash"\n\n[[programs]]\nname = "pandoc"\nnetwork = "CTAN mirrors"').replace('files = "read"', 'files = "none"');
  const bump = bumpOf(pinned, next);
  assert.equal(bump.automerge, false);
  assert.equal(bump.from_tier, "sandboxed");
  assert.equal(bump.tier, "privileged");
  assert.deepEqual(
    bump.added.map((l) => [l.text, l.privileged]),
    [
      ["Runs bash: this plugin can run any command on this host", true],
      ["Runs programs on this host: pandoc (with your permissions, under chimaera's time and memory limits)", true],
      ["pandoc uses the network: CTAN mirrors", true],
    ],
  );
  assert.deepEqual(bump.removed.map((l) => l.text), ["Reads files in this workspace"]);
  const { body } = pullRequest([bump]);
  assert.match(body, /- Tier: sandboxed → \*\*privileged\*\*\n/);
  assert.match(body, /  - \*\*privileged\*\* ` Runs bash: this plugin can run any command on this host `\n/);
  assert.match(body, /- Drops:\n  - ` Reads files in this workspace `\n/);
});

test("text from a release can't inject markdown, mentions, links or workflow commands", () => {
  const hostileName = "Evil | [x](https://e.vil) <img src=x> @someone **b** ::set-output name=automerge::true";
  const pinned = release("0.1.0");
  const next = release(
    "0.2.0",
    '[[programs]]\nname = "tool"\nnetwork = "`code` [y](https://e.vil) <script> @team \\n## heading ::warning::x"',
    hostileName,
  );
  const bump = bumpOf(pinned, next);
  assert.equal(bump.name, hostileName);
  const { body, title } = pullRequest([bump]);
  // The name: every punctuation mark escaped, no mention, no autolink.
  assert.ok(!body.includes("[x](https://e.vil)"), "no link forms");
  assert.doesNotMatch(body, /(^|[^\\])<img/m, "no HTML");
  assert.ok(!body.includes("@someone"), "no mention");
  assert.ok(!body.includes("| [x]"), "no extra table cell");
  assert.match(body, /\| Evil \\\| \\\[x\\\]\\\(https\\:\u200b\\\/\\\/e\\\.vil\\\) \\<img src\\=x\\> @\u200bsomeone \\\*\\\*b\\\*\\\* /);
  // A capability string: a literal code span, on its own line.
  assert.match(body, /  - \*\*privileged\*\* `` tool uses the network: `code` \[y\]\(https:\/\/e\.vil\) <script> @team  ## heading ::warning::x ``\n/);
  assert.ok(!body.split("\n").some((line) => line.startsWith("## heading")), "a newline in a manifest value starts no line");
  assert.ok(!body.split("\n").some((line) => line.startsWith("::")), "nor a workflow command");
  // The title is one line; the outputs never carry release text.
  assert.ok(!/[\r\n]/.test(title));
  assert.equal(outputs([bump], []), "bumped=true\nautomerge=false\nproblems=0\n");
  // A refusal's message is escaped before it rides a workflow command.
  assert.equal(commandMessage("a\nb\r%0A::set-output"), "a%0Ab%0D%250A::set-output");
});

test("md and code: release text as plain markdown and as a literal span", () => {
  assert.equal(md("a_b *c* #1"), "a\\_b \\*c\\* \\#1");
  assert.equal(md("two\nlines\u2028x"), "two lines x");
  assert.equal(md("visit www.e.vil"), "visit www\u200b\\.e\\.vil");
  assert.equal(code("plain"), "` plain `");
  assert.equal(code("has `ticks` and ``more``"), "``` has `ticks` and ``more`` ```");
  assert.equal(code("x\ny"), "` x y `");
});

test("a manifest the port can't read for capabilities is refused, not guessed", () => {
  const pinned = release("0.1.0");
  const entry = { id: "notes", tier: "sandboxed", caps: describeManifest(pinned).caps };
  assert.throws(() => capabilityChange(entry, pinned, release("0.2.0", "[[screens]]\nid = \"x\"")), /doesn't know: "screens"/);
  assert.throws(() => capabilityChange(entry, pinned, release("0.2.0", "when = 1979-05-27")), /dates and times/);
  assert.throws(() => capabilityChange(entry, pinned, release("0.2.0").replace('id = "notes"', 'id = "notes"\n"id" = "other"')), /set twice/);
  // A pinned manifest it can't read only costs the comparison.
  const change = capabilityChange(entry, "not toml at all [", release("0.2.0"));
  assert.equal(change.added, null);
  assert.match(change.unreadable, /plugin\.toml line/);
  const { body } = pullRequest([{ ...bumpOf(pinned, release("0.2.0")), ...change, caps: "9".repeat(64) }]);
  assert.match(body, /couldn't be read here/);
  // The lock's values for the pinned release are checked against the reading.
  const drift = capabilityChange({ ...entry, caps: "0".repeat(64) }, pinned, release("0.2.0"));
  assert.match(drift.lock_drift, /caps reads [0-9a-f]{12}/);
});

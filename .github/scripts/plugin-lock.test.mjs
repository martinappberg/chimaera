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

test("tool downloads: every [[tools.artifacts]] with its tool, read strictly", () => {
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
    "url = 'single-quoted, so not read'",
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
    { tool: "tinytex", platform: "macos-aarch64", url: null, sha256: null, size: null },
    { tool: "other", platform: "linux-x86_64", url: "https://example.org/other.zip", sha256: "b".repeat(64), size: null },
  ]);
  assert.deepEqual(toolArtifacts('id = "notes"\n[adds]\nui = ["x"]\n'), []);
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
    ...review(entry.tier, pinned, next),
  };
}

test("auto-merge is only for a sandboxed plugin whose capability lines are unchanged (the safeguard)", () => {
  const pinned = release("0.1.0");
  const reworded = release("0.1.1").replace("Notes for agents.", "Other words.");
  const grown = release("0.1.1", '[[programs]]\nname = "pandoc"');
  // Every case of the rule, spelled out: loosening any of them must fail here.
  assert.deepEqual(review("sandboxed", pinned, reworded), { automerge: true, why: null });
  assert.equal(review("sandboxed", pinned, grown).automerge, false, "new capability lines wait for a person");
  assert.equal(review("privileged", pinned, reworded).automerge, false, "a privileged plugin always waits, even unchanged");
  assert.equal(review("privileged", pinned, grown).automerge, false);
  assert.equal(review("", pinned, reworded).automerge, false, "only the exact word sandboxed merges by itself");
  assert.equal(review("Sandboxed", pinned, reworded).automerge, false);
  // The capability digest never decides on its own: lines that differ only
  // in a way the digest ignores (an inline comment) still wait.
  const commented = pinned.replace('files = "read"', 'files = "read" # still read');
  assert.equal(describeManifest(commented).caps, describeManifest(pinned).caps);
  assert.equal(review("sandboxed", pinned, commented).automerge, false);
  // The pull request follows the bumps: one that waits holds back all.
  const ok = bumpOf(pinned, reworded);
  const waits = { ...bumpOf(pinned, grown), id: "other", to: "0.2.0" };
  assert.equal(pullRequest([ok]).automerge, true);
  assert.equal(pullRequest([ok, waits]).automerge, false);
  assert.equal(outputs([ok], []), "bumped=true\nautomerge=true\nproblems=0\n");
  assert.equal(outputs([ok, waits], []), "bumped=true\nautomerge=false\nproblems=0\n");
  assert.equal(outputs([], ["x"]), "bumped=false\nautomerge=false\nproblems=1\n");
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
  assert.equal(bump.why, "its capability lines changed: it may ask for more");
  assert.equal(bump.tier, "sandboxed");
  assert.equal(bump.caps, describeManifest(next).caps);
  assert.notEqual(bump.caps, bump.from_caps);
  assert.deepEqual(bump.added.map((l) => l.text), ["Gives agents 1 tool: notes_write"]);
  assert.deepEqual(bump.removed, []);
  const { body, automerge } = pullRequest([bump]);
  assert.equal(automerge, false);
  assert.match(body, /- Tier: sandboxed \(unchanged\)\n- `caps`: `[0-9a-f]{12}` → `[0-9a-f]{12}`\n- Adds:\n  - ` Gives agents 1 tool: notes_write `\n/);
  assert.match(body, /no auto-merge/);
  assert.match(body, /already carries each release's `tier` and `caps`/);
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

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { atomsOf, beyond, describeManifest, digestOf, httpsHost, linesOf, parseToml, tierOf } from "./plugin-caps.mjs";

// Written by the Rust test plugins::capabilities::tests::the_lock_scripts_port_is_pinned
// from what `describe_manifest` (`chimaera plugin caps`) gives each manifest.
const FIXTURE = JSON.parse(readFileSync(new URL("../../web-ui/src/lib/net/fixtures/wire/plugin-caps.json", import.meta.url), "utf8"));

test("the fixture is the Rust test's and covers every kind of atom", () => {
  assert.equal(FIXTURE.family, "plugin-caps");
  assert.equal(FIXTURE.source, "crates/chimaera-server/src/plugins/capabilities.rs");
  const kinds = new Set(FIXTURE.cases.flatMap((c) => c.frame.describe.atoms.map((a) => JSON.parse(a)[0])));
  for (const kind of [
    "access",
    "agent-tool",
    "hook-line",
    "knowledge",
    "browser",
    "agent-plugin",
    "setup-prompt",
    "file-kind",
    "program",
    "program-also",
    "network",
    "download",
    "web",
    "web-library",
    "web-host",
  ]) {
    assert.ok(kinds.has(kind), `no fixture manifest has a ${kind} atom`);
  }
  const tiers = new Set(FIXTURE.cases.map((c) => c.frame.describe.tier));
  assert.deepEqual([...tiers].sort(), ["privileged", "sandboxed"]);
});

for (const { name, frame } of FIXTURE.cases) {
  test(`the port reads ${name} as the daemon does`, () => {
    const got = describeManifest(frame.manifest);
    const want = frame.describe;
    assert.equal(got.tier, want.tier, "tier");
    assert.equal(got.caps, want.caps, "digest");
    assert.deepEqual(got.atoms, want.atoms, "atoms, in the daemon's (byte) order");
    assert.deepEqual(got.can, want.can, "the Can lines");
    assert.equal(got.id, want.id);
    assert.equal(got.version, want.version);
  });
}

test("growth in words: what one release can do that another can't", () => {
  const before = atomsOf(parseToml('api = "0.2"\n[provides]\nmcp_tools = ["a"]\n'));
  const after = atomsOf(parseToml('api = "0.2"\n[provides]\nmcp_tools = ["a", "b"]\n[[programs]]\nname = "x"\n'));
  assert.deepEqual(
    linesOf(beyond(after, before)).map((l) => l.text),
    ["Gives agents 1 tool: b", "Runs programs on this host: x (with your permissions, under chimaera's time and memory limits)"],
  );
  assert.deepEqual(beyond(before, after), []);
  assert.equal(tierOf(before), "sandboxed");
  assert.equal(tierOf(after), "privileged");
  assert.notEqual(digestOf(before), digestOf(after));
});

test("an https URL's host, as platform::https_host reads it", () => {
  assert.equal(httpsHost("https://a.example/x"), "a.example");
  assert.equal(httpsHost("https://u@b.example:1?x#y"), "b.example:1");
  assert.equal(httpsHost("http://a.example/x"), null);
  assert.equal(httpsHost("https:///x"), null);
  assert.equal(httpsHost("https://a b/x"), null);
});

test("what this reader doesn't know is refused, never guessed", () => {
  const head = 'id = "d"\nname = "D"\nversion = "0.1.0"\nsummary = "s"\napi = "0.2"\n';
  // A newer kind of capability: a key the manifest's tables don't have here.
  assert.throws(() => describeManifest(`${head}[[screens]]\nid = "x"\n`), /doesn't know: "screens"/);
  assert.throws(() => describeManifest(`${head}[provides]\nsockets = ["x"]\n`), /doesn't know: "sockets"/);
  assert.throws(() => describeManifest(`${head}[[programs]]\nname = "x"\nenv = ["A"]\n`), /doesn't know: "env"/);
  assert.throws(() => describeManifest(`${head}[access]\nfiles = "write"\n`), /not one of/);
  // TOML it can't read.
  assert.throws(() => describeManifest(`${head}when = 1979-05-27\n`), /dates and times are refused/);
  assert.throws(() => describeManifest(`${head}x = "\\q"\n`), /unknown escape/);
  assert.throws(() => describeManifest(`${head}x = "a\nb"\n`), /unterminated/);
  assert.throws(() => describeManifest(`${head}api = "0.3"\n`), /set twice/);
  assert.throws(() => describeManifest(`${head}[access]\n[access]\n`), /defined twice/);
  assert.throws(() => describeManifest(`${head}x = 1 y = 2\n`), /end of the line/);
  assert.throws(() => describeManifest(`${head}[provides]\nmcp_tools = "one"\n`), /not a list of strings/);
  assert.throws(() => describeManifest('id = "d"\nname = "D"\nversion = "0.1.0"\nsummary = "s"\n'), /api is not a string/);
});

test("TOML strings: escapes, multi-line trimming, literal strings", () => {
  const t = parseToml('a = "\\u00e9\\U0001F680\\t\\e\\x41"\nb = """\\\n   one \\\n   two"""\nc = \'\'\'\nraw \\n\'\'\'\nd = """x""""\n');
  assert.equal(t.a, "\u00e9\u{1f680}\t\x1bA");
  assert.equal(t.b, "one two");
  assert.equal(t.c, "raw \\n");
  assert.equal(t.d, 'x"');
});

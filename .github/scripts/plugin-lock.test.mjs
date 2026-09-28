import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { manifestFields, newer, parseLock, parseSums, pullRequest, setLockValues } from "./plugin-lock.mjs";

const LOCK = `# comment
[[plugin]]
id = "agent-notes"
name = "Agent notes"
summary = "Agents leave short notes."
version = "0.1.3"
repo = "martinappberg/chimaera-plugin-agent-notes"
sha256_wasm = "${"a".repeat(64)}"
sha256_toml = "${"b".repeat(64)}"

[[plugin]]
id = "mycelium"
name = "Mycelium"
summary = "Project memory — findings, decisions, learnings."   # the card's line
version = "0.1.2"
repo = "martinappberg/chimaera-plugin-mycelium"
sha256_wasm = "${"c".repeat(64)}"
sha256_toml = "${"d".repeat(64)}"
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
  const [notes, bumped] = parseLock(next);
  assert.equal(notes.version, "0.1.3");
  assert.equal(notes.sha256_wasm, "a".repeat(64));
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
    id, name, to, from: "0.1.0", repo: `o/${id}`, url: "https://x", sha256_wasm: "a", sha256_toml: "b", wasm_bytes: 1,
  });
  const one = pullRequest([bump("mycelium", "Mycelium", "0.1.3")]);
  assert.equal(one.title, "fix: update Mycelium to 0.1.3");
  assert.equal(one.branch, "plugin-lock/mycelium-0.1.3");
  const two = pullRequest([bump("agent-notes", "Agent notes", "0.1.4"), bump("mycelium", "Mycelium", "0.1.3")]);
  assert.equal(two.title, "fix: update first-party plugins (Agent notes 0.1.4, Mycelium 0.1.3)");
  assert.equal(two.branch, "plugin-lock/agent-notes-0.1.4+mycelium-0.1.3");
  assert.match(two.body, /\| Mycelium \(`mycelium`\) \| 0\.1\.0 \| 0\.1\.3 \|/);
});

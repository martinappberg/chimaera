import assert from "node:assert/strict";
import test from "node:test";

import { buildComment, isSigned, parseSignatureStore } from "./cla.mjs";

test("signature identity follows immutable GitHub id across renames", () => {
  const store = parseSignatureStore(
    JSON.stringify({ signedContributors: [{ name: "old-name", id: 42 }] }),
  );
  assert.equal(isSigned({ login: "new-name", id: 42 }, store), true);
  assert.equal(isSigned({ login: "old-name", id: 99 }, store), false);
});

test("legacy records without an id remain compatible", () => {
  const store = { signedContributors: [{ name: "Contributor" }] };
  assert.equal(isSigned({ login: "contributor", id: 42 }, store), true);
});

test("corrupt signature stores fail closed", () => {
  assert.throws(() => parseSignatureStore("{}"), /signedContributors/);
  assert.throws(() => parseSignatureStore("not json"), /JSON/);
});

test("guidance names missing and unlinked contributors", () => {
  const body = buildComment({
    missing: [{ login: "octocat", id: 1 }],
    unlinked: ["Local Author <local@example.test>"],
  });
  assert.match(body, /@octocat/);
  assert.match(body, /I have read the CLA Document/);
  assert.match(body, /Local Author/);
  assert.match(body, /verified email/);
});

test("success comment is concise and carries the update marker", () => {
  assert.equal(
    buildComment({ missing: [], unlinked: [] }),
    "<!-- chimaera-cla -->\nAll contributors have signed the CLA. ✍️ Thank you!",
  );
});


import assert from "node:assert/strict";
import test from "node:test";

import { pinAction } from "./plugin-pins.mjs";

test("a pinned full release is left alone", () => {
  assert.equal(pinAction("0.1.4", false, null), "none");
  assert.equal(pinAction("0.1.4", false, "0.2.0"), "none");
});

test("a pinned pre-release becomes the latest release unless a newer one is", () => {
  assert.equal(pinAction("0.1.4", true, "0.1.3"), "latest");
  assert.equal(pinAction("0.1.4", true, null), "latest");
  assert.equal(pinAction("0.1.4", true, "0.1.4"), "latest");
  assert.equal(pinAction("0.1.4", true, "0.1.10"), "release");
  assert.equal(pinAction("0.1.4", true, "1.0.0"), "release");
});

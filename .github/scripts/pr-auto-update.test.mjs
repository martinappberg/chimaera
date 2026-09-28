import assert from "node:assert/strict";
import test from "node:test";

import { lastMoved, pick as pickAt, requiredState } from "./pr-auto-update.mjs";

const NOW = Date.parse("2026-09-28T12:00:00Z");
const pick = (prs) => pickAt(prs, NOW);

const run = (name, status, conclusion, isRequired = true, startedAt = "2026-09-28T11:40:00Z") => ({
  __typename: "CheckRun",
  name,
  status,
  conclusion,
  isRequired,
  startedAt,
});
const status = (context, state, isRequired = true) => ({ __typename: "StatusContext", context, state, isRequired });

/** A pull request in line (auto-merge on) unless overridden. */
const pr = (number, over = {}) => ({
  number,
  draft: false,
  crossRepository: false,
  autoMergeEnabledAt: `2026-09-28T10:0${number % 10}:00Z`,
  mergeState: "BEHIND",
  headSha: `sha${number}`,
  required: "passing",
  movedAt: "2026-09-28T11:50:00Z",
  ...over,
});

test("only required checks count, and a failure outranks a running check", () => {
  assert.equal(requiredState([]), "none");
  assert.equal(requiredState([run("bundle (macos-latest)", "IN_PROGRESS", null, false)]), "none");
  assert.equal(requiredState([run("rust", "COMPLETED", "SUCCESS"), status("cla", "SUCCESS")]), "passing");
  assert.equal(requiredState([run("rust", "QUEUED", null), status("cla", "SUCCESS")]), "pending");
  assert.equal(requiredState([run("rust", "COMPLETED", "SUCCESS"), status("cla", "PENDING")]), "pending");
  assert.equal(requiredState([run("ui", "COMPLETED", "FAILURE"), run("rust", "IN_PROGRESS", null)]), "failing");
  assert.equal(requiredState([run("rust", "COMPLETED", "CANCELLED")]), "failing");
  assert.equal(requiredState([status("cla", "ERROR")]), "failing");
  // A non-required failure (a bundle build, a smoke test) never stalls the line.
  assert.equal(requiredState([run("rust", "COMPLETED", "SUCCESS"), run("wsl", "COMPLETED", "FAILURE", false)]), "passing");
});

test("a check's latest run decides, as it does for GitHub's merge", () => {
  // The cla run that failed before the contributor signed, and the one after.
  const signed = [
    run("cla", "COMPLETED", "FAILURE", true, "2026-09-28T09:00:00Z"),
    run("cla", "COMPLETED", "SUCCESS", true, "2026-09-28T10:00:00Z"),
    run("rust", "COMPLETED", "SUCCESS"),
  ];
  assert.equal(requiredState(signed), "passing");
  // A re-run queued after a failure isn't started yet: it is the newest.
  assert.equal(
    requiredState([run("rust", "COMPLETED", "FAILURE", true, "2026-09-28T09:00:00Z"), run("rust", "QUEUED", null, true, null)]),
    "pending",
  );
  // A newer failure still fails.
  assert.equal(
    requiredState([run("ui", "COMPLETED", "SUCCESS", true, "2026-09-28T09:00:00Z"), run("ui", "COMPLETED", "FAILURE", true, "2026-09-28T10:00:00Z")]),
    "failing",
  );
});

test("a pull request moved when its head changed or a required run started", () => {
  assert.equal(lastMoved("2026-09-28T11:00:00Z", [run("rust", "IN_PROGRESS", null, true, "2026-09-28T11:30:00Z")]), "2026-09-28T11:30:00Z");
  assert.equal(lastMoved("2026-09-28T11:00:00Z", [run("bundle", "IN_PROGRESS", null, false, "2026-09-28T11:30:00Z")]), "2026-09-28T11:00:00Z");
  assert.equal(lastMoved(null, []), null);
});

test("the oldest behind pull request in line is next", () => {
  const next = pick([pr(3, { autoMergeEnabledAt: "2026-09-28T11:00:00Z" }), pr(7, { autoMergeEnabledAt: "2026-09-28T09:00:00Z" })]);
  assert.equal(next.number, 7);
  assert.equal(next.headSha, "sha7");
});

test("pull requests out of line are never updated", () => {
  const next = pick([
    pr(1, { autoMergeEnabledAt: null }),
    pr(2, { draft: true }),
    pr(3, { crossRepository: true }),
  ]);
  assert.equal(next.number, null);
  assert.match(next.reason, /no pull request has auto-merge on/);
});

test("one at a time: an up-to-date one still checking holds the line", () => {
  for (const required of ["pending", "none"]) {
    const next = pick([pr(4, { mergeState: "BLOCKED", required }), pr(5)]);
    assert.equal(next.number, null, required);
    assert.equal(next.holding, 4);
    assert.match(next.reason, /#4 is up to date/);
  }
});

test("a pull request that hasn't moved in 45 minutes no longer holds the line", () => {
  // A first-time contributor's CLA status, waiting on a person.
  const waiting = pr(4, { mergeState: "BLOCKED", required: "pending", movedAt: "2026-09-28T11:00:00Z" });
  assert.equal(pick([waiting, pr(5)]).number, 5);
  assert.equal(pick([pr(4, { mergeState: "BLOCKED", required: "none", movedAt: null }), pr(5)]).number, 5);
});

test("one that failed, is merging or conflicts doesn't hold the line", () => {
  for (const held of [
    { mergeState: "BLOCKED", required: "failing" },
    { mergeState: "CLEAN", required: "passing" },
    { mergeState: "UNSTABLE", required: "passing" },
    { mergeState: "DIRTY", required: "passing" },
  ]) {
    assert.equal(pick([pr(4, held), pr(5)]).number, 5, JSON.stringify(held));
  }
});

test("a behind pull request whose required checks failed waits for its fix", () => {
  assert.equal(pick([pr(4, { required: "failing" }), pr(5)]).number, 5);
  assert.equal(pick([pr(4, { required: "failing" })]).number, null);
});

test("unknown merge state is neither behind nor holding the line", () => {
  assert.equal(pick([pr(4, { mergeState: "UNKNOWN" }), pr(5)]).number, 5);
});

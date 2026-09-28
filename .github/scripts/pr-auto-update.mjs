#!/usr/bin/env node
// Keep pull requests with auto-merge moving. `main` requires branches to be
// up to date before merging, and GitHub's auto-merge never updates a branch
// itself, so every merge left the others waiting on a hand-made merge of
// main. This brings the next pull request in line up to date instead.
//
// In line: open against main, auto-merge on, not a draft, from this
// repository. The next one is the pull request in line that fell behind main
// and whose required checks haven't failed, oldest auto-merge first. It gets
// GitHub's update-branch (a merge of main into it, never a rebase), its
// checks rerun, and auto-merge lands it. One at a time: while a pull request
// in line is up to date and its required checks are still running, the rest
// wait — one merge must not set off a CI run for every waiting branch — but
// only for `HOLD_MS` after it last moved: a check waiting on a person (a
// first-time contributor's CLA) or a runner must not stall the line. A
// pull request whose required checks failed is skipped until it is fixed:
// its session (or author) owns that, and the line moves on without it.
//
//   node .github/scripts/pr-auto-update.mjs [--dry-run]
//
// Reads use GH_TOKEN (the workflow's own token). The update uses
// UPDATE_TOKEN (the PLUGIN_LOCK_TOKEN secret): a commit GITHUB_TOKEN makes
// starts no workflow, so the required checks would never report on it.
// Without UPDATE_TOKEN, or with --dry-run, the pick is only reported. EVENT
// is the triggering event: a scheduled run only warns on an error (it is
// the backstop, every 20 minutes; the run after each merge still fails).

import { pathToFileURL } from "node:url";

const API = "https://api.github.com";
const BASE = "main";
/** Merge state is computed lazily after main moves: ask again, this often. */
const UNKNOWN_RETRIES = 6;
const UNKNOWN_WAIT_MS = 10_000;
/** How long an up-to-date pull request still checking holds the line after
 *  it last moved (CI takes 10–15 minutes). */
const HOLD_MS = 45 * 60_000;

/** A required check's conclusions that need a fix, not a wait. */
const RUN_FAILED = new Set(["FAILURE", "ERROR", "CANCELLED", "TIMED_OUT", "ACTION_REQUIRED", "STARTUP_FAILURE", "STALE"]);

/**
 * The latest run of each required check: a re-run (or the cla workflow's
 * comment-triggered run beside its pull-request one) replaces the run before
 * it, as it does for GitHub's merge. A run not started yet is the newest.
 */
function latestRequired(contexts) {
  const latest = new Map();
  for (const c of contexts) {
    if (!c.isRequired) continue;
    const key = c.__typename === "CheckRun" ? `run:${c.name}` : `status:${c.context}`;
    const at = c.__typename === "CheckRun" ? (c.startedAt ?? "\uffff") : "";
    const seen = latest.get(key);
    if (!seen || at >= seen.at) latest.set(key, { c, at });
  }
  return [...latest.values()].map((v) => v.c);
}

/**
 * A commit's required checks in one word: "failing" (one needs a fix),
 * "pending" (one is still running or queued), "passing", or "none" (none
 * has reported yet — a branch just updated). `contexts` are the rollup's
 * CheckRun / StatusContext nodes with `isRequired`.
 */
export function requiredState(contexts) {
  const required = latestRequired(contexts);
  const failed = (c) =>
    c.__typename === "CheckRun" ? RUN_FAILED.has(c.conclusion) : c.state === "FAILURE" || c.state === "ERROR";
  const running = (c) =>
    c.__typename === "CheckRun" ? c.status !== "COMPLETED" : c.state === "PENDING" || c.state === "EXPECTED";
  if (required.some(failed)) return "failing";
  if (required.some(running)) return "pending";
  return required.length > 0 ? "passing" : "none";
}

/** When a pull request last moved: its head commit, or its newest required
 *  run starting (ISO time), whichever is later. */
export function lastMoved(committedDate, contexts) {
  let at = committedDate ?? "";
  for (const c of contexts) {
    if (c.isRequired && c.__typename === "CheckRun" && c.startedAt && c.startedAt > at) at = c.startedAt;
  }
  return at || null;
}

/** The pull requests in line: open against main (the query's filter), auto-merge
 *  on, not a draft, from this repository. */
export function inLine(prs) {
  return prs.filter((p) => p.autoMergeEnabledAt && !p.draft && !p.crossRepository);
}

/**
 * What to update, from the open pull requests: `{number, headSha, holding,
 * reason}`, `number` null when nothing should be (`holding` names the pull
 * request that holds the line, if one does). Each pull request carries
 * `number`, `draft`, `crossRepository`, `autoMergeEnabledAt` (null without
 * auto-merge), `mergeState` (GitHub's mergeStateStatus), `headSha`,
 * `required` (`requiredState`, or null when not asked) and `movedAt`
 * (`lastMoved`, or null).
 */
export function pick(prs, now = Date.now()) {
  const line = inLine(prs);
  if (line.length === 0) return { number: null, reason: "no pull request has auto-merge on" };
  // BLOCKED with nothing failed: its required checks haven't finished (main
  // requires no reviews). It holds the line while it moved recently; CLEAN /
  // UNSTABLE ones are merging now and don't: their merge starts the next run.
  const running = line.find(
    (p) =>
      p.mergeState === "BLOCKED" &&
      (p.required === "pending" || p.required === "none") &&
      p.movedAt !== null &&
      now - Date.parse(p.movedAt) < HOLD_MS,
  );
  if (running) {
    return {
      number: null,
      holding: running.number,
      reason: `#${running.number} is up to date and its checks are running; the rest wait for it`,
    };
  }
  const behind = line
    .filter((p) => p.mergeState === "BEHIND" && p.required !== "failing")
    .sort((a, b) => (a.autoMergeEnabledAt < b.autoMergeEnabledAt ? -1 : a.autoMergeEnabledAt > b.autoMergeEnabledAt ? 1 : a.number - b.number));
  if (behind.length === 0) return { number: null, reason: "no pull request in line is behind main" };
  const next = behind[0];
  return { number: next.number, headSha: next.headSha, reason: `#${next.number} is the oldest in line behind main` };
}

async function graphql(token, query, variables) {
  const res = await fetch(`${API}/graphql`, {
    method: "POST",
    headers: { authorization: `bearer ${token}`, "content-type": "application/json" },
    body: JSON.stringify({ query, variables }),
  });
  const body = await res.json();
  if (!res.ok || body.errors) throw new Error(`GitHub GraphQL: ${res.status} ${JSON.stringify(body.errors ?? body)}`);
  return body.data;
}

const LIST = `query($owner: String!, $name: String!, $base: String!) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: OPEN, baseRefName: $base, first: 100, orderBy: {field: CREATED_AT, direction: ASC}) {
      nodes { number isDraft isCrossRepository mergeStateStatus headRefOid autoMergeRequest { enabledAt } }
    }
  }
}`;

const CHECKS = `query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      commits(last: 1) { nodes { commit { committedDate statusCheckRollup { contexts(first: 100) { nodes {
        __typename
        ... on CheckRun { name status conclusion startedAt isRequired(pullRequestNumber: $number) }
        ... on StatusContext { context state isRequired(pullRequestNumber: $number) }
      } } } } } }
    }
  }
}`;

async function openPullRequests(token, owner, name) {
  for (let attempt = 0; ; attempt++) {
    const data = await graphql(token, LIST, { owner, name, base: BASE });
    const prs = data.repository.pullRequests.nodes.map((p) => ({
      number: p.number,
      draft: p.isDraft,
      crossRepository: p.isCrossRepository,
      mergeState: p.mergeStateStatus,
      headSha: p.headRefOid,
      autoMergeEnabledAt: p.autoMergeRequest?.enabledAt ?? null,
      required: null,
      movedAt: null,
    }));
    const unknown = prs.some((p) => p.autoMergeEnabledAt && p.mergeState === "UNKNOWN");
    if (!unknown || attempt >= UNKNOWN_RETRIES) return prs;
    await new Promise((resolve) => setTimeout(resolve, UNKNOWN_WAIT_MS));
  }
}

async function main() {
  const dryRun = process.argv.includes("--dry-run");
  const [owner, name] = (process.env.REPO ?? "").split("/");
  const readToken = process.env.GH_TOKEN;
  const updateToken = process.env.UPDATE_TOKEN;
  if (!owner || !name || !readToken) throw new Error("REPO (owner/name) and GH_TOKEN are required");

  const prs = await openPullRequests(readToken, owner, name);
  const ask = async (p) => {
    const data = await graphql(readToken, CHECKS, { owner, name, number: p.number });
    const commit = data.repository.pullRequest.commits.nodes[0]?.commit;
    const contexts = commit?.statusCheckRollup?.contexts.nodes ?? [];
    p.required = requiredState(contexts);
    p.movedAt = lastMoved(commit?.committedDate, contexts);
  };
  // The blocked ones first: one still checking holds the line, and then the
  // behind ones needn't be asked at all.
  await Promise.all(inLine(prs).filter((p) => p.mergeState === "BLOCKED").map(ask));
  if (!pick(prs).holding) {
    await Promise.all(inLine(prs).filter((p) => p.mergeState === "BEHIND").map(ask));
  }
  for (const p of prs.filter((p) => p.autoMergeEnabledAt)) {
    const moved = p.movedAt ? `, moved ${p.movedAt}` : "";
    console.log(`#${p.number}: ${p.mergeState}, required checks ${p.required ?? "not asked"}${moved}${p.draft ? ", draft" : ""}`);
  }

  const next = pick(prs);
  console.log(next.reason);
  if (next.number === null) return;
  if (dryRun) {
    console.log(`dry run: #${next.number} would be brought up to date`);
    return;
  }
  if (!updateToken) {
    console.log(
      `::warning title=pr-auto-update::#${next.number} is behind main, but the PLUGIN_LOCK_TOKEN secret isn't set, so it wasn't updated (setup: plugins/AGENTS.md)`,
    );
    return;
  }
  const res = await fetch(`${API}/repos/${owner}/${name}/pulls/${next.number}/update-branch`, {
    method: "PUT",
    headers: {
      authorization: `bearer ${updateToken}`,
      accept: "application/vnd.github+json",
      "content-type": "application/json",
    },
    // The head it was picked at: a push racing this run wins, and the next
    // run looks again.
    body: JSON.stringify({ expected_head_sha: next.headSha }),
  });
  if (res.status === 202) {
    console.log(`brought #${next.number} up to date with main; its checks rerun and auto-merge lands it`);
  } else {
    console.log(`::warning title=pr-auto-update::#${next.number} was not updated: ${res.status} ${await res.text()}`);
  }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  main().catch((err) => {
    if (process.env.EVENT === "schedule") {
      console.log(`::warning title=pr-auto-update::${err.message}`);
      return;
    }
    console.error(err.message);
    process.exit(1);
  });
}

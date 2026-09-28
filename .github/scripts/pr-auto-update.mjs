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
// wait — one merge must not set off a CI run for every waiting branch. A
// pull request whose required checks failed is skipped until it is fixed:
// its session (or author) owns that, and the line moves on without it.
//
//   node .github/scripts/pr-auto-update.mjs [--dry-run]
//
// Reads use GH_TOKEN (the workflow's own token). The update uses
// UPDATE_TOKEN (the PLUGIN_LOCK_TOKEN secret): a commit GITHUB_TOKEN makes
// starts no workflow, so the required checks would never report on it.
// Without UPDATE_TOKEN, or with --dry-run, the pick is only reported.

import { pathToFileURL } from "node:url";

const API = "https://api.github.com";
const BASE = "main";
/** Merge state is computed lazily after main moves: ask again, this often. */
const UNKNOWN_RETRIES = 6;
const UNKNOWN_WAIT_MS = 10_000;

/** A required check's conclusions that need a fix, not a wait. */
const RUN_FAILED = new Set(["FAILURE", "ERROR", "CANCELLED", "TIMED_OUT", "ACTION_REQUIRED", "STARTUP_FAILURE", "STALE"]);

/**
 * A commit's required checks in one word: "failing" (one needs a fix),
 * "pending" (one is still running or queued), "passing", or "none" (none
 * has reported yet — a branch just updated). `contexts` are the rollup's
 * CheckRun / StatusContext nodes with `isRequired`.
 */
export function requiredState(contexts) {
  const required = contexts.filter((c) => c.isRequired);
  const failed = (c) =>
    c.__typename === "CheckRun" ? RUN_FAILED.has(c.conclusion) : c.state === "FAILURE" || c.state === "ERROR";
  const running = (c) =>
    c.__typename === "CheckRun" ? c.status !== "COMPLETED" : c.state === "PENDING" || c.state === "EXPECTED";
  if (required.some(failed)) return "failing";
  if (required.some(running)) return "pending";
  return required.length > 0 ? "passing" : "none";
}

/**
 * What to update, from the open pull requests: `{number, headSha, reason}`,
 * `number` null when nothing should be. Each pull request carries `number`,
 * `draft`, `crossRepository`, `autoMergeEnabledAt` (null without auto-merge),
 * `mergeState` (GitHub's mergeStateStatus), `headSha` and `required`
 * (`requiredState`, or null when not asked).
 */
export function pick(prs) {
  const line = prs.filter((p) => p.autoMergeEnabledAt && !p.draft && !p.crossRepository);
  if (line.length === 0) return { number: null, reason: "no pull request has auto-merge on" };
  // BLOCKED with nothing failed: its required checks haven't finished (main
  // requires no reviews). CLEAN / UNSTABLE ones are merging now and don't
  // hold the line: their merge starts the next run.
  const running = line.find((p) => p.mergeState === "BLOCKED" && (p.required === "pending" || p.required === "none"));
  if (running) {
    return { number: null, reason: `#${running.number} is up to date and its checks are running; the rest wait for it` };
  }
  const behind = line
    .filter((p) => p.mergeState === "BEHIND" && p.required !== "failing")
    .sort((a, b) => a.autoMergeEnabledAt.localeCompare(b.autoMergeEnabledAt) || a.number - b.number);
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
      commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 100) { nodes {
        __typename
        ... on CheckRun { name status conclusion isRequired(pullRequestNumber: $number) }
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
  // Only the pull requests the pick weighs need their checks asked.
  for (const p of prs) {
    if (p.autoMergeEnabledAt && (p.mergeState === "BEHIND" || p.mergeState === "BLOCKED")) {
      const data = await graphql(readToken, CHECKS, { owner, name, number: p.number });
      const contexts = data.repository.pullRequest.commits.nodes[0]?.commit.statusCheckRollup?.contexts.nodes ?? [];
      p.required = requiredState(contexts);
    }
  }
  for (const p of prs.filter((p) => p.autoMergeEnabledAt)) {
    console.log(`#${p.number}: ${p.mergeState}, required checks ${p.required ?? "not asked"}${p.draft ? ", draft" : ""}`);
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
    console.error(err.message);
    process.exit(1);
  });
}

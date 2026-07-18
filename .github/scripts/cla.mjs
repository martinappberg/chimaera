#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const SIGN_PHRASE = "I have read the CLA Document and I hereby sign the CLA";
const RECHECK_PHRASE = "recheck";
const SIGNATURE_BRANCH = "cla-signatures";
const SIGNATURE_PATH = "signatures/version1/cla.json";
const COMMENT_MARKER = "<!-- chimaera-cla -->";
const STATUS_CONTEXT = "cla";
const ALLOWLIST = new Set(
  ["martinappberg", "dependabot[bot]", "renovate[bot]", "github-actions[bot]"].map((name) =>
    name.toLowerCase(),
  ),
);

export function parseSignatureStore(text) {
  const parsed = JSON.parse(text);
  if (!parsed || !Array.isArray(parsed.signedContributors)) {
    throw new Error("CLA signature store must contain a signedContributors array");
  }
  return parsed;
}

export function isSigned(contributor, store) {
  return store.signedContributors.some((signature) => {
    if (Number.isInteger(signature.id)) return signature.id === contributor.id;
    return (
      typeof signature.name === "string" &&
      signature.name.toLowerCase() === contributor.login.toLowerCase()
    );
  });
}

export function buildComment({ missing, unlinked }) {
  if (missing.length === 0 && unlinked.length === 0) {
    return `${COMMENT_MARKER}\nAll contributors have signed the CLA. ✍️ Thank you!`;
  }

  const lines = [
    COMMENT_MARKER,
    "Thank you for contributing to Chimaera. Before this can be merged, every contributor must sign the [Contributor License Agreement](https://github.com/martinappberg/chimaera/blob/main/CLA.md). You keep your copyright; the CLA grants the relicensing rights needed by Chimaera's dual-license model.",
  ];
  if (missing.length > 0) {
    lines.push(
      "",
      `Still awaiting: ${missing.map((person) => `@${person.login}`).join(", ")}`,
      "",
      "After reading the agreement, each listed contributor can sign by replying with exactly:",
      "",
      `\`${SIGN_PHRASE}\``,
    );
  }
  if (unlinked.length > 0) {
    lines.push(
      "",
      "These commit authors are not linked to a GitHub account, so the bot cannot verify their signature:",
      "",
      ...unlinked.map((author) => `- ${author}`),
      "",
      "Please re-author those commits with a GitHub-verified email, then push again.",
    );
  }
  lines.push("", `After changing commits or signing, comment \`${RECHECK_PHRASE}\` if needed.`);
  return lines.join("\n");
}

function requiredEnv(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
}

class GitHubApi {
  constructor(token, repository) {
    this.token = token;
    this.repository = repository;
    this.base = process.env.GITHUB_API_URL ?? "https://api.github.com";
    this.graphqlUrl = process.env.GITHUB_GRAPHQL_URL ?? "https://api.github.com/graphql";
  }

  async request(method, path, body, allow404 = false) {
    const response = await fetch(`${this.base}${path}`, {
      method,
      headers: {
        Accept: "application/vnd.github+json",
        Authorization: `Bearer ${this.token}`,
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "chimaera-cla-gate",
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (allow404 && response.status === 404) return null;
    if (!response.ok) {
      const detail = await response.text();
      const error = new Error(`${method} ${path}: HTTP ${response.status}: ${detail.slice(0, 500)}`);
      error.status = response.status;
      throw error;
    }
    if (response.status === 204) return null;
    return response.json();
  }

  async graphql(query, variables) {
    const response = await fetch(this.graphqlUrl, {
      method: "POST",
      headers: {
        Accept: "application/json",
        Authorization: `Bearer ${this.token}`,
        "Content-Type": "application/json",
        "User-Agent": "chimaera-cla-gate",
      },
      body: JSON.stringify({ query, variables }),
    });
    if (!response.ok) throw new Error(`GraphQL: HTTP ${response.status}: ${await response.text()}`);
    const payload = await response.json();
    if (payload.errors?.length) {
      throw new Error(`GraphQL: ${payload.errors.map((error) => error.message).join("; ")}`);
    }
    return payload.data;
  }
}

const AUTHORS_QUERY = `
  query PullRequestAuthors($owner: String!, $repo: String!, $number: Int!, $cursor: String) {
    repository(owner: $owner, name: $repo) {
      pullRequest(number: $number) {
        commits(first: 100, after: $cursor) {
          nodes {
            commit {
              oid
              authors(first: 100) {
                nodes { name email user { login databaseId } }
                pageInfo { hasNextPage }
              }
            }
          }
          pageInfo { hasNextPage endCursor }
        }
      }
    }
  }
`;

async function collectContributors(api, owner, repo, number, pull) {
  const contributors = new Map();
  const unlinked = new Set();
  contributors.set(pull.user.id, { id: pull.user.id, login: pull.user.login });

  let cursor = null;
  do {
    const data = await api.graphql(AUTHORS_QUERY, { owner, repo, number, cursor });
    const commits = data.repository?.pullRequest?.commits;
    if (!commits) throw new Error(`pull request #${number} was not found`);
    for (const node of commits.nodes) {
      const authors = node.commit.authors;
      if (authors.pageInfo.hasNextPage) {
        unlinked.add(`${node.commit.oid.slice(0, 12)} has more than 100 attributed authors`);
      }
      for (const author of authors.nodes) {
        if (author.user?.databaseId && author.user.login) {
          contributors.set(author.user.databaseId, {
            id: author.user.databaseId,
            login: author.user.login,
          });
        } else {
          const identity = [author.name, author.email].filter(Boolean).join(" <");
          unlinked.add(author.email ? `${identity}>` : identity || "unknown author");
        }
      }
    }
    cursor = commits.pageInfo.hasNextPage ? commits.pageInfo.endCursor : null;
  } while (cursor !== null);

  return {
    contributors: [...contributors.values()].filter(
      (person) => !ALLOWLIST.has(person.login.toLowerCase()),
    ),
    unlinked: [...unlinked].sort((a, b) => a.localeCompare(b)),
  };
}

function contentPath(path) {
  return path
    .split("/")
    .map((part) => encodeURIComponent(part))
    .join("/");
}

async function ensureSignatureBranch(api, defaultBranch) {
  const repoPath = `/repos/${api.repository}`;
  const existing = await api.request(
    "GET",
    `${repoPath}/git/ref/heads/${SIGNATURE_BRANCH}`,
    undefined,
    true,
  );
  if (existing !== null) return;
  const base = await api.request(
    "GET",
    `${repoPath}/git/ref/heads/${encodeURIComponent(defaultBranch)}`,
  );
  try {
    await api.request("POST", `${repoPath}/git/refs`, {
      ref: `refs/heads/${SIGNATURE_BRANCH}`,
      sha: base.object.sha,
    });
  } catch (error) {
    if (error.status !== 422) throw error; // another signer may have won the race
  }
}

async function readSignatures(api) {
  const result = await api.request(
    "GET",
    `/repos/${api.repository}/contents/${contentPath(SIGNATURE_PATH)}?ref=${SIGNATURE_BRANCH}`,
    undefined,
    true,
  );
  if (result === null) return { store: { signedContributors: [] }, sha: null };
  if (result.type !== "file" || typeof result.content !== "string") {
    throw new Error(`${SIGNATURE_PATH} is not a file`);
  }
  const text = Buffer.from(result.content.replaceAll("\n", ""), "base64").toString("utf8");
  return { store: parseSignatureStore(text), sha: result.sha };
}

async function recordSignature(api, event, defaultBranch) {
  const contributor = event.comment.user;
  for (let attempt = 1; attempt <= 4; attempt += 1) {
    await ensureSignatureBranch(api, defaultBranch);
    const { store, sha } = await readSignatures(api);
    if (isSigned({ id: contributor.id, login: contributor.login }, store)) return store;
    store.signedContributors.push({
      name: contributor.login,
      id: contributor.id,
      comment_id: event.comment.id,
      created_at: event.comment.created_at,
      repoId: event.repository.id,
      pullRequestNo: event.issue.number,
    });
    store.signedContributors.sort((a, b) => String(a.created_at ?? "").localeCompare(String(b.created_at ?? "")));
    const body = {
      message: `chore(legal): record ${contributor.login}'s CLA signature`,
      content: Buffer.from(`${JSON.stringify(store, null, 2)}\n`).toString("base64"),
      branch: SIGNATURE_BRANCH,
      ...(sha === null ? {} : { sha }),
    };
    try {
      await api.request(
        "PUT",
        `/repos/${api.repository}/contents/${contentPath(SIGNATURE_PATH)}`,
        body,
      );
      return store;
    } catch (error) {
      if (![409, 422].includes(error.status) || attempt === 4) throw error;
    }
  }
  throw new Error("could not update the CLA signature store");
}

async function upsertComment(api, number, body) {
  let page = 1;
  let found = null;
  while (found === null) {
    const comments = await api.request(
      "GET",
      `/repos/${api.repository}/issues/${number}/comments?per_page=100&page=${page}`,
    );
    found =
      comments.find(
        (comment) =>
          comment.user?.login === "github-actions[bot]" && comment.body?.includes(COMMENT_MARKER),
      ) ?? null;
    if (found !== null || comments.length < 100) break;
    page += 1;
  }
  if (found === null) {
    return api.request("POST", `/repos/${api.repository}/issues/${number}/comments`, { body });
  }
  if (found.body !== body) {
    return api.request("PATCH", `/repos/${api.repository}/issues/comments/${found.id}`, { body });
  }
  return found;
}

async function setStatus(api, pull, passing, comment) {
  await api.request("POST", `/repos/${api.repository}/statuses/${pull.head.sha}`, {
    state: passing ? "success" : "failure",
    context: STATUS_CONTEXT,
    description: passing ? "All contributors have signed the CLA" : "CLA signatures are required",
    target_url: `${pull.html_url}#issuecomment-${comment.id}`,
  });
}

async function main() {
  const token = requiredEnv("GITHUB_TOKEN");
  const repository = requiredEnv("GITHUB_REPOSITORY");
  const event = JSON.parse(await readFile(requiredEnv("GITHUB_EVENT_PATH"), "utf8"));
  const [owner, repo] = repository.split("/");
  if (!owner || !repo) throw new Error(`invalid GITHUB_REPOSITORY: ${repository}`);
  const api = new GitHubApi(token, repository);

  const number = event.pull_request?.number ?? event.issue?.number;
  if (!Number.isInteger(number)) throw new Error("CLA workflow event does not identify a PR");
  const pull =
    event.pull_request ?? (await api.request("GET", `/repos/${repository}/pulls/${number}`));
  if (pull.state !== "open") return;

  const body = event.comment?.body;
  if (body === SIGN_PHRASE) {
    await recordSignature(api, event, event.repository.default_branch);
  } else if (body !== undefined && body !== RECHECK_PHRASE) {
    return;
  }

  const { contributors, unlinked } = await collectContributors(api, owner, repo, number, pull);
  // Always evaluate against the branch after the signature write completed.
  // The workflow's per-PR concurrency guard makes this a stable snapshot until
  // the status/comment update finishes, including simultaneous sign comments.
  const signatureRead = await readSignatures(api).catch((error) => {
    if (error.status === 404) return { store: { signedContributors: [] }, sha: null };
    throw error;
  });
  const missing = contributors
    .filter((person) => !isSigned(person, signatureRead.store))
    .sort((a, b) => a.login.localeCompare(b.login));
  const passing = missing.length === 0 && unlinked.length === 0;
  const comment = await upsertComment(api, number, buildComment({ missing, unlinked }));
  await setStatus(api, pull, passing, comment);
  if (!passing) process.exitCode = 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.stack : error);
    process.exitCode = 1;
  });
}

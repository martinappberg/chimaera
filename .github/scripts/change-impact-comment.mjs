#!/usr/bin/env node
// Posts scripts/change-impact.mjs's markdown as one PR comment, updated in place
// on every push. Best-effort by design: the job's own exit status is the
// verdict, so a token that cannot comment (fork PRs get a read-only one) is
// logged and ignored rather than failing the run.
import { readFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

export const COMMENT_MARKER = '<!-- chimaera-change-impact -->';
const BOT = 'github-actions[bot]';

// `head` (the PR head sha) is appended so a reader can tell which push the
// comment describes when runs overlap or one was cancelled mid-flight.
export function withMarker(markdown, head) {
  let body = markdown.trim();
  if (!body.startsWith(COMMENT_MARKER)) body = `${COMMENT_MARKER}\n${body}`;
  if (head && /^[0-9a-f]{7,40}$/.test(head)) body += `\n\n<sub>for ${head.slice(0, 12)}</sub>`;
  return `${body}\n`;
}

export function findMarkerComment(comments) {
  return comments.find((comment) => comment.user?.login === BOT && comment.body?.includes(COMMENT_MARKER)) ?? null;
}

export class GitHubApi {
  constructor(token, repository, base = process.env.GITHUB_API_URL ?? 'https://api.github.com') {
    this.token = token;
    this.repository = repository;
    this.base = base;
  }

  async request(method, path, body) {
    const response = await fetch(`${this.base}${path}`, {
      method,
      headers: {
        Accept: 'application/vnd.github+json',
        Authorization: `Bearer ${this.token}`,
        'X-GitHub-Api-Version': '2022-11-28',
        'User-Agent': 'chimaera-change-impact',
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!response.ok) {
      const detail = await response.text();
      const error = new Error(`${method} ${path}: HTTP ${response.status}: ${detail.slice(0, 300)}`);
      error.status = response.status;
      throw error;
    }
    return response.status === 204 ? null : response.json();
  }
}

export async function upsertComment(api, number, body) {
  let found = null;
  for (let page = 1; found === null; page += 1) {
    const comments = await api.request(
      'GET',
      `/repos/${api.repository}/issues/${number}/comments?per_page=100&page=${page}`,
    );
    found = findMarkerComment(comments);
    if (comments.length < 100) break;
  }
  if (found === null) {
    await api.request('POST', `/repos/${api.repository}/issues/${number}/comments`, { body });
    return 'created';
  }
  if (found.body === body) return 'unchanged';
  await api.request('PATCH', `/repos/${api.repository}/issues/comments/${found.id}`, { body });
  return 'updated';
}

export async function main(argv = process.argv.slice(2)) {
  const file = argv[0];
  if (!file) {
    console.error('usage: change-impact-comment.mjs <report.md>');
    return 2;
  }
  const markdown = await readFile(file, 'utf8').catch(() => '');
  if (!markdown.trim()) {
    console.log('change-impact: no report to post');
    return 0;
  }
  const { GITHUB_TOKEN: token, GITHUB_REPOSITORY: repository, GITHUB_EVENT_PATH: eventPath } = process.env;
  if (!token || !repository || !eventPath) {
    console.log('change-impact: GITHUB_TOKEN, GITHUB_REPOSITORY or GITHUB_EVENT_PATH unset; not commenting');
    return 0;
  }
  const event = JSON.parse(await readFile(eventPath, 'utf8'));
  const number = event.pull_request?.number;
  if (!Number.isInteger(number)) {
    console.log('change-impact: event is not a pull request; not commenting');
    return 0;
  }
  try {
    const outcome = await upsertComment(
      new GitHubApi(token, repository),
      number,
      withMarker(markdown, event.pull_request?.head?.sha),
    );
    console.log(`change-impact: comment ${outcome} on #${number}`);
  } catch (error) {
    if ([401, 403, 404].includes(error.status)) {
      console.log(`change-impact: token cannot comment on #${number} (HTTP ${error.status}); the job result is the verdict`);
    } else {
      console.log(`::warning::change-impact comment not posted: ${error.message}`);
    }
  }
  return 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().then(
    (code) => {
      process.exitCode = code;
    },
    (error) => {
      console.log(`::warning::change-impact comment not posted: ${error.message}`);
    },
  );
}

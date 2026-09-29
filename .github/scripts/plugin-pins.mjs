#!/usr/bin/env node
// After a chimaera release: every plugin release plugins.lock pins is a full
// release, not a pre-release.
//
// A plugin release whose manifest only an unreleased chimaera can read goes
// out as a GitHub pre-release (the plugin repositories' release workflow,
// run by hand): every released daemon's release checker and
// plugin-lock.yml read `releases/latest`, so none of them sees it, while
// this lock installs it by tag. Once a chimaera release carrying the lock is
// out, release.yml runs this: each pinned tag that is still a pre-release
// becomes a full release, and its repository's latest unless a newer
// release already is. A pin that is already a release is left alone.
//
//   node .github/scripts/plugin-pins.mjs [--write] [--lock <file>]
//
// Without --write it only reports. PLUGIN_TOKEN edits the releases: a token
// with Contents: Read and write on every plugin repository in the lock (the
// PLUGIN_LOCK_TOKEN secret, plugins/AGENTS.md). Without it a run only says
// what it would do, with a warning; a release it can't edit fails the run.

import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

import { newer, parseLock } from "./plugin-lock.mjs";

const API = "https://api.github.com";

/** What a pinned release needs: "none" when it is already a full release,
 *  "latest" when it becomes one and its repository's latest, "release" when
 *  it becomes one but `latest` (the repository's latest version, or null)
 *  is newer, so that one stays latest. */
export function pinAction(pinned, prerelease, latest) {
  if (!prerelease) return "none";
  if (latest !== null && newer(latest, pinned)) return "release";
  return "latest";
}

async function api(method, path, token, body) {
  const headers = { accept: "application/vnd.github+json", "user-agent": "chimaera-plugin-pins" };
  if (token !== "") headers.authorization = `Bearer ${token}`;
  const init = { method, headers, signal: AbortSignal.timeout(60_000) };
  if (body !== undefined) {
    headers["content-type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  const res = await fetch(`${API}${path}`, init);
  if (!res.ok) throw Object.assign(new Error(`${method} ${path}: HTTP ${res.status}`), { status: res.status });
  return res.json();
}

async function latestVersion(repo, token) {
  try {
    const release = await api("GET", `/repos/${repo}/releases/latest`, token);
    const tag = typeof release.tag_name === "string" ? release.tag_name : "";
    return /^v[0-9]+\.[0-9]+\.[0-9]+$/.test(tag) ? tag.slice(1) : null;
  } catch (error) {
    if (error.status === 404) return null;
    throw error;
  }
}

async function main() {
  const args = process.argv.slice(2);
  const at = args.indexOf("--lock");
  const lockPath = at === -1 ? "plugins/plugins.lock" : args[at + 1];
  const write = args.includes("--write");
  const token = process.env.PLUGIN_TOKEN ?? "";
  let problems = 0;
  let waiting = 0;
  for (const entry of parseLock(await readFile(lockPath, "utf8"))) {
    const tag = `v${entry.version}`;
    try {
      const release = await api("GET", `/repos/${entry.repo}/releases/tags/${tag}`, token);
      const prerelease = release.prerelease === true;
      const action = pinAction(entry.version, prerelease, prerelease ? await latestVersion(entry.repo, token) : null);
      if (action === "none") {
        console.log(`${entry.id}: ${tag} is a release`);
        continue;
      }
      const what = action === "latest" ? "a release and the latest" : "a release (a newer one stays latest)";
      if (!write || token === "") {
        waiting++;
        console.log(`${entry.id}: ${tag} is a pre-release; would make it ${what}`);
        continue;
      }
      try {
        await api("PATCH", `/repos/${entry.repo}/releases/${release.id}`, token, {
          prerelease: false,
          make_latest: action === "latest" ? "true" : "false",
        });
      } catch (error) {
        // A repository the token doesn't cover answers 404 or 403.
        if (error.status === 403 || error.status === 404) {
          error.message += ` (PLUGIN_LOCK_TOKEN needs Contents: Read and write on ${entry.repo}; plugins/AGENTS.md)`;
        }
        throw error;
      }
      console.log(`${entry.id}: ${tag} is now ${what}`);
    } catch (error) {
      problems++;
      const message = error instanceof Error ? error.message : String(error);
      console.log(`::error title=plugin-pins::${entry.id} ${tag}: ${message}`);
    }
  }
  if (write && token === "" && waiting > 0) {
    console.log(`::warning title=plugin-pins::${waiting} pinned plugin release(s) are still pre-releases, and the PLUGIN_LOCK_TOKEN secret isn't set to publish them (plugins/AGENTS.md)`);
  }
  if (problems > 0) process.exitCode = 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.stack : error);
    process.exitCode = 1;
  });
}

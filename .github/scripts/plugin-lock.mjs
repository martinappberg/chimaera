#!/usr/bin/env node
// Bump plugins/plugins.lock to each first-party plugin's latest release.
//
// For every [[plugin]] in the lock, the plugin repository's latest release
// (tag vX.Y.Z, not a draft or prerelease) is compared with the pinned
// version. A newer one is downloaded and checked before the lock names it:
// SHA256SUMS must list both files with the bytes actually downloaded, and
// the release's plugin.toml must carry the lock's id, the tag's version and
// `[release] github` = the lock's repository. The lock then takes the new
// version, both sha256s, and the manifest's name and summary (the cards'
// words). Every tool download the manifest names (`[[tools.artifacts]]`) is
// fetched and compared with its sha256, so the lock's pin covers them too.
// CI's plugin tests install the same release against the new lock,
// so a bump this script writes still has to pass CI to merge.
//
// What the plugin can do is the maintainers' to approve (`tier` and `caps`,
// docs/design/plugin-platform-plan.md §2): a bump auto-merges only when the entry is
// sandboxed and the release's capability lines (`capabilityLines`: `api`
// and every table but the card's words, the release source and detect) are
// the pinned release's, so its digest is unchanged (`review`). Otherwise the
// PR opens without auto-merge and says why. Either way the lock takes the
// release's own `tier` and `caps`, read from its plugin.toml by
// plugin-caps.mjs (the daemon's reading, held to it by fixtures), and the
// PR body says, per plugin, whether the tier changed and which of the card's
// Can lines the release adds and drops against the pinned one — so a bump
// that waits for a person arrives green, and the person reads and merges.
// CI still checks tier and caps against the release. A plugin.toml that
// can't be read for capabilities is refused like any other failed check.
//
//   node .github/scripts/plugin-lock.mjs [--write] [--lock <file>] [--out <dir>]
//
// Without --write it only reports. --out receives `title`, `branch` and
// `body.md` for the pull request when something was bumped. A release that
// fails a check is skipped with an error (and a non-zero exit) while the
// other plugins still bump. GITHUB_TOKEN, when set, is sent to the GitHub
// API only (rate limits); release downloads are public. Text from a release
// (names, capability strings) reaches the PR body escaped, and never
// `$GITHUB_OUTPUT` or a workflow command unescaped.

import { createHash } from "node:crypto";
import { appendFile, mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

import { beyond, describeManifest, linesOf } from "./plugin-caps.mjs";

const API = "https://api.github.com";
/** The daemon's own caps (plugins::installed): a component, a manifest. */
const WASM_MAX = 16 * 1024 * 1024;
const TOML_MAX = 64 * 1024;
const SUMS_MAX = 4 * 1024;
const LOCK_KEYS = ["id", "name", "summary", "category", "version", "repo", "sha256_wasm", "sha256_toml", "tier", "caps"];
/** Tables of a plugin.toml that say nothing about what it can do. */
const WORDS_ONLY = new Set(["adds", "release", "detect"]);
const VERSION_RE = /^[0-9]+\.[0-9]+\.[0-9]+$/;
const REPO_RE = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;

/**
 * The lock's [[plugin]] tables, strictly (the grammar scripts/build-plugins.sh
 * reads): only `[[plugin]]` headers, `key = "value"` lines, comments and blank
 * lines. Each entry keeps the line of every key so a bump rewrites values in
 * place, comments and order untouched.
 */
export function parseLock(text) {
  const entries = [];
  let current = null;
  text.split("\n").forEach((line, i) => {
    if (/^\s*(#.*)?$/.test(line)) return;
    if (/^\s*\[\[plugin\]\]\s*(#.*)?$/.test(line)) {
      current = { start: i, keys: {} };
      entries.push(current);
      return;
    }
    const m = /^\s*([a-z0-9_]+)\s*=\s*"([^"]*)"\s*(#.*)?$/.exec(line);
    if (current !== null && m !== null) {
      if (!LOCK_KEYS.includes(m[1])) throw new Error(`plugins.lock line ${i + 1}: unknown key ${m[1]}`);
      if (m[1] in current.keys) throw new Error(`plugins.lock line ${i + 1}: ${m[1]} is set twice`);
      current.keys[m[1]] = { value: m[2], line: i };
      return;
    }
    throw new Error(`plugins.lock line ${i + 1}: not a [[plugin]] header or a key = "value" line`);
  });
  return entries.map(({ start, keys }) => {
    for (const key of LOCK_KEYS) {
      if (!(key in keys)) throw new Error(`plugins.lock: the [[plugin]] at line ${start + 1} has no ${key}`);
    }
    const entry = Object.fromEntries(LOCK_KEYS.map((key) => [key, keys[key].value]));
    if (!VERSION_RE.test(entry.version)) throw new Error(`plugins.lock: ${entry.id} has version "${entry.version}"`);
    if (!REPO_RE.test(entry.repo)) throw new Error(`plugins.lock: ${entry.id} has repo "${entry.repo}"`);
    entry.lines = Object.fromEntries(LOCK_KEYS.map((key) => [key, keys[key].line]));
    return entry;
  });
}

/** `text` with `entry`'s keys set to `values` on their own lines (the line
 *  count never changes, so every other entry's line numbers stay valid). */
export function setLockValues(text, entry, values) {
  const lines = text.split("\n");
  for (const [key, value] of Object.entries(values)) {
    if (!(key in entry.lines)) throw new Error(`plugins.lock has no ${key} key`);
    if (/["\\\r\n]/.test(value)) {
      throw new Error(`${entry.id}: ${key} holds a character plugins.lock can't store: ${JSON.stringify(value)}`);
    }
    const at = entry.lines[key];
    // The first quoted string on a `key = "value"` line is the value.
    lines[at] = lines[at].replace(/"[^"]*"/, () => `"${value}"`);
  }
  return lines.join("\n");
}

/**
 * What the lock takes from a release's plugin.toml: the top-level `id`,
 * `name`, `summary`, `version` (one-line basic strings, before the first
 * table) and `[release] github` — the fields scripts/build-plugins.sh and the
 * daemon compare with the lock. A value that isn't a plain one-line string
 * reads as absent, so the release is refused rather than misread.
 */
export function manifestFields(text) {
  const top = {};
  const release = {};
  let table = null;
  for (const line of text.split("\n")) {
    const header = /^\s*\[([^\]]*)\]\s*(#.*)?$/.exec(line);
    if (header !== null) {
      table = header[1].trim();
      continue;
    }
    const m = /^\s*([A-Za-z0-9_-]+)\s*=\s*"([^"\\]*)"\s*(#.*)?$/.exec(line);
    if (m === null) continue;
    const into = table === null ? top : table === "release" ? release : null;
    if (into !== null && !(m[1] in into)) into[m[1]] = m[2];
  }
  return { id: top.id, name: top.name, summary: top.summary, version: top.version, github: release.github };
}

/**
 * The lines of a plugin.toml that decide what the plugin can do: the
 * top-level `api`, and every line of every table except the card's words
 * (`[adds]`), its release source and its detect paths. Comment and blank
 * lines are dropped, the rest trimmed. Equal lines mean an equal capability
 * digest; any difference (an inline comment too) is treated as asking for
 * more, which only costs a person's review.
 */
export function capabilityLines(text) {
  const out = [];
  let table = null;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (line === "" || line.startsWith("#")) continue;
    const header = /^\[\[?\s*([^\]]*?)\s*\]\]?$/.exec(line);
    if (header !== null) {
      table = header[1];
      if (!WORDS_ONLY.has(table.split(".")[0])) out.push(line);
      continue;
    }
    if (table === null) {
      if (/^api\s*=/.test(line)) out.push(line);
      continue;
    }
    if (!WORDS_ONLY.has(table.split(".")[0])) out.push(line);
  }
  return out;
}

/**
 * A plugin.toml's tool downloads (`[[tools.artifacts]]`,
 * docs/design/plugin-platform-plan.md §8): `{tool, platform, url, sha256, size}`
 * for each, read strictly (`key = "string"` and `size = <integer>`, `_`
 * allowed). The lock pins the manifest, the manifest pins every artifact:
 * a bump downloads each one and compares, so the chain holds only if
 * these are read right — an artifact missing its url or sha256 reads as
 * such and is refused.
 */
export function toolArtifacts(text) {
  const out = [];
  let tool = null;
  let current = null;
  let table = null;
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (line === "" || line.startsWith("#")) continue;
    const header = /^(\[\[?)\s*([^\]]*?)\s*\]\]?$/.exec(line);
    if (header !== null) {
      table = header[2];
      if (header[1] === "[[" && table === "tools") {
        tool = { id: null };
        current = null;
      } else if (header[1] === "[[" && table === "tools.artifacts") {
        current = { tool: tool?.id ?? null, platform: null, url: null, sha256: null, size: null };
        out.push(current);
      }
      continue;
    }
    const str = /^([A-Za-z0-9_-]+)\s*=\s*"([^"\\]*)"\s*(#.*)?$/.exec(line);
    const int = /^([A-Za-z0-9_-]+)\s*=\s*([0-9][0-9_]*)\s*(#.*)?$/.exec(line);
    if (table === "tools" && tool !== null && str?.[1] === "id") tool.id = str[2];
    if (table !== "tools.artifacts" || current === null) continue;
    if (str !== null && ["platform", "url", "sha256"].includes(str[1])) current[str[1]] = str[2];
    if (int !== null && int[1] === "size") current.size = Number(int[2].replaceAll("_", ""));
  }
  return out;
}

/** The daemon's cap on one tool download (plugins::platform::DOWNLOAD_MAX). */
const DOWNLOAD_MAX = 2 * 1024 * 1024 * 1024;

/** Download `url` (≤ `max` bytes) and answer its sha256, streamed: a tool
 *  can be hundreds of megabytes and never sits in memory. */
async function sha256Of(url, max) {
  const res = await fetch(url, {
    headers: { "user-agent": "chimaera-plugin-lock" },
    redirect: "follow",
    signal: AbortSignal.timeout(1_800_000),
  });
  if (!res.ok || res.body === null) throw new Error(`${url}: HTTP ${res.status}`);
  const hash = createHash("sha256");
  let bytes = 0;
  for await (const chunk of res.body) {
    bytes += chunk.length;
    if (bytes > max) throw new Error(`${url}: over ${max} bytes`);
    hash.update(chunk);
  }
  return { sha256: hash.digest("hex"), bytes };
}

/** SHA256SUMS → file name → lowercase sha256 (`*name` and `./name` too). */
export function parseSums(text) {
  const sums = new Map();
  for (const line of text.split("\n")) {
    const m = /^([0-9a-fA-F]{64})\s+\*?(?:\.\/)?(\S+)$/.exec(line.trim());
    if (m !== null && !sums.has(m[2])) sums.set(m[2], m[1].toLowerCase());
  }
  return sums;
}

/** a > b for MAJOR.MINOR.PATCH strings. */
export function newer(a, b) {
  const pa = a.split(".").map(Number);
  const pb = b.split(".").map(Number);
  for (let i = 0; i < 3; i++) if (pa[i] !== pb[i]) return pa[i] > pb[i];
  return false;
}

/** Rust's `char::is_control` (Unicode Cc), and the two line separators. */
const CONTROL = /[\u0000-\u001f\u007f-\u009f\u2028\u2029]/gu;

/** One line of release text, at most `max` characters: control characters
 *  and line breaks become spaces, so nothing can start a line of its own. */
export function oneLine(text, max = 300) {
  const flat = String(text).replace(CONTROL, " ");
  const chars = [...flat];
  return chars.length > max ? `${chars.slice(0, max - 1).join("")}…` : flat;
}

/** Release text as plain markdown: one line, every ASCII punctuation mark
 *  backslash-escaped (no link, emphasis, HTML or table cell can form), and a
 *  zero-width space after `@` and in `://` / `www.` so no one is mentioned
 *  and nothing is autolinked. */
export function md(text) {
  return oneLine(text)
    .replace(/[!"#$%&'()*+,\-./:;<=>?[\\\]^_`{|}~]/g, (c) => `\\${c}`)
    .replace(/@/g, "@\u200b")
    .replace(/:\\\/\\\//g, ":\u200b\\/\\/")
    .replace(/www\\\./gi, (w) => `${w.slice(0, 3)}\u200b\\.`);
}

/** Release text as an inline code span (literal: no markdown, mention or
 *  link inside), fenced past any backticks it holds. */
export function code(text) {
  const s = oneLine(text);
  const longest = Math.max(0, ...(s.match(/`+/g) ?? []).map((run) => run.length));
  const fence = "`".repeat(longest + 1);
  return `${fence} ${s} ${fence}`;
}

const short = (digest) => `\`${digest.slice(0, 12)}\``;

/** What one bump's section of the body says about what it can do. */
function capabilitySection(b) {
  const out = [`### ${md(b.name)} (\`${b.id}\`) ${b.from} → ${b.to}`, ""];
  out.push(`[Release v${b.to}](${b.url}) · [Changes v${b.from}...v${b.to}](${b.compare})`, "");
  out.push(b.tier === b.from_tier ? `- Tier: ${b.tier} (unchanged)` : `- Tier: ${b.from_tier} → **${b.tier}**`);
  if (b.caps === b.from_caps) {
    out.push(`- Capabilities unchanged: \`caps\` stays ${short(b.caps)}.`);
  } else {
    out.push(`- \`caps\`: ${short(b.from_caps)} → ${short(b.caps)}`);
    if (b.added === null) {
      out.push(`- The pinned release's capabilities couldn't be read here (${code(b.unreadable)}), so this is all the release can do:`);
      out.push(...b.can.map((l) => `  - ${l.privileged ? "**privileged** " : ""}${code(l.text)}`));
    } else {
      const say = (label, lines) =>
        lines.length === 0 ? [] : [`- ${label}:`, ...lines.map((l) => `  - ${l.privileged ? "**privileged** " : ""}${code(l.text)}`)];
      out.push(...say("Adds", b.added), ...say("Drops", b.removed));
      if (b.added.length === 0 && b.removed.length === 0) out.push("- The Can lines read the same; an atom's detail changed (a download's sha256, say).");
    }
  }
  if (b.lock_drift) out.push(`- Note: the lock's tier and caps for v${b.from} aren't what this script reads from that release (${b.lock_drift}); check \`chimaera plugin caps\` on it.`);
  out.push("");
  return out;
}

/** The pull request for a set of bumps: its title (a `fix:` — merging it
 *  requests a patch release, which is how the app gets the new lock), branch
 *  and body. */
export function pullRequest(bumps) {
  const review = bumps.filter((b) => !b.automerge);
  const name = (b) => oneLine(b.name, 80);
  const named = bumps.map((b) => `${name(b)} ${b.to}`);
  const title =
    bumps.length === 1
      ? `fix: update ${name(bumps[0])} to ${bumps[0].to}`
      : `fix: update first-party plugins (${named.join(", ")})`;
  const branch = `plugin-lock/${bumps.map((b) => `${b.id}-${b.to}`).join("+")}`;
  const body = [
    "Automatic bump of `plugins/plugins.lock` to the first-party plugins' latest releases, opened by the `plugin-lock` workflow.",
    "",
    "| Plugin | From | To | Release |",
    "|---|---|---|---|",
    ...bumps.map((b) => `| ${md(b.name)} (\`${b.id}\`) | ${b.from} | ${b.to} | [${b.repo} v${b.to}](${b.url}) |`),
    "",
    "Checked before the lock named them: each release's `SHA256SUMS` lists exactly the downloaded `plugin.wasm` and `plugin.toml`, and its `plugin.toml` names the lock's id, the tag's version and `[release] github` = the repository.",
    "",
    ...bumps.map(
      (b) =>
        `- \`${b.id}\` ${b.to}: wasm \`${b.sha256_wasm}\` (${b.wasm_bytes} bytes), toml \`${b.sha256_toml}\`` +
        (b.tools_checked > 0 ? `; ${b.tools_checked} tool download(s) fetched and matched their sha256` : ""),
    ),
    "",
    "## What they can do",
    "",
    "`tier` and `caps` in the lock are each release's own, read from its `plugin.toml` as `chimaera plugin caps` reads it; CI checks both against the release. The lines below are the plugin card's **Can** lines, against the pinned release.",
    "",
    ...bumps.flatMap(capabilitySection),
    ...(review.length === 0
      ? [
          "Each release is sandboxed and asks for nothing its pinned release didn't (the same capability lines), so `tier` and `caps` carry over.",
          "",
          "CI installs these releases from GitHub against the new lock (`scripts/build-plugins.sh` and the server's plugin tests). Auto-merge (squash) is on: it lands when the required checks pass, and the `fix:` title requests a patch release, so the next daily release brings the app the new versions.",
        ]
      : [
          "**Needs a maintainer's review — no auto-merge.** What these plugins can do changed, or they run programs (privileged):",
          "",
          ...review.map((b) => `- \`${b.id}\` ${b.to}: ${b.why}`),
          "",
          "The lock already carries each release's `tier` and `caps`, so CI can go green without an edit. Review the release's code (the compare link) and the capability changes above; merge if the plugin should be able to do that. Merging approves those capabilities for every chimaera that installs it, and requests a patch release, which the next daily release ships. To turn the version down, close the PR unmerged.",
        ]),
  ].join("\n");
  return { title, branch, body, automerge: review.length === 0 };
}

/**
 * Whether a bump may merge by itself, and if not why: only a plugin the lock
 * has as sandboxed whose release has its pinned release's capability lines.
 * A privileged plugin, or any change to those lines, waits for a person.
 * This rule is a safeguard: plugin-lock.test.mjs pins every case of it.
 */
export function review(entryTier, pinnedToml, releaseToml) {
  const same = JSON.stringify(capabilityLines(pinnedToml)) === JSON.stringify(capabilityLines(releaseToml));
  const why =
    entryTier !== "sandboxed"
      ? "it runs programs (privileged): every release is reviewed"
      : same
        ? null
        : "its capability lines changed: it may ask for more";
  return { automerge: why === null, why };
}

/**
 * What the release can do against the pinned release, for the lock and the
 * body: its own tier and caps (the lock takes them), the Can lines it adds
 * and drops, and whether the lock's values for the pinned release are what
 * this reads from it. Throws when the release's manifest can't be read for
 * capabilities (the bump is refused); a pinned manifest that can't be read
 * only costs the comparison.
 */
export function capabilityChange(entry, pinnedToml, releaseToml) {
  const next = describeManifest(releaseToml);
  if (next.id !== entry.id) throw new Error(`its plugin.toml reads as id "${next.id}"`);
  let prev = null;
  let unreadable = null;
  try {
    prev = describeManifest(pinnedToml);
  } catch (error) {
    unreadable = error instanceof Error ? error.message : String(error);
  }
  const drift = [];
  if (prev !== null && prev.tier !== entry.tier) drift.push(`tier reads ${prev.tier}`);
  if (prev !== null && prev.caps !== entry.caps) drift.push(`caps reads ${prev.caps.slice(0, 12)}`);
  return {
    tier: next.tier,
    caps: next.caps,
    from_tier: entry.tier,
    from_caps: entry.caps,
    can: next.can,
    added: prev === null ? null : linesOf(beyond(next.atoms, prev.atoms)),
    removed: prev === null ? null : linesOf(beyond(prev.atoms, next.atoms)),
    unreadable,
    lock_drift: drift.length === 0 ? null : drift.join(", "),
  };
}

/** A workflow command's message, escaped as the runner unescapes it, so a
 *  newline in it can never start a command of its own. */
export function commandMessage(text) {
  return String(text).replaceAll("%", "%25").replaceAll("\r", "%0D").replaceAll("\n", "%0A");
}

/** What the run writes to `$GITHUB_OUTPUT`: three fixed keys, a boolean or
 *  a count each — never text from a release. */
export function outputs(bumps, problems) {
  const bumped = bumps.length > 0;
  const automerge = bumped && bumps.every((b) => b.automerge === true);
  return `bumped=${bumped}\nautomerge=${automerge}\nproblems=${Number(problems.length)}\n`;
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

async function get(url, { max, json = false, token = "" }) {
  const headers = { "user-agent": "chimaera-plugin-lock" };
  if (json) headers.accept = "application/vnd.github+json";
  // The token goes to the API only, never to a download host.
  if (token !== "" && url.startsWith(`${API}/`)) headers.authorization = `Bearer ${token}`;
  const res = await fetch(url, { headers, redirect: "follow", signal: AbortSignal.timeout(120_000) });
  if (!res.ok) throw Object.assign(new Error(`${url}: HTTP ${res.status}`), { status: res.status });
  if (Number(res.headers.get("content-length") ?? 0) > max) throw new Error(`${url}: over ${max} bytes`);
  const bytes = Buffer.from(await res.arrayBuffer());
  if (bytes.length > max) throw new Error(`${url}: over ${max} bytes`);
  return json ? JSON.parse(bytes.toString("utf8")) : bytes;
}

/** One lock entry against its repository's latest release: null when the
 *  lock is current, else the checked bump. Throws when the release fails a
 *  check. */
async function check(entry, token) {
  const release = await get(`${API}/repos/${entry.repo}/releases/latest`, { json: true, max: 1 << 20, token });
  const tag = typeof release.tag_name === "string" ? release.tag_name : "";
  const version = tag.startsWith("v") ? tag.slice(1) : "";
  if (!VERSION_RE.test(version)) throw new Error(`${entry.id}: the latest release's tag "${tag}" is not vX.Y.Z`);
  if (!newer(version, entry.version)) {
    console.log(`${entry.id}: ${entry.version} is current (latest release ${tag})`);
    return null;
  }
  const base = `https://github.com/${entry.repo}/releases/download/${tag}`;
  let sums, toml, wasm;
  try {
    sums = parseSums((await get(`${base}/SHA256SUMS`, { max: SUMS_MAX })).toString("utf8"));
    toml = await get(`${base}/plugin.toml`, { max: TOML_MAX });
    wasm = await get(`${base}/plugin.wasm`, { max: WASM_MAX });
  } catch (error) {
    // The release exists while its job is still uploading the files: the
    // next run finds them. Anything else is a real failure.
    if (error?.status !== 404) throw error;
    console.log(`${entry.id}: ${tag} is not fully published yet (${error.message}); next run`);
    return null;
  }
  const shaToml = sha256(toml);
  const shaWasm = sha256(wasm);
  const refuse = (why) => new Error(`${entry.id} ${tag}: ${why}`);
  if (sums.get("plugin.wasm") !== shaWasm) throw refuse("SHA256SUMS does not list the downloaded plugin.wasm");
  if (sums.get("plugin.toml") !== shaToml) throw refuse("SHA256SUMS does not list the downloaded plugin.toml");
  const m = manifestFields(toml.toString("utf8"));
  if (m.id !== entry.id) throw refuse(`plugin.toml names id "${m.id}", the lock "${entry.id}"`);
  if (m.version !== version) throw refuse(`plugin.toml says version "${m.version}", the tag ${version}`);
  if ((m.github ?? "").toLowerCase() !== entry.repo.toLowerCase()) {
    throw refuse(`plugin.toml names [release] github = "${m.github}", the lock ${entry.repo}`);
  }
  if (!m.name || !m.summary) throw refuse("plugin.toml has no one-line name or summary");
  // The daemon's own rule (plugins::validate): words, never control characters.
  for (const [what, text] of [["name", m.name], ["summary", m.summary]]) {
    if (Buffer.byteLength(text) > 200 || /[\u0000-\u001f\u007f-\u009f]/u.test(text)) {
      throw refuse(`plugin.toml's ${what} is not one line of at most 200 bytes without control characters`);
    }
  }
  // What it can do, against the release the lock pins now (its plugin.toml
  // checked against the lock's sha256 first). Read before any tool download:
  // a manifest that can't be read for capabilities is refused cheaply.
  const pinned = await get(`https://github.com/${entry.repo}/releases/download/v${entry.version}/plugin.toml`, { max: TOML_MAX });
  if (sha256(pinned) !== entry.sha256_toml) throw refuse(`the pinned v${entry.version} plugin.toml isn't the one the lock names`);
  let caps;
  try {
    caps = capabilityChange(entry, pinned.toString("utf8"), toml.toString("utf8"));
  } catch (error) {
    throw refuse(`plugin.toml can't be read for capabilities: ${error instanceof Error ? error.message : String(error)}`);
  }
  const { automerge, why } = review(entry.tier, pinned.toString("utf8"), toml.toString("utf8"));
  // Every tool download it names, fetched and compared: the lock pins this
  // manifest, the manifest pins these, so a reviewer's approval of the one
  // covers the others only if they are what it says.
  const artifacts = toolArtifacts(toml.toString("utf8"));
  for (const a of artifacts) {
    const what = `tool ${a.tool ?? "?"} for ${a.platform ?? "?"}`;
    if (a.url === null || !a.url.startsWith("https://")) throw refuse(`${what} has no https url`);
    if (a.sha256 === null || !/^[0-9a-f]{64}$/.test(a.sha256)) throw refuse(`${what} has no sha256`);
    const got = await sha256Of(a.url, Math.min(a.size ?? DOWNLOAD_MAX, DOWNLOAD_MAX));
    if (got.sha256 !== a.sha256) throw refuse(`${what}: ${a.url} is not the file its sha256 names`);
    console.log(`${entry.id} ${tag}: ${what} checked (${got.bytes} bytes)`);
  }
  return {
    id: entry.id,
    name: m.name,
    summary: m.summary,
    repo: entry.repo,
    from: entry.version,
    to: version,
    // Built from the checked repo and tag, never a URL the API sent.
    url: `https://github.com/${entry.repo}/releases/tag/${tag}`,
    compare: `https://github.com/${entry.repo}/compare/v${entry.version}...${tag}`,
    sha256_wasm: shaWasm,
    sha256_toml: shaToml,
    wasm_bytes: wasm.length,
    tools_checked: artifacts.length,
    ...caps,
    automerge,
    why,
  };
}

function option(args, name) {
  const at = args.indexOf(name);
  return at === -1 ? null : (args[at + 1] ?? null);
}

async function main() {
  const args = process.argv.slice(2);
  const lockPath = option(args, "--lock") ?? "plugins/plugins.lock";
  const out = option(args, "--out");
  const token = process.env.GITHUB_TOKEN ?? "";
  const original = await readFile(lockPath, "utf8");
  let text = original;
  const bumps = [];
  const problems = [];
  for (const entry of parseLock(original)) {
    try {
      const bump = await check(entry, token);
      if (bump === null) continue;
      text = setLockValues(text, entry, {
        version: bump.to,
        sha256_wasm: bump.sha256_wasm,
        sha256_toml: bump.sha256_toml,
        name: bump.name,
        summary: bump.summary,
        tier: bump.tier,
        caps: bump.caps,
      });
      bumps.push(bump);
      console.log(`${entry.id}: ${bump.from} → ${bump.to} (${bump.tier}, caps ${bump.caps})`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      problems.push(message);
      console.log(`::error title=plugin-lock::${commandMessage(message)}`);
    }
  }
  if (bumps.length > 0 && args.includes("--write")) await writeFile(lockPath, text);
  if (bumps.length > 0 && out !== null) {
    const pr = pullRequest(bumps);
    await mkdir(out, { recursive: true });
    await writeFile(join(out, "title"), pr.title);
    await writeFile(join(out, "branch"), pr.branch);
    await writeFile(join(out, "body.md"), pr.body);
  }
  if (process.env.GITHUB_OUTPUT) await appendFile(process.env.GITHUB_OUTPUT, outputs(bumps, problems));
  if (bumps.length === 0 && problems.length === 0) console.log("plugins.lock is current");
  if (problems.length > 0) process.exitCode = 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.stack : error);
    process.exitCode = 1;
  });
}

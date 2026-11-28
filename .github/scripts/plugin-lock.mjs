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
// No TOML is read here: the chimaera CLI built from this checkout
// (`chimaera plugin caps <release.toml> --against <pinned.toml> --json`,
// `--chimaera <bin>`) reads both manifests with the daemon's own code and
// answers each one's lock fields, tier, digest, Can lines and tool
// downloads, what the release adds and drops, and whether the two are the
// same but for their words (`bumpFacts` validates the answer strictly). A
// release the daemon refuses is refused here too.
//
// What the plugin can do is the maintainers' to approve (`tier` and `caps`,
// docs/design/plugin-platform-plan.md §2). A bump auto-merges only when
// (`review`): the pinned release reads as the lock records (its tier and
// caps); the manifests are identical except for words; and, for a
// privileged plugin, its plugin.wasm and plugin.toml are covered by one
// attestation from the plugin repository's release workflow
// (RELEASE_WORKFLOW), at the release's tag or on main (MAIN_REF), from the
// commit the tag points to, which is on main (`verifyProvenance`,
// `gh attestation verify`). That proves the files were built by that
// workflow from a commit on main; it does not prove anyone reviewed the
// commit: whoever can merge to a privileged plugin's main and release it
// ships to users without a second look at the lock PR, which is the
// maintainer's chosen trade. A sandboxed plugin needs no attestation: the
// sandbox bounds its code. Otherwise the PR opens without auto-merge and
// says which condition failed. Either way the lock is written with the
// release's own `tier` and `caps`, and the PR body says, per plugin,
// whether the tier changed and which of the card's Can lines the release
// adds and drops against the pinned one. CI then checks only that the lock
// matches the release: that is not an approval, so a green PR that waits
// for review still needs a person to read the code.
//
//   node .github/scripts/plugin-lock.mjs --newer [--lock <file>]
//   node .github/scripts/plugin-lock.mjs --chimaera <bin> [--write] [--lock <file>] [--out <dir>]
//
// --newer only says (`newer=` in $GITHUB_OUTPUT) whether any plugin has a
// newer release, so the workflow builds the CLI just then. Without --write
// it only reports. --out receives `title`, `branch` and `body.md` for the
// pull request when something was bumped. A release that fails a check is
// skipped with an error (and a non-zero exit) while the other plugins still
// bump. GITHUB_TOKEN, when set, is sent to the GitHub API only (rate
// limits); release downloads are public. Text from a release (names,
// capability strings) reaches the PR body escaped, and never
// `$GITHUB_OUTPUT` or a workflow command unescaped.

import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const API = "https://api.github.com";
/** The daemon's own caps (plugins::installed): a component, a manifest. */
const WASM_MAX = 16 * 1024 * 1024;
const TOML_MAX = 64 * 1024;
const SUMS_MAX = 4 * 1024;
const LOCK_KEYS = ["id", "name", "summary", "category", "version", "repo", "sha256_wasm", "sha256_toml", "tier", "caps"];
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
 * The `chimaera` CLI's answer about a bump, validated strictly:
 * `chimaera plugin caps <release.toml> --against <pinned.toml> --json`
 * (crates/chimaera-server/src/plugins/capabilities.rs `describe_bump`). The
 * daemon's own code reads both manifests — no second TOML reader, no port of
 * the capability rules — so the decision can't differ from what the daemon
 * will do. Exact keys and types at every level; anything else throws (the
 * bump is refused).
 */
export function bumpFacts(json) {
  const fail = (why) => {
    throw new Error(`chimaera plugin caps --against: ${why}`);
  };
  const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
  const keys = (v, want, where) => {
    if (!isObject(v)) fail(`${where} is not an object`);
    const got = Object.keys(v).sort();
    const expect = [...want].sort();
    if (JSON.stringify(got) !== JSON.stringify(expect)) fail(`${where} has keys ${got.join(",")}, not ${expect.join(",")}`);
  };
  const str = (v, where) => {
    if (typeof v !== "string") fail(`${where} is not a string`);
    return v;
  };
  const lines = (v, where) => {
    if (!Array.isArray(v)) fail(`${where} is not a list`);
    v.forEach((l, n) => {
      keys(l, ["text", "privileged"], `${where}[${n}]`);
      str(l.text, `${where}[${n}].text`);
      if (typeof l.privileged !== "boolean") fail(`${where}[${n}].privileged is not a boolean`);
    });
    return v;
  };
  const side = (v, where) => {
    keys(v, ["id", "version", "name", "summary", "github", "tier", "caps", "can", "downloads"], where);
    for (const k of ["id", "version", "name", "summary"]) str(v[k], `${where}.${k}`);
    if (v.github !== null) str(v.github, `${where}.github`);
    if (v.tier !== "sandboxed" && v.tier !== "privileged") fail(`${where}.tier is ${JSON.stringify(v.tier)}`);
    if (!/^[0-9a-f]{64}$/.test(str(v.caps, `${where}.caps`))) fail(`${where}.caps is not a digest`);
    lines(v.can, `${where}.can`);
    if (!Array.isArray(v.downloads)) fail(`${where}.downloads is not a list`);
    v.downloads.forEach((d, n) => {
      const at = `${where}.downloads[${n}]`;
      keys(d, ["tool", "platform", "url", "sha256", "size"], at);
      for (const k of ["tool", "platform", "url", "sha256"]) str(d[k], `${at}.${k}`);
      if (!Number.isSafeInteger(d.size) || d.size <= 0) fail(`${at}.size is not a positive integer`);
    });
    return v;
  };
  let v;
  try {
    v = JSON.parse(json);
  } catch {
    fail("not JSON");
  }
  keys(v, ["release", "pinned", "pinned_error", "added", "dropped", "same_behaviour"], "the answer");
  side(v.release, "release");
  if (typeof v.same_behaviour !== "boolean") fail("same_behaviour is not a boolean");
  if (v.pinned === null) {
    str(v.pinned_error, "pinned_error");
    if (v.added !== null || v.dropped !== null || v.same_behaviour !== false) fail("a pinned manifest it couldn't read compares as nothing");
  } else {
    side(v.pinned, "pinned");
    if (v.pinned_error !== null) fail("pinned_error with a pinned manifest");
    lines(v.added, "added");
    lines(v.dropped, "dropped");
  }
  return v;
}

/**
 * Run `chimaera` (`bin`: the binary built from this checkout, never a
 * downloaded release — an older binary could ignore a key the current
 * daemon reads) on the two manifests, from a private temp dir, without a
 * shell. Throws when it fails or its answer isn't what `bumpFacts` accepts.
 */
export async function caps(bin, releaseToml, pinnedToml, run = execFileAsync) {
  if (typeof bin !== "string" || bin === "") throw new Error("no chimaera CLI to read the manifests with (--chimaera)");
  const dir = await mkdtemp(join(tmpdir(), "plugin-lock-caps-"));
  try {
    const release = join(dir, "release.toml");
    const pinned = join(dir, "pinned.toml");
    await writeFile(release, releaseToml);
    await writeFile(pinned, pinnedToml);
    let stdout;
    try {
      ({ stdout } = await run(bin, ["plugin", "caps", release, "--against", pinned, "--json"], {
        env: { PATH: process.env.PATH ?? "", HOME: process.env.HOME ?? "" },
        timeout: 60_000,
        maxBuffer: 4 * 1024 * 1024,
      }));
    } catch (error) {
      const first = oneLine(String(error?.stderr ?? "").trim().split("\n").at(-1) || String(error?.message ?? "failed"), 200);
      throw new Error(`the release's plugin.toml doesn't read as a manifest: ${first}`);
    }
    return bumpFacts(stdout);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
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
  if (b.lock_drift) out.push(`- Note: the lock's tier and caps for v${b.from} aren't what the daemon reads from that release (${b.lock_drift}); check \`chimaera plugin caps\` on it.`);
  if (b.provenance) {
    const p = b.provenance;
    const commit = p.commit ? `commit \`${p.commit}\`` : "the commit the tag points to (unresolved)";
    const refs = (p.refs ?? []).map((r) => `\`${r}\``).join(" or ");
    out.push(
      p.verified
        ? `- Provenance: verified — one attestation covers \`plugin.wasm\` and \`plugin.toml\`, by \`${b.repo}\`'s \`${p.workflow}\`, signed at \`${p.ref}\`, built from ${commit} (the tag's, on main).`
        : `- Provenance: **not verified** (${code(p.why)}); expected one attestation of \`plugin.wasm\` and \`plugin.toml\` by \`${b.repo}\`'s \`${p.workflow}\` at ${refs}, built from ${commit}, which must be on main.`,
    );
  }
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
          "Each pinned release reads as the lock records, and each release's `plugin.toml` is identical to it except for words (read by the daemon's own code), so the `tier` and `caps` written are the approved ones. A privileged one's `plugin.wasm` and `plugin.toml` are also covered by one attestation from the plugin repository's release workflow, at the release's tag or on main, from the commit the tag points to, which is on main. That proves where and from what they were built, not that anyone reviewed that commit: merging a privileged plugin's main and releasing it ships it without a second look here, by the maintainer's choice.",
          "",
          "CI installs these releases from GitHub against the new lock (`scripts/build-plugins.sh` and the server's plugin tests). Auto-merge (squash) is on: it lands when the required checks pass, and the `fix:` title requests a patch release, so the next daily release brings the app the new versions.",
        ]
      : [
          "**Needs a maintainer's review — no auto-merge.** A bump merges by itself only when the pinned release reads as the lock records, the two `plugin.toml`s are identical except for words, and — for a privileged plugin — its `plugin.wasm` and `plugin.toml` are covered by one attestation from the plugin repository's release workflow, at the release's tag or on main, from the commit the tag points to, which is on main. Here that doesn't hold:",
          "",
          ...review.map((b) => `- \`${b.id}\` ${b.to}: ${code(b.why)}`),
          "",
          "The lock is written with each release's own `tier` and `caps`, so CI can go green without an edit. CI checks only that the lock matches the release; a green check here is not an approval. Read the release's code (the compare link) and the capability changes above, and merge only if the plugin should be able to do that: merging approves those capabilities for every chimaera that installs it, and requests a patch release, which the next daily release ships. To turn the version down, close the PR unmerged.",
        ]),
  ].join("\n");
  return { title, branch, body, automerge: review.length === 0 };
}
/**
 * Whether a bump may merge by itself, and if not why. `facts` is what the
 * daemon's own code says about the two manifests (`bumpFacts`). The rule:
 *   1. the pinned release reads as the lock records: the lock's tier is
 *      exactly `sandboxed` or `privileged`, and the pinned release's tier
 *      and caps digest are the lock's, so the approval it records is real;
 *   2. the manifests are identical except for words (`same_behaviour`,
 *      `plugins::same_behaviour` in the daemon: every field of the manifest
 *      type named as words or behaviour, unknown keys refused), so the
 *      release's tier and digest are the pinned one's too — checked again
 *      here because the values come from the same call;
 *   3. for a privileged plugin only: provenance verified (`provenance`, the
 *      result of `verifyProvenance`): its plugin.wasm and plugin.toml are
 *      covered by one attestation from the plugin repository's release
 *      workflow, at the release's tag or on main, from the commit the tag
 *      points to, which is on main. That proves where and from what the
 *      files were built; it does not prove anyone reviewed that commit, so
 *      with this rule whoever can merge to a privileged plugin's main and
 *      release it ships without a second look at the lock PR (the
 *      maintainer's chosen trade). A sandboxed plugin needs no provenance:
 *      the sandbox bounds what its code can do.
 * Anything else waits for a person; `why` names every condition that failed.
 * Because the lock is then written with the release's own tier and caps,
 * CI going green is no approval: this function is the only thing between a
 * release and an unreviewed merge. plugin-lock.test.mjs pins every case.
 */
export function review(entry, facts, provenance = null) {
  const failed = [];
  if (entry.tier !== "sandboxed" && entry.tier !== "privileged") failed.push("the lock's tier is neither sandboxed nor privileged");
  const pinned = facts?.pinned ?? null;
  const next = facts?.release ?? null;
  if (pinned === null || pinned.tier !== entry.tier || pinned.caps !== entry.caps) {
    failed.push("the pinned release doesn't read as the tier and caps the lock records");
  }
  if (facts?.same_behaviour !== true) failed.push("its manifest changes more than its words");
  if (next === null) failed.push("the release can't be read for capabilities");
  else if (next.tier !== entry.tier) failed.push(`its tier changes (${entry.tier} → ${next.tier})`);
  if (next === null || next.caps !== entry.caps) failed.push("its capability digest is not the approved one: it may ask for more");
  if (entry.tier !== "sandboxed" && provenance?.verified !== true) {
    failed.push(`it is privileged and its provenance did not verify: ${provenance?.why ?? "not checked"}`);
  }
  return failed.length === 0 ? { automerge: true, why: null } : { automerge: false, why: failed.join("; ") };
}

/**
 * What the release can do against the pinned release, for the lock and the
 * body, from the daemon's answer (`facts`): its own tier and caps (the lock
 * takes them), the Can lines it adds and drops, and whether the lock's
 * values for the pinned release are what the daemon reads from it.
 */
export function capabilityChange(entry, facts) {
  const next = facts.release;
  const prev = facts.pinned;
  const drift = [];
  if (prev !== null && prev.tier !== entry.tier) drift.push(`tier reads ${prev.tier}`);
  if (prev !== null && prev.caps !== entry.caps) drift.push(`caps reads ${prev.caps.slice(0, 12)}`);
  return {
    tier: next.tier,
    caps: next.caps,
    from_tier: entry.tier,
    from_caps: entry.caps,
    can: next.can,
    added: facts.added,
    removed: facts.dropped,
    unreadable: facts.pinned_error,
    lock_drift: drift.length === 0 ? null : drift.join(", "),
  };
}

/** The release workflow every first-party plugin repository publishes from:
 *  a convention fixed here, never read from the release. */
export const RELEASE_WORKFLOW = ".github/workflows/release.yml";
/** The plugin repositories' default branch, where a release run by hand
 *  (workflow_dispatch) signs from: a constant, never read from the release. */
export const MAIN_REF = "refs/heads/main";

const COMMIT_RE = /^[0-9a-f]{40}$/;

/**
 * The identity a release's plugin.wasm must be attested by: the plugin
 * repository's own release workflow, signed at the release's tag or on main
 * (exactly these two refs), from `commit`, the commit the tag points to.
 * Repo, tag and commit are checked against the lock's, the tag's and git's
 * grammar first.
 */
export function attestationIdentity(repo, tag, commit) {
  if (!REPO_RE.test(repo)) throw new Error(`not a repository: ${JSON.stringify(repo)}`);
  if (!/^v[0-9]+\.[0-9]+\.[0-9]+$/.test(tag)) throw new Error(`not a release tag: ${JSON.stringify(tag)}`);
  if (!COMMIT_RE.test(commit ?? "")) throw new Error(`not a commit: ${JSON.stringify(commit)}`);
  return { repo, workflow: RELEASE_WORKFLOW, refs: [`refs/tags/${tag}`, MAIN_REF], commit };
}

/** The certificate identity (SAN) of `repo`'s release workflow at `ref`. */
export function signerIdentity(repo, ref) {
  return `https://github.com/${repo}/${RELEASE_WORKFLOW}@${ref}`;
}

/**
 * `gh attestation verify`'s arguments (an argv array, run without a shell)
 * for the release's plugin.wasm at `file`, one attempt per accepted ref:
 * attestations of that repository only (`--repo`), whose certificate names
 * exactly its release workflow at `ref` (`--cert-identity`, an exact SAN, so
 * no look-alike path or ref can match), from that ref (`--source-ref`) and
 * from the tagged commit (`--source-digest`, the certificate's source
 * repository digest), not built on a self-hosted runner, SLSA provenance
 * (gh's default predicate).
 */
export function attestationArgs(file, id, ref) {
  if (!id.refs.includes(ref)) throw new Error(`not an accepted ref: ${JSON.stringify(ref)}`);
  return [
    "attestation",
    "verify",
    file,
    "--repo",
    id.repo,
    "--cert-identity",
    signerIdentity(id.repo, ref),
    "--source-ref",
    ref,
    "--source-digest",
    id.commit,
    "--deny-self-hosted-runners",
    "--format",
    "json",
  ];
}

/** The OIDC issuer of GitHub Actions' signing certificates, and the
 *  predicate a build provenance attestation carries. */
const ACTIONS_ISSUER = "https://token.actions.githubusercontent.com";
const SLSA_PROVENANCE = "https://slsa.dev/provenance/v1";

/** Why one attestation from gh's result is not the one we need, or null when
 *  it is: from exactly the repository, signed by exactly its release
 *  workflow at `ref`, by GitHub Actions' issuer, from the tagged commit, on
 *  a GitHub-hosted runner, SLSA provenance, with both files among its
 *  subjects. */
function attestationProblem(r, id, digests, ref) {
  const cert = r?.verificationResult?.signature?.certificate ?? {};
  const statement = r?.verificationResult?.statement ?? {};
  if (cert.sourceRepositoryURI !== `https://github.com/${id.repo}`) return "an attestation from another repository";
  const san = String(cert.subjectAlternativeName ?? "");
  if (san !== signerIdentity(id.repo, ref)) {
    const at = san.lastIndexOf("@");
    if (at < 0 || san.slice(0, at) !== `https://github.com/${id.repo}/${id.workflow}`) return "an attestation signed by another workflow";
    return `an attestation signed at another ref (${oneLine(san.slice(at + 1), 80)})`;
  }
  if (cert.issuer !== ACTIONS_ISSUER) return "an attestation not issued to GitHub Actions";
  if (cert.sourceRepositoryRef !== ref) return `an attestation made at another ref, not ${ref}`;
  if (cert.sourceRepositoryDigest !== id.commit) return "an attestation built from another commit than the tag's";
  if (cert.runnerEnvironment !== "github-hosted") return "an attestation made on a self-hosted runner";
  if (statement.predicateType !== SLSA_PROVENANCE) return "an attestation that is not SLSA build provenance";
  const subjects = Array.isArray(statement.subject) ? statement.subject : [];
  const has = (sha) => subjects.some((s) => s?.digest?.sha256 === sha);
  if (!has(digests.wasm)) return "an attestation over other bytes";
  if (!has(digests.toml)) return "no attestation covers this plugin.toml with this plugin.wasm";
  return null;
}

/**
 * What `gh attestation verify --format json` printed for the attempt at
 * `ref`, checked again here (fail closed, whatever gh's flags did): verified
 * only when ONE attestation, on its own, is from exactly the repository,
 * signed by exactly its release workflow at `ref`, issued to GitHub Actions,
 * from the tagged commit, on a GitHub-hosted runner, SLSA build provenance,
 * and has both the wasm's and the toml's sha256 (`digests`, the bytes the
 * lock pins) among its subjects — so two unrelated attestations, one over
 * each file, never add up to one. `{verified, why}`.
 */
export function provenanceFrom(stdout, id, digests, ref) {
  let results;
  try {
    results = JSON.parse(stdout);
  } catch {
    return { verified: false, why: "gh printed no verification result" };
  }
  if (!id.refs.includes(ref)) return { verified: false, why: `not an accepted ref: ${ref}` };
  if (!Array.isArray(results) || results.length === 0) return { verified: false, why: "no attestation" };
  const problems = results.map((r) => attestationProblem(r, id, digests, ref));
  if (problems.includes(null)) return { verified: true, why: null };
  return { verified: false, why: problems[0] };
}

/** What a failed `gh attestation verify` means, in one line. */
export function provenanceFailure(error) {
  if (error?.code === "ENOENT") return "gh is not installed";
  const text = `${error?.stderr ?? ""} ${error?.message ?? ""}`;
  if (/HTTP 404|no attestations? (were )?found/i.test(text)) return "no attestation";
  if (error?.killed || error?.signal) return "gh attestation verify timed out";
  const first = oneLine(String(error?.stderr ?? "").trim().split("\n")[0] || String(error?.message ?? "failed"), 160);
  return `gh attestation verify failed: ${first}`;
}

/**
 * The commit `tag` points to in `repo`, through the GitHub API (`getJson`
 * fetches one API path): the response must be for exactly `refs/tags/<tag>`,
 * and an annotated tag is dereferenced to its commit. Throws unless it ends
 * at a commit with a 40-hex SHA.
 */
export async function resolveTagCommit(repo, tag, getJson) {
  const found = await getJson(`/repos/${repo}/git/ref/tags/${tag}`);
  if (found?.ref !== `refs/tags/${tag}`) throw new Error(`the API answered for another ref than refs/tags/${tag}`);
  let object = found.object;
  for (let hops = 0; object?.type === "tag" && hops < 4; hops++) {
    if (!COMMIT_RE.test(object.sha ?? "")) break;
    object = (await getJson(`/repos/${repo}/git/tags/${object.sha}`))?.object;
  }
  if (object?.type !== "commit" || !COMMIT_RE.test(object.sha ?? "")) {
    throw new Error(`${tag} does not resolve to a commit`);
  }
  return object.sha;
}

/**
 * Whether `commit` is on `repo`'s main: GitHub's compare of `commit...main`
 * says main is `ahead` of it or `identical`, with `commit` itself as the
 * merge base. A tag pushed to a side commit runs that commit's own copy of
 * the release workflow, so its attestation would otherwise verify. Throws on
 * any other answer, an error or a missing field. Only as strong as who can
 * push to main.
 */
export async function assertOnMain(repo, commit, getJson) {
  if (!REPO_RE.test(repo)) throw new Error(`not a repository: ${JSON.stringify(repo)}`);
  if (!COMMIT_RE.test(commit ?? "")) throw new Error(`not a commit: ${JSON.stringify(commit)}`);
  const branch = MAIN_REF.slice("refs/heads/".length);
  const cmp = await getJson(`/repos/${repo}/compare/${commit}...${branch}`);
  if (cmp?.status !== "ahead" && cmp?.status !== "identical") {
    throw new Error(`the tagged commit is not on ${branch} (compare: ${oneLine(String(cmp?.status ?? "no status"), 40)})`);
  }
  if (cmp?.merge_base_commit?.sha !== commit) throw new Error(`the tagged commit is not on ${branch} (another merge base)`);
}

/** The environment gh runs with: only what it needs to find its config and
 *  reach the API, and the token as GH_TOKEN — never the rest of ours (the
 *  workflow's other tokens). */
export function ghEnv(source, token) {
  const keep = ["PATH", "HOME", "TMPDIR", "LANG", "LC_ALL", "XDG_CONFIG_HOME", "GH_CONFIG_DIR", "HTTPS_PROXY", "https_proxy", "NO_PROXY", "no_proxy"];
  const env = {};
  for (const key of keep) if (typeof source[key] === "string") env[key] = source[key];
  if (token !== "") env.GH_TOKEN = token;
  return env;
}

/**
 * Whether the release's plugin.wasm and plugin.toml (`wasm`, `toml`: the
 * exact bytes whose sha256s the lock takes) are covered by one attestation
 * from `repo`'s release workflow, at the release's tag or on main, from the
 * commit `tag` points to, which is on main. Resolves the tag and checks main
 * itself, then runs gh on the wasm once per accepted ref (no shell; a
 * minimal environment with the token only as GH_TOKEN) and checks the toml's
 * digest in the same attestation. Every failure — an unresolvable tag, a
 * commit off main, gh missing, a network error, no attestation, another
 * identity, commit or file — is `verified: false` with why, never a throw.
 * `{verified, why, workflow, ref, refs, commit}`: `ref` is the ref it was
 * signed at, when it verified.
 */
export async function verifyProvenance(wasm, toml, repo, tag, { token = "", run = execFileAsync, getJson, env = process.env } = {}) {
  const base = { workflow: RELEASE_WORKFLOW, ref: null, refs: [`refs/tags/${tag}`, MAIN_REF], commit: null };
  const api = getJson ?? ((path) => get(`${API}${path}`, { json: true, max: 4 << 20, token }));
  let commit;
  try {
    if (!REPO_RE.test(repo)) throw new Error(`not a repository: ${JSON.stringify(repo)}`);
    if (!/^v[0-9]+\.[0-9]+\.[0-9]+$/.test(tag)) throw new Error(`not a release tag: ${JSON.stringify(tag)}`);
    commit = await resolveTagCommit(repo, tag, api);
  } catch (error) {
    return { ...base, verified: false, why: `the tag's commit is unknown: ${oneLine(error instanceof Error ? error.message : String(error), 160)}` };
  }
  try {
    await assertOnMain(repo, commit, api);
  } catch (error) {
    return { ...base, commit, verified: false, why: oneLine(error instanceof Error ? error.message : String(error), 160) };
  }
  const id = attestationIdentity(repo, tag, commit);
  const where = { workflow: id.workflow, refs: id.refs, commit };
  const digests = { wasm: sha256(wasm), toml: sha256(toml) };
  const dir = await mkdtemp(join(tmpdir(), "plugin-lock-"));
  try {
    const file = join(dir, "plugin.wasm");
    await writeFile(file, wasm);
    const failures = [];
    for (const ref of id.refs) {
      let result;
      try {
        const { stdout } = await run("gh", attestationArgs(file, id, ref), { env: ghEnv(env, token), timeout: 180_000, maxBuffer: 16 * 1024 * 1024 });
        result = provenanceFrom(stdout, id, digests, ref);
      } catch (error) {
        result = { verified: false, why: provenanceFailure(error) };
      }
      if (result.verified) return { ...where, verified: true, why: null, ref };
      failures.push([ref, result.why]);
    }
    const whys = [...new Set(failures.map(([, why]) => why))];
    const why = whys.length === 1 ? whys[0] : failures.map(([ref, w]) => `at ${ref}: ${w}`).join("; ");
    return { ...where, verified: false, why, ref: null };
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
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

/** The entry's repository's latest release when it is newer than the lock's
 *  pin (`{tag, version}`), else null. Throws when its tag isn't vX.Y.Z. */
async function newerRelease(entry, token) {
  const release = await get(`${API}/repos/${entry.repo}/releases/latest`, { json: true, max: 1 << 20, token });
  const tag = typeof release.tag_name === "string" ? release.tag_name : "";
  const version = tag.startsWith("v") ? tag.slice(1) : "";
  if (!VERSION_RE.test(version)) throw new Error(`${entry.id}: the latest release's tag "${tag}" is not vX.Y.Z`);
  if (!newer(version, entry.version)) {
    console.log(`${entry.id}: ${entry.version} is current (latest release ${tag})`);
    return null;
  }
  return { tag, version };
}

/** One lock entry against its repository's latest release: null when the
 *  lock is current, else the checked bump. Throws when the release fails a
 *  check. `bin` is the chimaera CLI built from this checkout, which reads
 *  both manifests. */
async function check(entry, token, bin) {
  const latest = await newerRelease(entry, token);
  if (latest === null) return null;
  const { tag, version } = latest;
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
  // Both manifests read by the daemon's own code: the release's (refused if
  // the daemon refuses it) against the one the lock pins now (its sha256
  // checked against the lock's first).
  const pinned = await get(`https://github.com/${entry.repo}/releases/download/v${entry.version}/plugin.toml`, { max: TOML_MAX });
  if (sha256(pinned) !== entry.sha256_toml) throw refuse(`the pinned v${entry.version} plugin.toml isn't the one the lock names`);
  let facts;
  try {
    facts = await caps(bin, toml, pinned);
  } catch (error) {
    throw refuse(error instanceof Error ? error.message : String(error));
  }
  const m = facts.release;
  if (m.id !== entry.id) throw refuse(`plugin.toml names id ${JSON.stringify(m.id)}, the lock "${entry.id}"`);
  if (m.version !== version) throw refuse(`plugin.toml says version ${JSON.stringify(m.version)}, the tag ${version}`);
  if ((m.github ?? "").toLowerCase() !== entry.repo.toLowerCase()) {
    throw refuse(`plugin.toml names [release] github = ${JSON.stringify(m.github)}, the lock ${entry.repo}`);
  }
  // The daemon refuses a manifest whose name or summary is more than one
  // line of 200 bytes; the lock stores them as plain one-line strings.
  if (m.name === "" || m.summary === "") throw refuse("plugin.toml has no name or summary");
  // A privileged plugin's release merges by itself only if its plugin.wasm
  // and plugin.toml (these bytes, whose sha256s the lock takes) are in one
  // attestation from the plugin repository's release workflow, at this tag
  // or on main, from the commit the tag points to, which is on main.
  const provenance =
    entry.tier === "privileged" || m.tier === "privileged" ? await verifyProvenance(wasm, toml, entry.repo, tag, { token }) : null;
  if (provenance !== null) console.log(`${entry.id} ${tag}: provenance ${provenance.verified ? "verified" : `not verified (${provenance.why})`}`);
  const { automerge, why } = review(entry, facts, provenance);
  // Every tool download it names (as the daemon reads them), fetched and
  // compared: the lock pins this manifest, the manifest pins these, so an
  // approval of the one covers the others only if they are what it says.
  for (const a of m.downloads) {
    const what = `tool ${a.tool} for ${a.platform}`;
    if (!a.url.startsWith("https://")) throw refuse(`${what} has no https url`);
    if (!/^[0-9a-fA-F]{64}$/.test(a.sha256)) throw refuse(`${what} has no sha256`);
    const got = await sha256Of(a.url, Math.min(a.size, DOWNLOAD_MAX));
    if (got.sha256 !== a.sha256.toLowerCase()) throw refuse(`${what}: ${a.url} is not the file its sha256 names`);
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
    tools_checked: m.downloads.length,
    ...capabilityChange(entry, facts),
    provenance,
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
  const bin = option(args, "--chimaera") ?? "";
  const token = process.env.GITHUB_TOKEN ?? "";
  const original = await readFile(lockPath, "utf8");
  // --newer: only whether any plugin has a newer release, so the workflow
  // builds the CLI just when there is something to read.
  if (args.includes("--newer")) {
    let any = false;
    let failed = 0;
    for (const entry of parseLock(original)) {
      try {
        if ((await newerRelease(entry, token)) !== null) any = true;
      } catch (error) {
        failed++;
        console.log(`::error title=plugin-lock::${commandMessage(error instanceof Error ? error.message : String(error))}`);
      }
    }
    console.log(any ? "a newer release exists" : "plugins.lock is current");
    if (process.env.GITHUB_OUTPUT) await appendFile(process.env.GITHUB_OUTPUT, `newer=${any}\n`);
    if (failed > 0) process.exitCode = 1;
    return;
  }
  let text = original;
  const bumps = [];
  const problems = [];
  for (const entry of parseLock(original)) {
    try {
      const bump = await check(entry, token, bin);
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

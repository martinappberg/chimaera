// What a plugin release can do, read from its plugin.toml the way the daemon
// reads it: the capability atoms, the tier, the digest the lock records
// (`caps`) and the card's **Can** lines. A port of
// crates/chimaera-server/src/plugins/capabilities.rs (`Caps::of`, `digest`,
// `tier`, `lines`) and of `describe_manifest`, which `chimaera plugin caps`
// prints, so the plugin-lock workflow can fill a bump's `tier` and `caps`
// without building the daemon.
//
// Held to the Rust code by fixtures: a Rust test (capabilities.rs,
// `the_lock_scripts_port_is_pinned`) writes manifests covering every kind of
// atom, with what `describe_manifest` gives each, to
// web-ui/src/lib/net/fixtures/wire/plugin-caps.json (`cargo test`
// regenerates it, `just check` and CI fail on a stale one), and
// plugin-caps.test.mjs requires this module to give the same tier, digest,
// atoms and lines for every one. A change to the Rust atoms or lines is a
// change here too, in the same PR.
//
// It fails closed: TOML this reader doesn't know (a date, an unknown escape),
// or a key the manifest's capability tables don't have (a newer kind of
// capability), throws rather than guess — the bump is then refused with an
// error, like any other failed check. The daemon's own reading (CI's lock
// tests) still decides.

import { createHash } from "node:crypto";

/** capabilities.rs `DIGEST_VERSION`. */
const DIGEST_VERSION = "chimaera-caps/1";
/** capabilities.rs `PRIVILEGED_KINDS`. */
const PRIVILEGED_KINDS = ["program", "program-also", "network", "download"];
/** platform.rs `SHELLS`. */
const SHELLS = ["sh", "bash", "zsh", "fish", "dash", "ksh", "csh", "tcsh", "python", "python3", "perl", "ruby", "node", "env", "xargs"];
/** The kinds `Caps::lines` has words for. */
const KNOWN_KINDS = [
  "access",
  "agent-tool",
  "hook-line",
  "knowledge",
  "browser",
  "agent-plugin",
  "setup-prompt",
  "file-kind",
  "program",
  "program-also",
  "network",
  "download",
  "web",
  "web-library",
  "web-host",
];

// ---------------------------------------------------------------- TOML ----

const TABLE = Symbol("table");
const ARRAY_OF_TABLES = Symbol("array of tables");

function newTable(kind = "implicit") {
  const t = Object.create(null);
  Object.defineProperty(t, TABLE, { value: { kind } });
  return t;
}

function isTable(v) {
  return v !== null && typeof v === "object" && TABLE in v;
}

/** Rust's `char::is_whitespace` (Unicode White_Space): not JavaScript's
 *  `trim` set, which adds U+FEFF and lacks U+0085. */
const RUST_WHITESPACE = /^[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]$/u;
function isRustWhitespace(ch) {
  return RUST_WHITESPACE.test(ch);
}
function rustTrim(s) {
  const chars = [...s];
  let a = 0;
  let b = chars.length;
  while (a < b && isRustWhitespace(chars[a])) a++;
  while (b > a && isRustWhitespace(chars[b - 1])) b--;
  return chars.slice(a, b).join("");
}

/**
 * A strict reader for the TOML a plugin.toml is written in (TOML 1.1, as the
 * daemon's `toml` crate reads it): tables, arrays of tables, dotted and
 * quoted keys, the four kinds of string, integers, floats, booleans, arrays
 * and inline tables. Dates and times, which no manifest field takes, are
 * refused, and so is anything malformed.
 */
export function parseToml(text) {
  if (text.startsWith("\ufeff")) text = text.slice(1);
  let i = 0;
  const root = newTable("root");
  let current = root;

  const fail = (why) => {
    const line = text.slice(0, i).split("\n").length;
    throw new Error(`plugin.toml line ${line}: ${why}`);
  };
  const peek = (n = 0) => text[i + n];
  const startsWith = (s) => text.startsWith(s, i);
  const skipWs = () => {
    while (peek() === " " || peek() === "\t") i++;
  };
  const skipComment = () => {
    if (peek() !== "#") return;
    while (i < text.length && peek() !== "\n") {
      const c = text.charCodeAt(i);
      if ((c < 0x20 && c !== 0x09 && c !== 0x0d) || c === 0x7f) fail("a control character in a comment");
      i++;
    }
  };
  const newline = () => {
    if (peek() === "\n") {
      i++;
      return true;
    }
    if (peek() === "\r" && peek(1) === "\n") {
      i += 2;
      return true;
    }
    return false;
  };
  const endOfLine = () => {
    skipWs();
    skipComment();
    if (i >= text.length) return;
    if (!newline()) fail("expected the end of the line");
  };
  /** Whitespace, comments and newlines (inside arrays and inline tables). */
  const skipAll = () => {
    for (;;) {
      skipWs();
      skipComment();
      if (!newline()) return;
    }
  };

  const hex = (n) => {
    const digits = text.slice(i, i + n);
    if (!new RegExp(`^[0-9A-Fa-f]{${n}}$`).test(digits)) fail("a bad escape");
    i += n;
    const cp = parseInt(digits, 16);
    if (cp > 0x10ffff || (cp >= 0xd800 && cp <= 0xdfff)) fail("an escape that is not a Unicode scalar value");
    return String.fromCodePoint(cp);
  };
  const escape = () => {
    i++; // the backslash
    const c = peek();
    i++;
    switch (c) {
      case "b":
        return "\b";
      case "t":
        return "\t";
      case "n":
        return "\n";
      case "f":
        return "\f";
      case "r":
        return "\r";
      case "e":
        return "\x1b";
      case '"':
        return '"';
      case "\\":
        return "\\";
      case "x":
        return hex(2);
      case "u":
        return hex(4);
      case "U":
        return hex(8);
      default:
        i--;
        return fail(`an unknown escape \\${c ?? ""}`);
    }
  };
  const checkChar = (multiline) => {
    const c = text.charCodeAt(i);
    if (c === 0x7f || (c < 0x20 && c !== 0x09 && !(multiline && (c === 0x0a || c === 0x0d)))) {
      fail("a control character in a string");
    }
  };
  const basicString = () => {
    if (startsWith('"""')) {
      i += 3;
      newline();
      let out = "";
      for (;;) {
        if (i >= text.length) fail("an unterminated string");
        if (startsWith('"""')) {
          // Up to two more quotes belong to the string.
          let q = 3;
          while (peek(q) === '"' && q < 5) q++;
          out += '"'.repeat(q - 3);
          i += q;
          return out;
        }
        if (peek() === "\\") {
          // A line-ending backslash trims the newline and the whitespace after it.
          let j = i + 1;
          while (text[j] === " " || text[j] === "\t") j++;
          if (text[j] === "\n" || (text[j] === "\r" && text[j + 1] === "\n")) {
            i = j;
            while (peek() === " " || peek() === "\t" || peek() === "\n" || (peek() === "\r" && peek(1) === "\n")) i++;
            continue;
          }
          out += escape();
          continue;
        }
        if (peek() === "\r" && peek(1) !== "\n") fail("a bare carriage return");
        checkChar(true);
        out += text[i++];
      }
    }
    i++;
    let out = "";
    for (;;) {
      if (i >= text.length || peek() === "\n") fail("an unterminated string");
      if (peek() === '"') {
        i++;
        return out;
      }
      if (peek() === "\\") {
        out += escape();
        continue;
      }
      checkChar(false);
      out += text[i++];
    }
  };
  const literalString = () => {
    if (startsWith("'''")) {
      i += 3;
      newline();
      let out = "";
      for (;;) {
        if (i >= text.length) fail("an unterminated string");
        if (startsWith("'''")) {
          let q = 3;
          while (peek(q) === "'" && q < 5) q++;
          out += "'".repeat(q - 3);
          i += q;
          return out;
        }
        if (peek() === "\r" && peek(1) !== "\n") fail("a bare carriage return");
        checkChar(true);
        out += text[i++];
      }
    }
    i++;
    let out = "";
    for (;;) {
      if (i >= text.length || peek() === "\n") fail("an unterminated string");
      if (peek() === "'") {
        i++;
        return out;
      }
      checkChar(false);
      out += text[i++];
    }
  };

  const key = () => {
    const parts = [];
    for (;;) {
      skipWs();
      if (peek() === '"') {
        if (startsWith('"""')) fail("a multi-line string can't be a key");
        parts.push(basicString());
      } else if (peek() === "'") {
        if (startsWith("'''")) fail("a multi-line string can't be a key");
        parts.push(literalString());
      } else {
        const m = /^[A-Za-z0-9_-]+/.exec(text.slice(i, i + 256));
        if (m === null) fail("expected a key");
        parts.push(m[0]);
        i += m[0].length;
      }
      skipWs();
      if (peek() !== ".") return parts;
      i++;
    }
  };

  const number = () => {
    const rest = text.slice(i, i + 128);
    const m =
      /^[+-]?(inf|nan)(?![A-Za-z0-9_])/.exec(rest) ??
      /^0x[0-9A-Fa-f](_?[0-9A-Fa-f])*(?![A-Za-z0-9_.])/.exec(rest) ??
      /^0o[0-7](_?[0-7])*(?![A-Za-z0-9_.])/.exec(rest) ??
      /^0b[01](_?[01])*(?![A-Za-z0-9_.])/.exec(rest) ??
      /^[+-]?(0|[1-9](_?[0-9])*)(\.[0-9](_?[0-9])*)?([eE][+-]?[0-9](_?[0-9])*)?(?![A-Za-z0-9_.:-])/.exec(rest);
    if (m === null) fail("a value this reader doesn't know (dates and times are refused)");
    i += m[0].length;
    const s = m[0].replaceAll("_", "");
    if (/inf|nan/.test(s)) return s.includes("nan") ? NaN : s.startsWith("-") ? -Infinity : Infinity;
    if (/^0[xob]/.test(s)) return Number(s);
    return Number(s);
  };

  const value = () => {
    const c = peek();
    if (c === '"') return basicString();
    if (c === "'") return literalString();
    if (c === "[") return array();
    if (c === "{") return inlineTable();
    if (startsWith("true") && !/[A-Za-z0-9_-]/.test(peek(4) ?? "")) {
      i += 4;
      return true;
    }
    if (startsWith("false") && !/[A-Za-z0-9_-]/.test(peek(5) ?? "")) {
      i += 5;
      return false;
    }
    return number();
  };
  const array = () => {
    i++;
    const out = [];
    for (;;) {
      skipAll();
      if (peek() === "]") {
        i++;
        return out;
      }
      out.push(value());
      skipAll();
      if (peek() === ",") {
        i++;
        continue;
      }
      if (peek() === "]") {
        i++;
        return out;
      }
      fail("expected , or ] in an array");
    }
  };
  const inlineTable = () => {
    i++;
    const t = newTable("inline");
    for (;;) {
      skipAll();
      if (peek() === "}") {
        i++;
        return t;
      }
      keyValue(t);
      skipAll();
      if (peek() === ",") {
        i++;
        continue;
      }
      if (peek() === "}") {
        i++;
        return t;
      }
      fail("expected , or } in an inline table");
    }
  };

  /** `key = value` into `table` (dotted keys make implicit tables). */
  const keyValue = (table) => {
    const parts = key();
    if (peek() !== "=") fail("expected =");
    i++;
    skipWs();
    const v = value();
    let t = table;
    for (const part of parts.slice(0, -1)) {
      if (!(part in t)) {
        const sub = newTable("dotted");
        t[part] = sub;
        t = sub;
      } else if (isTable(t[part]) && t[part][TABLE].kind === "dotted") {
        t = t[part];
      } else {
        fail(`${parts.join(".")} redefines a key`);
      }
    }
    const last = parts.at(-1);
    if (last in t) fail(`${parts.join(".")} is set twice`);
    t[last] = v;
  };

  const header = (arrayOfTables) => {
    i += arrayOfTables ? 2 : 1;
    const parts = key();
    if (arrayOfTables ? !startsWith("]]") : peek() !== "]") fail("an unterminated table header");
    i += arrayOfTables ? 2 : 1;
    let t = root;
    parts.forEach((part, n) => {
      const last = n === parts.length - 1;
      if (last && arrayOfTables) {
        if (!(part in t)) {
          const list = [];
          Object.defineProperty(list, ARRAY_OF_TABLES, { value: true });
          t[part] = list;
        }
        if (!Array.isArray(t[part]) || !t[part][ARRAY_OF_TABLES]) fail(`[[${parts.join(".")}]] is not an array of tables`);
        const entry = newTable("header");
        t[part].push(entry);
        t = entry;
        return;
      }
      if (!(part in t)) {
        t[part] = newTable(last ? "header" : "implicit");
        t = t[part];
        return;
      }
      let next = t[part];
      if (Array.isArray(next) && next[ARRAY_OF_TABLES]) {
        if (last) fail(`[${parts.join(".")}] is an array of tables`);
        next = next.at(-1);
      } else if (!isTable(next) || next[TABLE].kind === "inline" || (next[TABLE].kind === "dotted" && last)) {
        fail(`[${parts.join(".")}] redefines a key`);
      } else if (last) {
        if (next[TABLE].kind !== "implicit") fail(`[${parts.join(".")}] is defined twice`);
        next[TABLE].kind = "header";
      }
      t = next;
    });
    current = t;
  };

  while (i < text.length) {
    skipWs();
    if (i >= text.length) break;
    if (peek() === "#" || peek() === "\n" || peek() === "\r") {
      endOfLine();
      continue;
    }
    if (startsWith("[[")) header(true);
    else if (peek() === "[") header(false);
    else keyValue(current);
    endOfLine();
  }
  return root;
}

// ------------------------------------------------------ the manifest ----

function expectKeys(table, allowed, where) {
  for (const k of Object.keys(table)) {
    if (!allowed.includes(k)) throw new Error(`plugin.toml: ${where} has a key this reader doesn't know: ${JSON.stringify(k)}`);
  }
}
function expectTable(v, where) {
  if (!isTable(v)) throw new Error(`plugin.toml: ${where} is not a table`);
  return v;
}
function optTable(v, where) {
  return v === undefined ? newTable() : expectTable(v, where);
}
function expectString(v, where) {
  if (typeof v !== "string") throw new Error(`plugin.toml: ${where} is not a string`);
  return v;
}
function optString(v, where) {
  return v === undefined ? null : expectString(v, where);
}
function stringList(v, where) {
  if (v === undefined) return [];
  if (!Array.isArray(v) || v.some((s) => typeof s !== "string")) throw new Error(`plugin.toml: ${where} is not a list of strings`);
  return v;
}
function tableList(v, where) {
  if (v === undefined) return [];
  if (!Array.isArray(v) || v.some((t) => !isTable(t))) throw new Error(`plugin.toml: ${where} is not a list of tables`);
  return v;
}

/** `Manifest`'s top-level fields (mod.rs, deny_unknown_fields). */
const MANIFEST_KEYS = [
  "id",
  "name",
  "version",
  "summary",
  "description",
  "homepage",
  "category",
  "api",
  "detect",
  "access",
  "requires",
  "recommends",
  "setup",
  "provides",
  "adds",
  "release",
  "views",
  "files",
  "actions",
  "settings",
  "programs",
  "tools",
  "web",
];

function agentPlugins(table, where) {
  const out = [];
  for (const [agent, req] of Object.entries(optTable(table, where))) {
    const r = expectTable(req, `${where}.${agent}`);
    expectKeys(r, ["id", "marketplace", "source"], `${where}.${agent}`);
    out.push([agent, expectString(r.id, `${where}.${agent}.id`), optString(r.marketplace, `${where}.${agent}.marketplace`) ?? ""]);
  }
  return out;
}

const ACCESS = {
  files: ["none", "read"],
  timeline: ["none", "read", "notes"],
  sessions: ["none", "read"],
};

/** platform.rs `https_host`: the host of an https URL, else null. */
export function httpsHost(url) {
  if (!url.startsWith("https://")) return null;
  const rest = url.slice("https://".length);
  const host = rest.split(/[/?#]/)[0].split("@").at(-1);
  return host !== "" && ![...host].some(isRustWhitespace) ? host : null;
}

/** UTF-8 byte order: a `BTreeSet<String>`'s. */
function byteOrder(a, b) {
  return Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8"));
}

/** `atom(&[..])`: serde_json's compact array, which JSON.stringify writes
 *  byte for byte for an array of strings (the same escapes, lowercase hex). */
function atom(parts) {
  return JSON.stringify(parts);
}

/** `Caps::of`: the sorted atoms a parsed manifest declares (or, at API 0.1,
 *  implies). */
export function atomsOf(m) {
  expectKeys(m, MANIFEST_KEYS, "the top level");
  const api = expectString(m.api, "api");
  const atoms = new Set();
  const add = (...parts) => atoms.add(atom(parts));

  const access = optTable(m.access, "[access]");
  expectKeys(access, Object.keys(ACCESS), "[access]");
  const implied = api === "0.1" ? { files: "read", timeline: "notes", sessions: "read" } : { files: "none", timeline: "none", sessions: "none" };
  const level = {};
  for (const [k, allowed] of Object.entries(ACCESS)) {
    const v = access[k] === undefined ? implied[k] : expectString(access[k], `access.${k}`);
    if (!allowed.includes(v)) throw new Error(`plugin.toml: access.${k} = ${JSON.stringify(v)} is not one of ${allowed.join(", ")}`);
    level[k] = v;
  }
  if (level.files === "read") add("access", "files", "read");
  if (level.timeline !== "none") add("access", "timeline", "read");
  if (level.timeline === "notes") add("access", "timeline", "notes");
  if (level.sessions === "read") add("access", "sessions", "read");

  const provides = optTable(m.provides, "[provides]");
  expectKeys(provides, ["knowledge", "mcp_tools", "views", "events", "browser"], "[provides]");
  for (const tool of stringList(provides.mcp_tools, "provides.mcp_tools")) add("agent-tool", tool);
  if (stringList(provides.events, "provides.events").includes("hook")) add("hook-line");
  if (optString(provides.knowledge, "provides.knowledge") !== null) add("knowledge");
  const browser = optString(provides.browser, "provides.browser");
  if (browser !== null) add("browser", browser);

  if (m.setup !== undefined) {
    expectKeys(expectTable(m.setup, "[setup]"), ["prompt"], "[setup]");
    add("setup-prompt");
  }

  for (const p of tableList(m.programs, "[[programs]]")) {
    expectKeys(p, ["name", "version", "network", "also"], "[[programs]]");
    const name = expectString(p.name, "programs.name");
    add("program", name);
    for (const dir of stringList(p.also, "programs.also")) add("program-also", name, dir);
    const net = optString(p.network, "programs.network");
    if (net !== null) add("network", name, rustTrim(net));
  }

  for (const t of tableList(m.tools, "[[tools]]")) {
    expectKeys(t, ["id", "name", "version", "programs", "home", "artifacts", "setup"], "[[tools]]");
    const id = expectString(t.id, "tools.id");
    const name = expectString(t.name, "tools.name");
    const version = expectString(t.version, "tools.version");
    for (const a of tableList(t.artifacts, "[[tools.artifacts]]")) {
      expectKeys(a, ["platform", "url", "sha256", "size", "unpack", "bin"], "[[tools.artifacts]]");
      const url = expectString(a.url, "tools.artifacts.url");
      add("download", id, name, version, expectString(a.platform, "tools.artifacts.platform"), httpsHost(url) ?? "", expectString(a.sha256, "tools.artifacts.sha256"));
    }
  }

  if (m.web !== undefined) {
    const web = expectTable(m.web, "[web]");
    expectKeys(web, ["pages", "libraries", "network"], "[web]");
    const pages = stringList(web.pages, "web.pages");
    const libraries = stringList(web.libraries, "web.libraries");
    if (pages.length > 0 || libraries.length > 0) add("web");
    for (const name of libraries) add("web-library", name);
    for (const host of stringList(web.network, "web.network")) add("web-host", host);
  }

  for (const kind of tableList(m.files, "[[files]]")) {
    expectKeys(kind, ["match", "view", "label", "debounce_ms"], "[[files]]");
    for (const pattern of stringList(kind.match, "files.match")) add("file-kind", pattern);
  }

  const requires = optTable(m.requires, "[requires]");
  expectKeys(requires, ["agent_plugins", "chimaera", "summary"], "[requires]");
  const recommends = optTable(m.recommends, "[recommends]");
  expectKeys(recommends, ["agent_plugins", "summary"], "[recommends]");
  for (const [agent, id, marketplace] of [
    ...agentPlugins(requires.agent_plugins, "requires.agent_plugins"),
    ...agentPlugins(recommends.agent_plugins, "recommends.agent_plugins"),
  ]) {
    add("agent-plugin", agent, id, marketplace);
  }
  return [...atoms].sort(byteOrder);
}

/** `Caps::digest`. */
export function digestOf(atoms) {
  return createHash("sha256")
    .update([DIGEST_VERSION, ...[...atoms].sort(byteOrder)].join("\n"), "utf8")
    .digest("hex");
}

/** `Caps::tier`. */
export function tierOf(atoms) {
  return atoms.some((a) => PRIVILEGED_KINDS.includes(JSON.parse(a)[0])) ? "privileged" : "sandboxed";
}

/** `Caps::lines`: the card's **Can** list, `{text, privileged}`. */
export function linesOf(atoms) {
  const parsed = [...atoms].sort(byteOrder).map((a) => JSON.parse(a));
  const has = (...parts) => parsed.some((p) => p.length === parts.length && p.every((x, n) => x === parts[n]));
  const ofKind = (k) => parsed.filter((p) => p[0] === k);
  const lines = [];
  const plain = (text) => lines.push({ text, privileged: false });
  const loud = (text) => lines.push({ text, privileged: true });

  if (has("access", "files", "read")) plain("Reads files in this workspace");
  if (has("access", "timeline", "notes")) plain("Reads the Timeline and posts notes to it");
  else if (has("access", "timeline", "read")) plain("Reads the Timeline");
  if (has("access", "sessions", "read")) plain("Sees this workspace's sessions (their names and kinds)");
  const tools = ofKind("agent-tool")
    .filter((p) => p.length > 1)
    .map((p) => p[1]);
  if (tools.length > 0) plain(`Gives agents ${tools.length} tool${tools.length === 1 ? "" : "s"}: ${tools.join(" · ")}`);
  if (has("hook-line")) plain("Adds a short line to what claude sees when a session starts or you send a prompt");
  if (has("knowledge")) plain("Fills the Knowledge view");
  for (const p of ofKind("agent-plugin")) {
    if (p.length !== 4) continue;
    const [, agent, id, marketplace] = p;
    plain(`Offers an agent-side plugin for ${agent}: ${id} from ${marketplace} (installed only on your click; it then runs inside ${agent})`);
  }
  if (has("setup-prompt")) plain("Has a setup prompt it sends to an agent you choose, shown in full first");
  const browsers = ofKind("browser")
    .filter((p) => p.length > 1)
    .map((p) => p[1]);
  for (const program of browsers) {
    loud(
      `Runs ${program} on this host as the browser agents' browser tools drive, where it is switched on and when this host has no Chrome or Chromium of its own`,
    );
  }
  const programs = ofKind("program")
    .filter((p) => p.length > 1)
    .map((p) => p[1])
    .filter((p) => !browsers.includes(p));
  const shells = programs.filter((p) => SHELLS.includes(p));
  for (const shell of shells) loud(`Runs ${shell}: this plugin can run any command on this host`);
  const others = programs.filter((p) => !shells.includes(p));
  if (others.length > 0) {
    loud(`Runs programs on this host: ${others.join(", ")} (with your permissions, under chimaera's time and memory limits)`);
  }
  const also = [];
  for (const p of ofKind("program-also")) {
    if (p.length !== 3) continue;
    const found = also.find(([name]) => name === p[1]);
    if (found) found[1].push(p[2]);
    else also.push([p[1], [p[2]]]);
  }
  for (const [program, dirs] of also) loud(`Looks for ${program} in ${dirs.join(", ")} too`);
  for (const p of ofKind("network")) {
    if (p.length === 3) loud(`${p[1]} uses the network: ${p[2]}`);
  }
  const toolsSeen = [];
  for (const p of ofKind("download")) {
    if (p.length !== 7) continue;
    const [, id, name, version, , host] = p;
    if (toolsSeen.includes(id)) continue;
    toolsSeen.push(id);
    loud(`Downloads ${name} ${version} from ${host} when you ask, into its own folder (nothing else on the system changes)`);
  }
  const kinds = ofKind("file-kind")
    .filter((p) => p.length > 1)
    .map((p) => p[1]);
  if (kinds.length > 0) plain(`Opens ${kinds.join(", ")} files in its own view (Text is always one click away)`);
  if (has("web")) plain("Draws its own views (web pages in a sandbox)");
  for (const p of ofKind("web-library")) {
    if (p.length === 2) plain(`Lends its ${p[1]} library to agents' pages where it is switched on`);
  }
  for (const p of ofKind("web-host")) {
    if (p.length === 2) plain(`Its pages reach ${p[1]}`);
  }
  for (const p of parsed) {
    const k = p[0] ?? "";
    if (!KNOWN_KINDS.includes(k)) {
      lines.push({ text: `Something this chimaera can't describe yet: ${p.join(" ")}`, privileged: PRIVILEGED_KINDS.includes(k) });
    }
  }
  return lines;
}

/** `describe_manifest` (what `chimaera plugin caps plugin.toml` prints):
 *  `{id, version, tier, caps, can, atoms}`. Throws on a manifest it can't
 *  read for capabilities. */
export function describeManifest(text) {
  const m = parseToml(text);
  const atoms = atomsOf(m);
  return {
    id: expectString(m.id, "id"),
    version: expectString(m.version, "version"),
    tier: tierOf(atoms),
    caps: digestOf(atoms),
    can: linesOf(atoms),
    atoms,
  };
}

/** `Caps::beyond`: the atoms of `a` that `b` lacks. */
export function beyond(a, b) {
  const base = new Set(b);
  return a.filter((x) => !base.has(x));
}

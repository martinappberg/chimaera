#!/usr/bin/env node
// The plugin kill switch's keys and signatures (plugins::revoke,
// docs/plugin-platform-plan.md §2). A fetched plugins/revoked.json counts
// only with Ed25519 signatures (plugins/revoked.sig, one hex signature per
// line) from at least `threshold` of the keys in plugins/revocation-keys.txt,
// which every chimaera build embeds.
//
//   node scripts/revocations.mjs keygen <private-key.pem>
//       A new key pair. The private key is written to that path (0600) and
//       never belongs in this repository; the public key is printed as the
//       line to add to plugins/revocation-keys.txt (it ships in the next
//       chimaera release).
//   node scripts/revocations.mjs sign --key <private-key.pem> [--append]
//       Sign plugins/revoked.json as it is now into plugins/revoked.sig
//       (replacing it, or adding a line with --append: a second maintainer
//       signs the same bytes). Commit both files together.
//   node scripts/revocations.mjs verify
//       Which embedded keys signed the current list, and whether it counts.
//
// Node's own crypto; no dependencies.

import { createPrivateKey, createPublicKey, generateKeyPairSync, sign, verify } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const LIST = join(ROOT, "plugins/revoked.json");
const SIG = join(ROOT, "plugins/revoked.sig");
const KEYS = join(ROOT, "plugins/revocation-keys.txt");
/** An Ed25519 SPKI DER key is this 12-byte prefix, then the raw 32 bytes. */
const SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

function rawPublic(key) {
  const pub = key.type === "public" ? key : createPublicKey(key);
  const der = pub.export({ format: "der", type: "spki" });
  return der.subarray(der.length - 32).toString("hex");
}

function publicFromHex(hex) {
  return createPublicKey({ key: Buffer.concat([SPKI_PREFIX, Buffer.from(hex, "hex")]), format: "der", type: "spki" });
}

/** The embedded keys and threshold, as plugins::revoke reads them. */
export function readKeys(text) {
  const keys = [];
  let threshold = 1;
  for (const raw of text.split("\n")) {
    const line = raw.split("#")[0].trim();
    if (line === "") continue;
    const t = /^threshold\s+(\d+)$/.exec(line);
    if (t !== null) {
      threshold = Math.max(1, Number(t[1]));
      continue;
    }
    if (!/^[0-9a-f]{64}$/.test(line)) throw new Error(`revocation-keys.txt: not a public key: ${line}`);
    keys.push(line);
  }
  return { keys, threshold };
}

/** The distinct embedded keys whose signature over `list` is in `sig`. */
export function signers(list, sig, keys) {
  const found = new Set();
  for (const line of sig.split("\n").map((l) => l.trim()).filter((l) => /^[0-9a-f]{128}$/.test(l))) {
    for (const key of keys) {
      if (verify(null, list, publicFromHex(key), Buffer.from(line, "hex"))) found.add(key);
    }
  }
  return found;
}

function main() {
  const [cmd, ...args] = process.argv.slice(2);
  if (cmd === "keygen") {
    const out = args[0];
    if (!out) throw new Error("usage: keygen <private-key.pem>");
    const { privateKey, publicKey } = generateKeyPairSync("ed25519");
    writeFileSync(out, privateKey.export({ format: "pem", type: "pkcs8" }), { mode: 0o600, flag: "wx" });
    console.log(`private key: ${out} (keep it offline; it never goes in the repository)`);
    console.log("add this line to plugins/revocation-keys.txt:");
    console.log(rawPublic(publicKey));
    return;
  }
  if (cmd === "sign") {
    const at = args.indexOf("--key");
    if (at === -1 || !args[at + 1]) throw new Error("usage: sign --key <private-key.pem> [--append]");
    const key = createPrivateKey(readFileSync(args[at + 1]));
    const list = readFileSync(LIST);
    JSON.parse(list.toString("utf8"));
    const line = sign(null, list, key).toString("hex");
    const kept = args.includes("--append") ? readFileSync(SIG, "utf8").trimEnd() : "";
    writeFileSync(SIG, `${kept === "" ? "" : `${kept}\n`}${line}\n`);
    const { keys } = readKeys(readFileSync(KEYS, "utf8"));
    const mine = rawPublic(key);
    console.log(`signed plugins/revoked.json as ${mine}${keys.includes(mine) ? "" : " — NOT an embedded key yet"}`);
    return;
  }
  if (cmd === "verify") {
    const { keys, threshold } = readKeys(readFileSync(KEYS, "utf8"));
    let sig = "";
    try {
      sig = readFileSync(SIG, "utf8");
    } catch {
      // No signatures yet.
    }
    const found = signers(readFileSync(LIST), sig, keys);
    console.log(`${found.size} of ${keys.length} embedded keys signed it; ${threshold} needed`);
    for (const k of found) console.log(`  ${k}`);
    if (found.size < threshold) process.exitCode = 1;
    return;
  }
  throw new Error("usage: keygen <private-key.pem> | sign --key <pem> [--append] | verify");
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}

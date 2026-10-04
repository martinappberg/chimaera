#!/usr/bin/env node
// Exercise WASM inside the signed app's actual --daemon executable. Debug
// binaries and cargo test don't reproduce hardened-runtime code-signing kills.
// Requires scripts/build-plugins.sh first, or an explicit Mycelium directory
// holding the release plugins/plugins.lock pins (the expected answer is that
// release's, below).
// Usage: node scripts/smoke-macos-plugins.mjs [chimaera.app] [mycelium-directory] [chimaera|chimaera-pro]
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { cp, mkdtemp, readFile, realpath, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';

export function smokeBinaryName(value = 'chimaera') {
  assert.ok(value === 'chimaera' || value === 'chimaera-pro', 'fixed native binary name required');
  return value;
}

async function main() {
assert.ok(process.argv.length <= 5, 'at most app, plugin directory and fixed binary name');
const binaryName = smokeBinaryName(process.argv[4]);
assert.equal(process.platform, 'darwin', 'run against the macOS signed bundle');
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const app = resolve(process.argv[2] ?? join(root, 'crates/chimaera-app/target/release/bundle/macos/chimaera.app'));
const plugin = resolve(process.argv[3] ?? join(root, 'plugins/dist-test/mycelium'));
const binary = join(app, 'Contents/MacOS', binaryName);
const details = spawnSync('/usr/bin/codesign', ['-dv', binary], { encoding: 'utf8' });
assert.equal(details.status, 0, 'bundle must be signed');
assert.match(details.stderr, /flags=.*\bruntime\b/, 'bundle must use the hardened runtime');
const verified = spawnSync('/usr/bin/codesign', ['--verify', '--strict', app], { encoding: 'utf8' });
assert.equal(verified.status, 0, `bundle signature must be valid: ${verified.stderr}`);
await readFile(join(plugin, 'plugin.wasm')); // Fail before starting a daemon if missing.
// The Knowledge response the daemon's tests pin for this same fixture tree
// and locked Mycelium (tests/knowledge.rs). Taking the expectations from it,
// not from numbers of our own, keeps a fixture change and its re-bless from
// leaving this gate stale (and every release blocked behind it).
const pinned = JSON.parse(
  await readFile(join(root, 'crates/chimaera-server/src/tests/fixtures/knowledge/mycelium.json'), 'utf8'),
);
const findings = (snapshot) =>
  snapshot.topics.flatMap((topic) => topic.findings.map(({ key, span }) => ({ key, span })));
assert.ok(pinned.counts.findings > 0, 'the pinned response must hold findings');

const scratch = await mkdtemp(join(tmpdir(), 'chimaera-plugin-smoke-'));
const workspaceRoot = join(scratch, 'workspace');
let daemon;
let exit;
let log = '';
let base;
let token;
async function api(method, path, body) {
  const response = await fetch(`${base}/api/v1${path}`, {
    method,
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(40_000),
  });
  assert.equal(response.ok, true, `${method} ${path}: ${response.status} ${await response.clone().text()}`);
  return response.json();
}

try {
  await cp(join(root, 'crates/chimaera-server/src/tests/fixtures/living'), workspaceRoot, { recursive: true });
  daemon = spawn(binary, ['--daemon'], {
    env: { ...process.env, CHIMAERA_HOME: join(scratch, 'home'), RUST_LOG: 'info' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  exit = new Promise((done) => {
    daemon.once('exit', (code, signal) => done({ code, signal }));
    daemon.once('error', (error) => done({ error: error.message }));
  });
  const capture = (chunk) => { log = (log + chunk.toString()).slice(-16_384); };
  daemon.stdout.on('data', capture);
  daemon.stderr.on('data', capture);
  for (let attempt = 0; attempt < 100; attempt++) {
    const stopped = await Promise.race([exit, delay(100).then(() => null)]);
    assert.equal(stopped, null, `daemon exited before ready: ${JSON.stringify(stopped)}`);
    let manifest;
    try {
      manifest = JSON.parse(await readFile(join(scratch, 'home/data/manifest.json'), 'utf8'));
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
      continue;
    }
    assert.equal(manifest.pid, daemon.pid, 'must be our isolated daemon');
    base = `http://127.0.0.1:${manifest.port}`;
    token = manifest.token;
    break;
  }
  assert.ok(base, 'daemon did not write a manifest within 10s');
  await api('GET', '/health');
  await api('POST', '/plugins/install', { path: plugin });
  const workspace = await api('POST', '/workspaces', { root: workspaceRoot });
  await api('PUT', `/workspaces/${workspace.id}/plugins/mycelium`, { on: true });
  const started = performance.now();
  const knowledge = await api('GET', `/workspaces/${workspace.id}/knowledge`);
  assert.equal(knowledge.provider, 'mycelium');
  // Only the plugin's reader, run over the files, can produce these: every
  // section's count, and each finding by its key at its span in its file.
  // Not the whole response: the fields derived from mtimes (`read`, each
  // `written_ms`) differ, as the tests pin mtimes and this copy doesn't.
  assert.deepEqual(knowledge.counts, pinned.counts, 'must execute the reader over the whole fixture');
  assert.deepEqual(findings(knowledge), findings(pinned), 'must return each fixture finding at its span');
  await api('GET', '/health');
  const warm = await api('GET', `/workspaces/${workspace.id}/knowledge`);
  assert.deepEqual(warm, knowledge);
  console.log(
    `PASS: signed app installed Mycelium; its reader returned the pinned fixture (${knowledge.counts.findings} findings ` +
      `in ${knowledge.topics.length} topics, every count matching) and the daemon remained healthy ` +
      `(${Math.round(performance.now() - started)} ms).`,
  );
} catch (error) {
  console.error(log.replace(/#token=[^\s]+/g, '#token=<redacted>'));
  if (daemon) console.error(`daemon exit: ${JSON.stringify(await Promise.race([exit, delay(100).then(() => 'still running')]))}`);
  throw error;
} finally {
  if (daemon) {
    daemon.kill('SIGTERM');
    const stopped = await Promise.race([exit, delay(5_000).then(() => null)]);
    if (!stopped) {
      daemon.kill('SIGKILL');
      await exit;
    }
  }
  await rm(scratch, { recursive: true, force: true });
}
}

if (process.argv[1] && await realpath(process.argv[1]) === await realpath(fileURLToPath(import.meta.url))) {
  await main();
}

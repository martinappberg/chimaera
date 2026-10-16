import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const script = join(dirname(fileURLToPath(import.meta.url)), 'build-macos-release.sh');
const identity = 'Developer ID Application: Release Fixture (TESTTEAM01)';

function run(overrides = {}, localConfig = '') {
  const root = mkdtempSync(join(tmpdir(), 'chimaera-signing-test-'));
  try {
    for (const path of ['scripts', 'bin', 'crates/chimaera-app', 'home/.config/chimaera/signing']) mkdirSync(join(root, path), { recursive: true });
    copyFileSync(script, join(root, 'scripts/build-macos-release.sh'));
    if (localConfig) writeFileSync(join(root, 'home/.config/chimaera/signing/env.sh'), localConfig);
    writeFileSync(join(root, 'notary.p8'), 'fixture only');
    const mock = `#!/bin/bash
set -eu
name=$(basename "$0")
case "$name $1" in
  'npx --no-install') step=build ;;
  'codesign --verify') step=signature ;;
  'codesign -dvvv')
    printf 'Authority=%s\\nTeamIdentifier=%s\\n' "$MOCK_IDENTITY" "$MOCK_TEAM" >&2
    exit 0 ;;
  'xcrun stapler') step=stapler ;;
  'spctl --assess') step=gatekeeper ;;
  node*) step=smoke ;;
  *) exit 99 ;;
esac
printf '%s\\n' "$step" >> "$MOCK_LOG"
if [ "$step" = "$MOCK_FAIL" ]; then exit 42; fi
`;
    for (const name of ['npx', 'codesign', 'xcrun', 'spctl', 'node']) {
      writeFileSync(join(root, 'bin', name), mock, { mode: 0o700 });
    }
    const result = spawnSync('bash', [join(root, 'scripts/build-macos-release.sh'), 'build'], {
      encoding: 'utf8',
      env: {
        PATH: `${join(root, 'bin')}:/usr/bin:/bin`,
        HOME: join(root, 'home'),
        APPLE_SIGNING_IDENTITY: identity,
        APPLE_TEAM_ID: 'TESTTEAM01',
        APPLE_API_ISSUER: 'fixture-issuer',
        APPLE_API_KEY: 'fixture-key',
        APPLE_API_KEY_PATH: join(root, 'notary.p8'),
        MOCK_IDENTITY: identity,
        MOCK_TEAM: 'TESTTEAM01',
        MOCK_LOG: join(root, 'calls'),
        MOCK_FAIL: '',
        ...overrides,
      },
    });
    let calls = [];
    try { calls = readFileSync(join(root, 'calls'), 'utf8').trim().split('\n'); } catch {}
    return { ...result, calls };
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

test('release command succeeds only after signature, ticket, Gatekeeper and plugin checks', () => {
  const result = run();
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.calls, ['build', 'signature', 'stapler', 'gatekeeper', 'smoke']);
});

test('missing credentials or ad hoc identity fail before building', () => {
  for (const overrides of [
    { APPLE_SIGNING_IDENTITY: '-' },
    { APPLE_TEAM_ID: '' },
    { APPLE_API_ISSUER: '' },
    { APPLE_API_KEY: '' },
    { APPLE_API_KEY_PATH: '/nonexistent/chimaera-notary.p8' },
  ]) {
    const result = run(overrides);
    assert.notEqual(result.status, 0);
    assert.deepEqual(result.calls, []);
  }
});

test('Apple ID notarization credentials are also accepted', () => {
  const result = run({ APPLE_ID: 'fixture@example.invalid', APPLE_PASSWORD: 'fixture', APPLE_API_ISSUER: '' });
  assert.equal(result.status, 0, result.stderr);
});

test('local signing configuration is loaded locally and ignored in CI', () => {
  const config = 'export APPLE_API_ISSUER=local-issuer\n';
  const local = run({ APPLE_API_ISSUER: '' }, config);
  assert.equal(local.status, 0, local.stderr);
  const ci = run({ CI: 'true', APPLE_API_ISSUER: '' }, config);
  assert.notEqual(ci.status, 0);
  assert.deepEqual(ci.calls, []);
});

test('wrong certificate or team blocks publication', () => {
  for (const overrides of [{ MOCK_IDENTITY: 'Apple Development: Wrong' }, { MOCK_TEAM: 'WRONGTEAM1' }]) {
    const result = run(overrides);
    assert.notEqual(result.status, 0);
    assert.deepEqual(result.calls, ['build', 'signature']);
  }
});

test('every failed build or verification step propagates to tauri-action', () => {
  const steps = ['build', 'signature', 'stapler', 'gatekeeper', 'smoke'];
  for (const [index, step] of steps.entries()) {
    const result = run({ MOCK_FAIL: step });
    assert.equal(result.status, 42, result.stderr);
    assert.deepEqual(result.calls, steps.slice(0, index + 1));
  }
});

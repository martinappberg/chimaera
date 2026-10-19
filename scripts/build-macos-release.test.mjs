import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const script = join(dirname(fileURLToPath(import.meta.url)), 'build-macos-release.sh');
const dmgScript = join(dirname(script), 'notarize-macos-dmg.sh');
const identity = 'Developer ID Application: Release Fixture (TESTTEAM01)';
const steps = ['build', 'app-signature', 'app-ticket', 'app-gatekeeper', 'smoke',
  'dmg-signature', 'submit', 'dmg-staple', 'dmg-ticket', 'dmg-final-signature', 'dmg-gatekeeper'];

function run(overrides = {}, localConfig = '', { standalone = false, dmgs = ['chimaera fixture.dmg'] } = {}) {
  const root = mkdtempSync(join(tmpdir(), 'chimaera-signing-test-'));
  try {
    for (const path of ['scripts', 'bin', 'crates/chimaera-app', 'home/.config/chimaera/signing']) mkdirSync(join(root, path), { recursive: true });
    copyFileSync(script, join(root, 'scripts/build-macos-release.sh'));
    copyFileSync(dmgScript, join(root, 'scripts/notarize-macos-dmg.sh'));
    const dmgDir = join(root, 'crates/chimaera-app/target/release/bundle/dmg');
    mkdirSync(dmgDir, { recursive: true });
    for (const name of dmgs) writeFileSync(join(dmgDir, name), 'signed fixture');
    if (localConfig) writeFileSync(join(root, 'home/.config/chimaera/signing/env.sh'), localConfig);
    writeFileSync(join(root, 'notary.p8'), 'fixture only');
    const mock = `#!/bin/bash
set -eu
name=$(basename "$0")
kind=app
case "\${!#}" in *.dmg) kind=dmg ;; esac
case "$name $1" in
  'npx --no-install') step=build ;;
  'codesign --verify')
    step="$kind-signature"
    if [ "$kind" = dmg ] && [ -f "$MOCK_LOG" ] && grep -q '^dmg-staple$' "$MOCK_LOG"; then step=dmg-final-signature; fi ;;
  'codesign -dvvv')
    if [ "$kind" = dmg ]; then
      printf 'Authority=%s\\nTeamIdentifier=%s\\n' "$MOCK_DMG_IDENTITY" "$MOCK_DMG_TEAM" >&2
      exit 0
    fi
    printf 'Authority=%s\\nTeamIdentifier=%s\\n' "$MOCK_IDENTITY" "$MOCK_TEAM" >&2
    exit 0 ;;
  'xcrun notarytool')
    step=submit
    [[ " $* " == *' --wait --timeout 20m --output-format json ' ]] || exit 98
    if [ "$MOCK_AUTH" = apple-id ]; then
      [[ " $* " == *' --apple-id fixture@example.invalid --password fixture --team-id TESTTEAM01 '* ]] || exit 97
    else
      [[ " $* " == *" --key $APPLE_API_KEY_PATH --key-id fixture-key --issuer $APPLE_API_ISSUER "* ]] || exit 96
    fi
    printf '%s\\n' "$MOCK_NOTARY_JSON" ;;
  'xcrun stapler')
    case "$2" in
      staple) step="$kind-staple" ;;
      validate) step="$kind-ticket" ;;
      *) exit 95 ;;
    esac ;;
  'spctl --assess')
    step="$kind-gatekeeper"
    if [ "$kind" = dmg ]; then
      [[ " $* " == *' --type open --context context:primary-signature '* ]] || exit 94
    fi ;;
  node*) step=smoke ;;
  *) exit 99 ;;
esac
printf '%s\\n' "$step" >> "$MOCK_LOG"
if [ "$step" = "$MOCK_FAIL" ]; then exit 42; fi
`;
    for (const name of ['npx', 'codesign', 'xcrun', 'spctl', 'node']) {
      writeFileSync(join(root, 'bin', name), mock, { mode: 0o700 });
    }
    // Linux CI has no plutil; exercise JSON parsing with the host Node binary.
    writeFileSync(join(root, 'bin/plutil'), `#!${process.execPath}
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv.at(-1), 'utf8'));
if (typeof result.status !== 'string') process.exit(1);
process.stdout.write(result.status);
`, { mode: 0o700 });
    const args = standalone
      ? [join(root, 'scripts/notarize-macos-dmg.sh'), join(dmgDir, 'chimaera fixture.dmg')]
      : [join(root, 'scripts/build-macos-release.sh'), 'build'];
    const result = spawnSync('bash', args, {
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
        MOCK_DMG_IDENTITY: identity,
        MOCK_DMG_TEAM: 'TESTTEAM01',
        MOCK_AUTH: 'api',
        MOCK_NOTARY_JSON: JSON.stringify({ status: 'Accepted', id: 'fixture-submission' }),
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

test('release command requires app checks and accepted, stapled, Gatekeeper-approved DMG', () => {
  const result = run();
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.calls, steps);
});

test('missing credentials or ad hoc identity fail before building', () => {
  for (const overrides of [
    { APPLE_SIGNING_IDENTITY: '-' },
    { APPLE_TEAM_ID: '' },
    { APPLE_API_ISSUER: '' },
    { APPLE_API_KEY: '' },
    { APPLE_API_KEY_PATH: '/nonexistent/chimaera-notary.p8' },
  ]) {
    for (const standalone of [false, true]) {
      const result = run(overrides, '', { standalone });
      assert.notEqual(result.status, 0);
      assert.deepEqual(result.calls, []);
    }
  }
});

test('Apple ID notarization credentials are also accepted', () => {
  const result = run({ APPLE_ID: 'fixture@example.invalid', APPLE_PASSWORD: 'fixture', APPLE_API_ISSUER: '', MOCK_AUTH: 'apple-id' });
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
    assert.deepEqual(result.calls, ['build', 'app-signature']);
  }
});

test('every failed build or verification step propagates to tauri-action', () => {
  for (const [index, step] of steps.entries()) {
    const result = run({ MOCK_FAIL: step });
    assert.equal(result.status, 42, result.stderr);
    assert.deepEqual(result.calls, steps.slice(0, index + 1));
  }
});

test('existing signed DMGs can be repaired without rebuilding the app', () => {
  const result = run({}, '', { standalone: true });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.calls, steps.slice(5));
});

test('DMG certificate and team must match before submission', () => {
  for (const overrides of [{ MOCK_DMG_IDENTITY: 'Apple Development: Wrong' }, { MOCK_DMG_TEAM: 'WRONGTEAM1' }]) {
    const result = run(overrides);
    assert.notEqual(result.status, 0);
    assert.deepEqual(result.calls, steps.slice(0, 6));
  }
});

test('rejected, pending, missing or malformed notary status blocks stapling and publication', () => {
  for (const response of ['{"status":"Invalid"}', '{"status":"In Progress"}', '{}', 'not JSON']) {
    const result = run({ MOCK_NOTARY_JSON: response });
    assert.notEqual(result.status, 0);
    assert.deepEqual(result.calls, steps.slice(0, 7));
  }
});

test('missing or ambiguous DMG output blocks publication', () => {
  for (const dmgs of [[], ['one.dmg', 'two.dmg']]) {
    const result = run({}, '', { dmgs });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Expected exactly one release DMG/);
    assert.deepEqual(result.calls, steps.slice(0, 5));
  }
});

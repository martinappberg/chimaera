import test from 'node:test';
import assert from 'node:assert/strict';
import { smokeBinaryName } from './smoke-macos-plugins.mjs';

test('signed smoke defaults to original binary and admits only the fixed private assembly', () => {
  assert.equal(smokeBinaryName(), 'chimaera');
  assert.equal(smokeBinaryName('chimaera'), 'chimaera');
  assert.equal(smokeBinaryName('chimaera-pro'), 'chimaera-pro');
  for (const value of ['', '../chimaera-pro', '/tmp/chimaera-pro', 'chimaera-pro --daemon', null]) {
    assert.throws(() => smokeBinaryName(value), /fixed native binary name/);
  }
});

test('alias invocation runs main and refuses fixed arguments before bundle/socket work', async () => {
  const { mkdtemp, symlink, rm } = await import('node:fs/promises');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { spawnSync } = await import('node:child_process');
  const scratch = await mkdtemp(join(tmpdir(), 'chimaera-smoke-entry-'));
  try {
    const alias = join(scratch, 'alias.mjs');
    await symlink(new URL('./smoke-macos-plugins.mjs', import.meta.url), alias);
    const child = spawnSync(process.execPath, [alias, '/not-an-app', '/not-a-plugin', 'arbitrary-binary'], {
      encoding: 'utf8', timeout: 2000, maxBuffer: 65536,
    });
    assert.equal(child.error, undefined);
    assert.equal(child.status, 1);
    assert.match(child.stderr, /fixed native binary name required/);
  } finally { await rm(scratch, { recursive: true, force: true }); }
});

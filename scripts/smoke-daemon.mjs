#!/usr/bin/env node
// Headless end-to-end smoke against a RUNNING daemon — the "verify live" floor
// where no browser pane exists (Claude cloud sessions; see
// docs/agent-guides/cloud-sessions.md). Opens a workspace, spawns a shell
// session, round-trips a command through the session WebSocket (first-frame
// auth, binary PTY frames), then deletes the session. Zero dependencies; Node 22's
// built-in WebSocket and fetch.
//
//   node scripts/smoke-daemon.mjs [manifest.json] [workspace-root]
//
// Defaults: the isolated daemon's manifest (.chimaera-dev/data/manifest.json,
// written by .claude/skills/develop/serve-isolated.sh) and the repo root.
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const manifestPath = process.argv[2] ?? resolve(root, '.chimaera-dev/data/manifest.json');
const workspaceRoot = process.argv[3] ?? root;
const { port, token } = JSON.parse(readFileSync(manifestPath, 'utf8'));
const base = `http://127.0.0.1:${port}`;

async function api(method, path, body) {
  const res = await fetch(base + path, {
    method,
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${path} → ${res.status} ${text}`);
  return text ? JSON.parse(text) : null;
}

const workspace = await api('POST', '/api/v1/workspaces', { root: workspaceRoot });
const session = await api('POST', '/api/v1/sessions', {
  workspace_id: workspace.id,
  kind: 'shell',
  cols: 100,
  rows: 30,
});
console.log(`workspace ${workspace.id} → shell session ${session.id}`);

// The marker is computed by the shell, so seeing it proves a real round trip
// (input reached the PTY, output came back) rather than a local echo.
const marker = 'chimaera-smoke-42';
try {
  await new Promise((resolveRun, reject) => {
    const ws = new WebSocket(`ws://127.0.0.1:${port}/ws/sessions/${session.id}`);
    ws.binaryType = 'arraybuffer';
    let seen = '';
    const timer = setTimeout(() => {
      ws.close();
      reject(new Error(`no "${marker}" within 15s; last output:\n${seen.slice(-500)}`));
    }, 15000);
    ws.onopen = () => {
      ws.send(JSON.stringify({ type: 'auth', token }));
      ws.send(new TextEncoder().encode(`echo chimaera-smoke-$((6*7))\r`));
    };
    ws.onmessage = (ev) => {
      if (typeof ev.data === 'string') return; // JSON control frames
      seen += new TextDecoder().decode(ev.data);
      if (seen.includes(marker)) {
        clearTimeout(timer);
        ws.close();
        resolveRun();
      }
    };
    ws.onerror = () => {
      clearTimeout(timer);
      reject(new Error('session websocket error'));
    };
  });
  console.log(`✓ PTY round trip over /ws/sessions/${session.id}`);
} finally {
  await api('DELETE', `/api/v1/sessions/${session.id}`).catch(() => {});
}

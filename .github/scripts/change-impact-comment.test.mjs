import assert from 'node:assert/strict';
import test from 'node:test';

import { COMMENT_MARKER, findMarkerComment, upsertComment, withMarker } from './change-impact-comment.mjs';

const bot = (id, body) => ({ id, user: { login: 'github-actions[bot]' }, body });

test('withMarker prepends once and trims', () => {
  const marked = withMarker('  ### Hi\n\n');
  assert.equal(marked, `${COMMENT_MARKER}\n### Hi\n`);
  assert.equal(withMarker(marked), marked);
});

test('findMarkerComment: only the bot, only with the marker', () => {
  const mine = bot(3, `${COMMENT_MARKER}\nold`);
  const comments = [
    { id: 1, user: { login: 'someone' }, body: `${COMMENT_MARKER} spoof` },
    bot(2, 'other bot comment'),
    mine,
    { id: 4, body: COMMENT_MARKER },
  ];
  assert.equal(findMarkerComment(comments), mine);
  assert.equal(findMarkerComment([]), null);
});

function fakeApi(pages) {
  const calls = [];
  return {
    repository: 'o/r',
    calls,
    async request(method, path, body) {
      calls.push([method, path, body]);
      if (method === 'GET') return pages[Number(new URL(`http://x${path}`).searchParams.get('page')) - 1] ?? [];
      return {};
    },
  };
}

test('upsertComment: creates, updates, leaves unchanged', async () => {
  const body = withMarker('new');
  const created = fakeApi([[]]);
  assert.equal(await upsertComment(created, 7, body), 'created');
  assert.deepEqual(created.calls.at(-1), ['POST', '/repos/o/r/issues/7/comments', { body }]);

  const updated = fakeApi([[bot(9, withMarker('old'))]]);
  assert.equal(await upsertComment(updated, 7, body), 'updated');
  assert.deepEqual(updated.calls.at(-1), ['PATCH', '/repos/o/r/issues/comments/9', { body }]);

  const same = fakeApi([[bot(9, body)]]);
  assert.equal(await upsertComment(same, 7, body), 'unchanged');
  assert.equal(same.calls.length, 1);
});

test('upsertComment: finds the marker on a later page', async () => {
  const filler = Array.from({ length: 100 }, (_, i) => ({ id: i, user: { login: 'u' }, body: 'x' }));
  const api = fakeApi([filler, [bot(500, withMarker('old'))]]);
  assert.equal(await upsertComment(api, 1, withMarker('new')), 'updated');
  assert.equal(api.calls.filter(([m]) => m === 'GET').length, 2);
});

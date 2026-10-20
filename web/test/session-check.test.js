import assert from 'node:assert/strict';
import { test } from 'node:test';

import { sessionVerdict } from '../src/session-check.js';

test('an ended session ends the stream', () => {
  assert.deepEqual(sessionVerdict(401, null), { kind: 'ended' });
});

test('a cursor the log no longer reaches back to resyncs from the newest event the answer names', () => {
  const body = {
    error: { code: 'cursor_expired', message: 'x.', details: { pruned_through: 40, latest: 97 } },
  };
  assert.deepEqual(sessionVerdict(410, body), { kind: 'resync', from: 97 });
});

test('an expired cursor without a usable newest event resyncs from the start', () => {
  for (const body of [null, {}, { error: { details: { latest: -1 } } }, { error: { details: { latest: '9' } } }]) {
    assert.deepEqual(sessionVerdict(410, body), { kind: 'resync', from: 0 }, JSON.stringify(body));
  }
});

test('anything else lets the stream be opened again', () => {
  for (const status of [0, 204, 500, 503]) {
    assert.deepEqual(sessionVerdict(status, null), { kind: 'retry' }, String(status));
  }
});

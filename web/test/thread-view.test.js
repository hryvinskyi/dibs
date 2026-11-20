import assert from 'node:assert/strict';
import { test } from 'node:test';

import { messageOfAnchor, settle } from '../src/thread-view.js';

test('an answer replaces the messages it was asked for that it holds', () => {
  assert.deepEqual(settle([4, 5], [4, 5], new Set([4, 5, 9])), { replace: [4, 5], waiting: [] });
});

test('a message the answer lacks stays as appended and waits no more', () => {
  assert.deepEqual(settle([4, 5], [4, 5], new Set([5])), { replace: [5], waiting: [] });
});

test('a failed answer, or one for an address the page left, replaces nothing and settles what it was asked for', () => {
  assert.deepEqual(settle([4, 5], [4, 5], new Set()), { replace: [], waiting: [] });
});

test('a message appended after the answer was asked for still waits, even when the answer holds it', () => {
  assert.deepEqual(settle([4, 5, 6], [4, 5], new Set([4, 5, 6])), { replace: [4, 5], waiting: [6] });
});

test('only a message anchor names a message', () => {
  assert.equal(messageOfAnchor('m-17'), 17);
  assert.equal(messageOfAnchor('m-0'), 0);
  for (const anchor of ['', 'm-', 'm-1a', 'x-17', 'm--1', ' m-1', 'reply-body', `m-${'9'.repeat(20)}`]) {
    assert.equal(messageOfAnchor(anchor), null, anchor);
  }
});

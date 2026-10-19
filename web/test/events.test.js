import assert from 'node:assert/strict';
import { test } from 'node:test';

import { ContractError, createSeen, isChange, messageOf, parseEvent } from '../src/events.js';

const posted = {
  id: 12,
  at: '2026-03-02T09:00:00Z',
  kind: 'message_posted',
  payload: { thread: 4, message: 9, author: 2 },
  members: { 2: 'bob' },
  message: {
    id: 9,
    thread: 4,
    author: 'bob',
    label: 'web',
    body: '<b>hi</b>\nthere',
    created_at: '2026-03-02T09:00:00Z',
  },
};

test('a posted message parses with its text as written', () => {
  const event = parseEvent(JSON.stringify(posted));
  assert.equal(event.id, 12);
  assert.equal(event.kind, 'message_posted');
  assert.deepEqual(event.message, {
    id: 9,
    thread: 4,
    author: 'bob',
    label: 'web',
    body: '<b>hi</b>\nthere',
    createdAt: '2026-03-02T09:00:00Z',
  });
});

test('an event that announces no message parses without one', () => {
  const event = parseEvent(JSON.stringify({ id: 3, kind: 'claimed', payload: {}, members: {} }));
  assert.equal(event.message, null);
  assert.equal(event.kind, 'claimed');
});

test('data of another shape is a contract error naming where it broke', () => {
  const cases = [
    [42, '$'],
    ['not json', '$'],
    ['[1]', '$'],
    [JSON.stringify({ kind: 'claimed' }), '$.id'],
    [JSON.stringify({ id: 0, kind: 'claimed' }), '$.id'],
    [JSON.stringify({ id: 1.5, kind: 'claimed' }), '$.id'],
    [JSON.stringify({ id: 1, kind: '' }), '$.kind'],
    [JSON.stringify({ id: 1, kind: 7 }), '$.kind'],
    [JSON.stringify({ ...posted, message: 'text' }), '$.message'],
    [JSON.stringify({ ...posted, message: { ...posted.message, body: 5 } }), '$.message.body'],
    [JSON.stringify({ ...posted, message: { ...posted.message, thread: -4 } }), '$.message.thread'],
  ];
  for (const [data, path] of cases) {
    assert.throws(
      () => parseEvent(data),
      (error) => error instanceof ContractError && error.path === path,
      String(data),
    );
  }
});

test('a message belongs to the thread it was posted to only', () => {
  const event = parseEvent(JSON.stringify(posted));
  assert.equal(messageOf(event, 4)?.id, 9);
  assert.equal(messageOf(event, 5), null);
  const claimed = parseEvent(JSON.stringify({ id: 3, kind: 'claimed' }));
  assert.equal(messageOf(claimed, 4), null);
  const odd = parseEvent(JSON.stringify({ ...posted, kind: 'thread_started' }));
  assert.equal(messageOf(odd, 4), null);
});

test('a change is an event of one of the kinds named', () => {
  const kinds = new Set(['claimed', 'released']);
  assert.equal(isChange(parseEvent(JSON.stringify({ id: 1, kind: 'claimed' })), kinds), true);
  assert.equal(isChange(parseEvent(JSON.stringify(posted)), kinds), false);
});

test('an id is handled once, and the memory forgets the oldest past its limit', () => {
  const seen = createSeen(3);
  assert.equal(seen.add(1), true);
  assert.equal(seen.add(1), false);
  seen.add(2);
  seen.add(3);
  seen.add(4);
  assert.equal(seen.has(1), false);
  assert.equal(seen.has(2), true);
  assert.equal(seen.has(4), true);
  assert.equal(seen.add(4), false);
});

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { anchorOf, messageOfAnchor } from '../src/message-anchors.js';
import { ackEntryView, messageView, stampOf } from '../src/message-view.js';
import { settle } from '../src/rendered-messages.js';

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

test('a message anchor names the message it was made for', () => {
  assert.equal(anchorOf(42), 'm-42');
  assert.equal(messageOfAnchor(anchorOf(42)), 42);
});

test('only a message anchor names a message', () => {
  assert.equal(messageOfAnchor('m-17'), 17);
  assert.equal(messageOfAnchor('m-0'), 0);
  for (const anchor of ['', 'm-', 'm-1a', 'x-17', 'm--1', ' m-1', 'reply-body', `m-${'9'.repeat(20)}`]) {
    assert.equal(messageOfAnchor(anchor), null, anchor);
  }
});

/** A fixed clock's moment: the 20th of November 2026, 15:30 in the zone the test runs in. */
const NOW = new Date(2026, 10, 20, 15, 30);

/**
 * An RFC 3339 time for a wall-clock moment in the zone the test runs in.
 * @param {number} month zero-based
 * @param {number} day
 * @param {number} hours
 * @param {number} minutes
 * @returns {string}
 */
const at = (month, day, hours, minutes) => new Date(2026, month, day, hours, minutes).toISOString();

/** @type {import('../src/message-view.js').ViewerConfig} */
const viewer = {
  me: 'bob',
  sender: 'alice',
  systemAuthor: 'from dibs',
  ackAction: '/ui/threads/7/messages/{message}/ack',
};

/** @type {import('../src/events.js').StreamMessage} */
const message = {
  id: 42,
  thread: 7,
  author: 'alice',
  system: false,
  label: 'alice-laptop',
  body: 'Shipping it today.',
  importance: 'normal',
  ackRequired: false,
  createdAt: at(10, 20, 9, 5),
};

test('a time of the same day shows its hour and minute against the clock given', () => {
  assert.deepEqual(stampOf(at(10, 20, 9, 5), NOW), { datetime: at(10, 20, 9, 5), text: '09:05' });
});

test('a time of another day shows its date as well', () => {
  assert.deepEqual(stampOf(at(10, 19, 23, 59), NOW), { datetime: at(10, 19, 23, 59), text: '2026-11-19 23:59' });
});

test('the same moment reads differently against another clock', () => {
  const tomorrow = new Date(2026, 10, 21, 8, 0);
  assert.equal(stampOf(at(10, 20, 9, 5), tomorrow).text, '2026-11-20 09:05');
});

test('a value that is not a time keeps the text the element has', () => {
  assert.deepEqual(stampOf('yesterday', NOW), { datetime: 'yesterday', text: null });
});

test("a teammate's message shows its author, session and time, with no badge and no acknowledge form", () => {
  assert.deepEqual(messageView(message, viewer, NOW), {
    anchor: 'm-42',
    mine: false,
    system: false,
    author: 'alice',
    session: 'alice-laptop',
    body: 'Shipping it today.',
    badge: null,
    asksAck: false,
    ackAction: null,
    time: { datetime: message.createdAt, text: '09:05' },
  });
});

test("the reader's own message is marked as theirs and offers them no acknowledge form", () => {
  const view = messageView({ ...message, author: 'bob', ackRequired: true }, viewer, NOW);
  assert.equal(view.mine, true);
  assert.equal(view.asksAck, true);
  assert.equal(view.ackAction, null);
});

test('a raised message asking for an acknowledgement shows its badge and posts to its own address', () => {
  const view = messageView({ ...message, importance: 'urgent', ackRequired: true }, viewer, NOW);
  assert.equal(view.badge, 'urgent');
  assert.equal(view.ackAction, '/ui/threads/7/messages/42/ack');
});

test('what dibs wrote is named as the page says and names no session', () => {
  const view = messageView({ ...message, author: 'dibs', system: true, label: 'ignored' }, viewer, NOW);
  assert.equal(view.author, 'from dibs');
  assert.equal(view.session, null);
  assert.equal(view.mine, false);
  assert.equal(view.system, true);
});

test('an acknowledgement names its recipient, marks the reader as themselves, and stamps its time', () => {
  const acked = { thread: 7, message: 42, member: 'bob', at: at(10, 20, 15, 1) };
  assert.deepEqual(ackEntryView(acked, 'bob', NOW), {
    member: 'bob',
    you: true,
    time: { datetime: acked.at, text: '15:01' },
  });
  assert.equal(ackEntryView(acked, 'alice', NOW).you, false);
});

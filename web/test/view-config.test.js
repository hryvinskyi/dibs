import assert from 'node:assert/strict';
import { test } from 'node:test';

import { ContractError } from '../src/events.js';
import { countLabel, isLocalPath, parseThreadConfig } from '../src/view-config.js';

const thread = {
  streamUrl: '/ui/events/stream',
  sessionUrl: '/ui/session',
  after: '17',
  threadId: '4',
  me: 'alice',
  sender: 'bob',
  systemAuthor: 'from dibs',
  newMessageLabel: '1 new message',
  newMessagesLabel: '{count} new messages',
  ackAction: '/ui/threads/4/messages/{message}/ack',
};

test('a thread page configuration parses from its attributes', () => {
  assert.deepEqual(parseThreadConfig(thread), {
    streamUrl: '/ui/events/stream',
    sessionUrl: '/ui/session',
    after: 17,
    thread: 4,
    me: 'alice',
    sender: 'bob',
    systemAuthor: 'from dibs',
    newMessageLabel: '1 new message',
    newMessagesLabel: '{count} new messages',
    ackAction: '/ui/threads/4/messages/{message}/ack',
  });
  assert.equal(parseThreadConfig({ ...thread, after: '0' }).after, 0);
  assert.equal(parseThreadConfig({ ...thread, sender: '' }).sender, '', 'a thread dibs started');
  assert.equal(parseThreadConfig({ ...thread, sender: undefined }).sender, '');
});

test('a missing or malformed attribute is a contract error naming it', () => {
  /** @type {{ data: Record<string, string | undefined>, path: string }[]} */
  const cases = [
    { data: { ...thread, threadId: undefined }, path: 'dataset.threadId' },
    { data: { ...thread, threadId: '0' }, path: 'dataset.threadId' },
    { data: { ...thread, after: '-1' }, path: 'dataset.after' },
    { data: { ...thread, after: '1e3' }, path: 'dataset.after' },
    { data: { ...thread, me: ' ' }, path: 'dataset.me' },
    { data: { ...thread, streamUrl: '' }, path: 'dataset.streamUrl' },
    { data: { ...thread, ackAction: undefined }, path: 'dataset.ackAction' },
    { data: { ...thread, ackAction: '/ui/threads/4/messages/ack' }, path: 'dataset.ackAction' },
    { data: { ...thread, systemAuthor: undefined }, path: 'dataset.systemAuthor' },
  ];
  for (const { data, path } of cases) {
    assert.throws(
      () => parseThreadConfig(data),
      (error) => error instanceof ContractError && error.path === path,
      path,
    );
  }
});

test('the count label is the single form for one and the counted form otherwise', () => {
  assert.equal(countLabel(1, '1 new message', '{count} new messages'), '1 new message');
  assert.equal(countLabel(3, '1 new message', '{count} new messages'), '3 new messages');
  assert.equal(countLabel(0, 'one', '{count} of {count}'), '0 of 0');
});

test('a local path starts with one slash and holds no backslash, space or control character', () => {
  assert.equal(isLocalPath('/ui/claims/7?filter=silent&who=bob'), true);
  for (const value of ['', 'claims', '//example.com', '/\\example.com', '/a\\b', '/ui/ claims', '/ui/\nclaims', 'https://example.com/']) {
    assert.equal(isLocalPath(value), false, JSON.stringify(value));
  }
});

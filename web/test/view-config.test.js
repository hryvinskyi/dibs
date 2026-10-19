import assert from 'node:assert/strict';
import { test } from 'node:test';

import { ContractError } from '../src/events.js';
import { countLabel, parseStatusConfig, parseThreadConfig } from '../src/view-config.js';

const thread = {
  streamUrl: '/ui/events/stream',
  sessionUrl: '/ui/session',
  after: '17',
  threadId: '4',
  me: 'alice',
  newMessageLabel: '1 new message',
  newMessagesLabel: '{count} new messages',
};

test('a thread page configuration parses from its attributes', () => {
  assert.deepEqual(parseThreadConfig(thread), {
    streamUrl: '/ui/events/stream',
    sessionUrl: '/ui/session',
    after: 17,
    thread: 4,
    me: 'alice',
    newMessageLabel: '1 new message',
    newMessagesLabel: '{count} new messages',
  });
  assert.equal(parseThreadConfig({ ...thread, after: '0' }).after, 0);
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
  ];
  for (const { data, path } of cases) {
    assert.throws(
      () => parseThreadConfig(data),
      (error) => error instanceof ContractError && error.path === path,
      path,
    );
  }
});

test('a status page names the kinds of event that change it', () => {
  const status = {
    streamUrl: '/ui/events/stream',
    sessionUrl: '/ui/session',
    after: '0',
    changedKinds: ' claimed  released\ntaken_over ',
  };
  const config = parseStatusConfig(status);
  assert.deepEqual([...config.kinds], ['claimed', 'released', 'taken_over']);
  assert.throws(() => parseStatusConfig({ ...status, changedKinds: undefined }), ContractError);
});

test('the count label is the single form for one and the counted form otherwise', () => {
  assert.equal(countLabel(1, '1 new message', '{count} new messages'), '1 new message');
  assert.equal(countLabel(3, '1 new message', '{count} new messages'), '3 new messages');
  assert.equal(countLabel(0, 'one', '{count} of {count}'), '0 of 0');
});

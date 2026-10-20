import assert from 'node:assert/strict';
import { test } from 'node:test';

import { ContractError } from '../src/events.js';
import { countLabel, isLocalPath, parseBoardConfig, parseStatusConfig, parseThreadConfig } from '../src/view-config.js';

const thread = {
  streamUrl: '/ui/events/stream',
  sessionUrl: '/ui/session',
  after: '17',
  threadId: '4',
  me: 'alice',
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
    newMessageLabel: '1 new message',
    newMessagesLabel: '{count} new messages',
    ackAction: '/ui/threads/4/messages/{message}/ack',
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
    { data: { ...thread, ackAction: undefined }, path: 'dataset.ackAction' },
    { data: { ...thread, ackAction: '/ui/threads/4/messages/ack' }, path: 'dataset.ackAction' },
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

const board = {
  streamUrl: '/ui/events/stream',
  sessionUrl: '/ui/session',
  after: '3',
  changedKinds: 'claimed  item_updated',
  fragmentUrl: '/ui/board/fragment?area=api%3Abilling',
};

test('a board configuration parses with its fragment address', () => {
  const config = parseBoardConfig(board);
  assert.equal(config.fragmentUrl, '/ui/board/fragment?area=api%3Abilling');
  assert.equal(config.after, 3);
  assert.deepEqual([...config.kinds], ['claimed', 'item_updated']);
});

test('a board fragment address must be a path of this service', () => {
  for (const fragmentUrl of [
    undefined,
    '',
    'https://example.com/ui/board/fragment',
    '//example.com/x',
    '/\\example.com',
    'ui/board',
    '/ui/board fragment',
  ]) {
    assert.throws(
      () => parseBoardConfig({ ...board, fragmentUrl }),
      (error) => error instanceof ContractError && error.path === 'dataset.fragmentUrl',
      String(fragmentUrl),
    );
  }
  assert.throws(
    () => parseBoardConfig({ ...board, changedKinds: undefined }),
    (error) => error instanceof ContractError && error.path === 'dataset.changedKinds',
  );
});

test('a local path starts with one slash and holds no backslash, space or control character', () => {
  assert.equal(isLocalPath('/ui/board/fragment?area=api%3Abilling&member=bob'), true);
  for (const value of ['', 'board', '//example.com', '/\\example.com', '/a\\b', '/ui/ board', '/ui/\nboard', 'https://example.com/']) {
    assert.equal(isLocalPath(value), false, JSON.stringify(value));
  }
});

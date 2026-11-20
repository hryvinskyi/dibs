import assert from 'node:assert/strict';
import { test } from 'node:test';

import { ContractError } from '../src/events.js';
import { parseScreenConfig } from '../src/screen-config.js';

const shell = {
  streamUrl: '/ui/events/stream',
  sessionUrl: '/ui/session',
  after: '41',
  changedKinds: 'claimed  released\tcompleted',
};

test("the screen's stream, cursor and change kinds come from the app element", () => {
  const config = parseScreenConfig(shell);
  assert.deepEqual(config.stream, { streamUrl: '/ui/events/stream', sessionUrl: '/ui/session', after: 41 });
  assert.deepEqual([...(config.kinds ?? [])], ['claimed', 'released', 'completed']);
});

test('without change kinds the screen follows every change', () => {
  const { changedKinds: _, ...without } = shell;
  assert.equal(parseScreenConfig(without).kinds, null);
});

test('an empty list of change kinds is an empty set, not every change', () => {
  for (const listed of ['', '  \t ']) {
    const { kinds } = parseScreenConfig({ ...shell, changedKinds: listed });
    assert.deepEqual([...(kinds ?? ['every change'])], [], JSON.stringify(listed));
  }
});

test('a missing or malformed address or cursor is a contract error', () => {
  for (const broken of [
    { ...shell, streamUrl: undefined },
    { ...shell, sessionUrl: '' },
    { ...shell, streamUrl: 'https://elsewhere.example.com/stream' },
    { ...shell, sessionUrl: '//elsewhere.example.com/session' },
    { ...shell, after: '-1' },
    { ...shell, after: 'soon' },
    { ...shell, after: '99999999999999999999' },
  ]) {
    assert.throws(() => parseScreenConfig(broken), ContractError, JSON.stringify(broken));
  }
});

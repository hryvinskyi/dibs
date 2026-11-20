import assert from 'node:assert/strict';
import { test } from 'node:test';

import { ContractError } from '../src/events.js';
import { answerStillWanted, focusIndex, isRelevant, parseScreenConfig, regionAction } from '../src/screen-view.js';

test('a region its own script keeps current is skipped, even with a form in use', () => {
  assert.equal(regionAction({ ownLive: true, focusInside: false, edited: false }), 'skip');
  assert.equal(regionAction({ ownLive: true, focusInside: true, edited: true }), 'skip');
});

test('a region with the focus in a form control is held', () => {
  assert.equal(regionAction({ ownLive: false, focusInside: true, edited: false }), 'hold');
});

test('a region holding a control the person changed is held, wherever the focus is', () => {
  assert.equal(regionAction({ ownLive: false, focusInside: false, edited: true }), 'hold');
});

test('any other region is swapped', () => {
  assert.equal(regionAction({ ownLive: false, focusInside: false, edited: false }), 'swap');
});

/**
 * @param {string} kind
 * @returns {import('../src/events.js').StreamEvent}
 */
const event = (kind) => ({ id: 7, kind, message: null, ack: null });

test('an event of a change kind the screen lists asks for a refresh', () => {
  assert.equal(isRelevant(event('claimed'), new Set(['claimed', 'released'])), true);
});

test('an event of a kind the screen does not list does not', () => {
  assert.equal(isRelevant(event('message_posted'), new Set(['claimed', 'released'])), false);
  assert.equal(isRelevant(event('claimed'), new Set()), false);
});

test('a screen that lists no kinds follows every change', () => {
  assert.equal(isRelevant(event('message_posted'), null), true);
});

test('an answer is still wanted only while the page shows the address it was fetched for', () => {
  const at = 'http://dibs.example.com/ui/claims/7?who=alice';
  assert.equal(answerStillWanted(at, at), true);
  assert.equal(answerStillWanted(at, `${at}#notes`), true, 'only the fragment differs');
  assert.equal(answerStillWanted(at, 'http://dibs.example.com/ui/claims/7?who=bob'), false, 'another query');
  assert.equal(answerStillWanted(at, 'http://dibs.example.com/ui/claims/7'), false, 'no query');
  assert.equal(answerStillWanted(at, 'http://dibs.example.com/ui/claims/8?who=alice'), false, 'another path');
  assert.equal(answerStillWanted(at, 'http://other.example.com/ui/claims/7?who=alice'), false, 'another origin');
  assert.equal(answerStillWanted(at, 'not an address'), false);
});

test('the focus goes back to the element with the same id, or else the link with the same address', () => {
  const candidates = [
    { id: '', href: '/ui/claims/7' },
    { id: 'slot-deploy', href: '' },
    { id: 'reason', href: '/ui/claims/12' },
  ];
  assert.equal(focusIndex({ id: 'slot-deploy', href: '' }, candidates), 1);
  assert.equal(focusIndex({ id: 'reason', href: '/ui/claims/7' }, candidates), 2, 'the id wins');
  assert.equal(focusIndex({ id: 'gone', href: '/ui/claims/7' }, candidates), 0);
  assert.equal(focusIndex({ id: '', href: '' }, candidates), -1, 'an empty id or address matches nothing');
  assert.equal(focusIndex({ id: 'gone', href: '/ui/queue' }, candidates), -1);
});

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

test('an empty list of change kinds refreshes on no change at all', () => {
  const { kinds } = parseScreenConfig({ ...shell, changedKinds: '' });
  assert.deepEqual([...(kinds ?? ['every change'])], []);
  assert.equal(isRelevant(event('claimed'), kinds), false);
});

test('a missing or malformed address or cursor is a contract error', () => {
  for (const broken of [
    { ...shell, streamUrl: undefined },
    { ...shell, sessionUrl: '' },
    { ...shell, streamUrl: 'https://elsewhere.example.com/stream' },
    { ...shell, sessionUrl: '//elsewhere.example.com/session' },
    { ...shell, after: '-1' },
    { ...shell, after: 'soon' },
  ]) {
    assert.throws(() => parseScreenConfig(broken), ContractError, JSON.stringify(broken));
  }
});

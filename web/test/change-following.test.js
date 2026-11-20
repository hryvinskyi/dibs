import assert from 'node:assert/strict';
import { test } from 'node:test';

import { asksItsAddress, isRelevant, reloadAddress } from '../src/change-following.js';
import { parseScreenConfig } from '../src/screen-config.js';

/**
 * @param {string} kind
 * @returns {import('../src/events.js').StreamEvent}
 */
const event = (kind) => ({ id: 7, kind, message: null, ack: null });

/**
 * The change kinds of a frame listing `listed`, or listing none when `undefined`.
 * @param {string | undefined} listed
 * @returns {ReadonlySet<string> | null}
 */
const kindsOf = (listed) =>
  parseScreenConfig({ streamUrl: '/ui/events/stream', sessionUrl: '/ui/session', after: '3', changedKinds: listed }).kinds;

const POSTED_TO = 'http://dibs.example.com/ui/threads/7/messages';
const SHOWN = 'http://dibs.example.com/ui/claims/7?who=alice';

test('an event of a change kind the screen lists asks for a refresh', () => {
  assert.equal(isRelevant(event('claimed'), new Set(['claimed', 'released'])), true);
});

test('an event of a kind the screen does not list does not', () => {
  assert.equal(isRelevant(event('message_posted'), new Set(['claimed', 'released'])), false);
});

test('a screen that lists no kinds follows every change', () => {
  assert.equal(isRelevant(event('message_posted'), null), true);
});

test('a page answering a form refreshes on no change, never asks for its own address, and keeps its Reload', () => {
  const kinds = kindsOf('');
  assert.equal(isRelevant(event('claimed'), kinds), false);
  assert.equal(isRelevant(event('message_posted'), kinds), false);
  assert.equal(asksItsAddress(kinds), false, 'not even on a resync');
  assert.equal(reloadAddress(kinds, POSTED_TO), null, "Reload keeps the server's address");
});

test('a screen at its own address asks for it again, and its Reload follows the address shown', () => {
  for (const kinds of [kindsOf('claimed released'), kindsOf(undefined)]) {
    assert.equal(asksItsAddress(kinds), true);
    assert.equal(reloadAddress(kinds, SHOWN), SHOWN);
  }
});

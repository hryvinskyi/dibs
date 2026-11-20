import assert from 'node:assert/strict';
import { test } from 'node:test';

import { offerFor, offered } from '../src/palette-offer.js';

/** @type {Element} */
const REGION = /** @type {Element} */ (/** @type {unknown} */ ({ tagName: 'DIV' }));

test('an answer with the results region shows it', () => {
  const answer = { kind: /** @type {const} */ ('ok'), regions: new Map([['results', REGION]]) };
  assert.deepEqual(offerFor(answer), { kind: 'show', region: REGION });
});

test('an answer that failed clears the hits of the earlier query', () => {
  assert.deepEqual(offerFor({ kind: 'failed' }), { kind: 'clear' });
});

test('an answer without the results region clears the hits of the earlier query', () => {
  assert.deepEqual(offerFor({ kind: 'ok', regions: new Map() }), { kind: 'clear' });
  assert.deepEqual(offerFor({ kind: 'ok', regions: new Map([['other', REGION]]) }), { kind: 'clear' });
});

test('an answer that says the session ended ends it', () => {
  assert.deepEqual(offerFor({ kind: 'signed-out' }), { kind: 'signed-out' });
});

test('the hits listed are offered once they answer the query in the box', () => {
  assert.equal(offered(3, false), 3);
  assert.equal(offered(0, false), 0);
});

test('nothing is offered while the query in the box has not been answered', () => {
  assert.equal(offered(3, true), 0);
  assert.equal(offered(0, true), 0);
});

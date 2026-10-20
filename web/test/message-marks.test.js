import assert from 'node:assert/strict';
import { test } from 'node:test';

import { ackAction, offersAck, showsImportance, speaksFor } from '../src/message-marks.js';

test('only a raised importance gets a badge', () => {
  assert.equal(showsImportance('normal'), false);
  assert.equal(showsImportance('high'), true);
  assert.equal(showsImportance('urgent'), true);
});

test('the acknowledge button is offered for a message that asks, to anyone but its author', () => {
  assert.equal(offersAck({ ackRequired: true, author: 'alice', system: false }, 'bob', 'alice'), true);
  assert.equal(offersAck({ ackRequired: true, author: 'bob', system: false }, 'bob', 'alice'), false);
  assert.equal(offersAck({ ackRequired: false, author: 'alice', system: false }, 'bob', 'alice'), false);
});

test('what dibs wrote speaks for the member who started the thread, who is not asked', () => {
  const fromDibs = { ackRequired: true, author: 'dibs', system: true };
  assert.equal(speaksFor(fromDibs, 'alice'), 'alice');
  assert.equal(offersAck(fromDibs, 'alice', 'alice'), false);
  assert.equal(offersAck(fromDibs, 'bob', 'alice'), true);
  assert.equal(speaksFor(fromDibs, ''), '', 'in a thread dibs started it speaks for nobody');
  assert.equal(offersAck(fromDibs, 'dibs', ''), true, 'a member named dibs is still asked');
  assert.equal(speaksFor({ author: 'carol', system: false }, 'alice'), 'carol');
});

test('the form posts to the template with the message id put in', () => {
  assert.equal(ackAction('/ui/threads/4/messages/{message}/ack', 12), '/ui/threads/4/messages/12/ack');
  assert.equal(ackAction('/a/{message}/{message}', 3), '/a/3/3');
});

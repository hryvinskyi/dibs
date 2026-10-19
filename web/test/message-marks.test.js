import assert from 'node:assert/strict';
import { test } from 'node:test';

import { ackAction, offersAck, showsImportance } from '../src/message-marks.js';

test('only a raised importance gets a badge', () => {
  assert.equal(showsImportance('normal'), false);
  assert.equal(showsImportance('high'), true);
  assert.equal(showsImportance('urgent'), true);
});

test('the acknowledge button is offered for a message that asks, to anyone but its author', () => {
  assert.equal(offersAck({ ackRequired: true, author: 'alice' }, 'bob'), true);
  assert.equal(offersAck({ ackRequired: true, author: 'bob' }, 'bob'), false);
  assert.equal(offersAck({ ackRequired: false, author: 'alice' }, 'bob'), false);
});

test('the form posts to the template with the message id put in', () => {
  assert.equal(ackAction('/ui/threads/4/messages/{message}/ack', 12), '/ui/threads/4/messages/12/ack');
  assert.equal(ackAction('/a/{message}/{message}', 3), '/a/3/3');
});

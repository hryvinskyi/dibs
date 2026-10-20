import assert from 'node:assert/strict';
import { test } from 'node:test';

import { BOARD_FRAGMENT, onAnswer, whenDue } from '../src/board-refresh.js';

test('a refresh that comes due fetches only on a visible, idle page of a live session', () => {
  assert.equal(whenDue({ ended: false, hidden: false, typing: false }), 'fetch');
  assert.equal(whenDue({ ended: false, hidden: false, typing: true }), 'hold');
  assert.equal(whenDue({ ended: false, hidden: true, typing: false }), 'owe');
  assert.equal(whenDue({ ended: false, hidden: true, typing: true }), 'owe');
  assert.equal(whenDue({ ended: true, hidden: false, typing: false }), 'stop');
  assert.equal(whenDue({ ended: true, hidden: true, typing: true }), 'stop');
});

const fragment = { status: 200, contentType: 'text/html; charset=utf-8', fragment: BOARD_FRAGMENT };

test('only the board fragment is swapped in, and not while a form is in use', () => {
  assert.equal(onAnswer(fragment, false), 'swap');
  assert.equal(onAnswer({ ...fragment, contentType: 'TEXT/HTML' }, false), 'swap');
  assert.equal(onAnswer(fragment, true), 'hold');
});

test('an answer that is not the board fragment is never swapped in', () => {
  /** @type {import('../src/board-refresh.js').Answer[]} */
  const others = [
    { ...fragment, fragment: '' },
    { ...fragment, fragment: 'thread' },
    { ...fragment, contentType: 'application/json' },
    { ...fragment, contentType: 'text/html-but-not' },
    { ...fragment, contentType: '' },
    { ...fragment, status: 500 },
    { ...fragment, status: 302 },
    { ...fragment, status: 204 },
  ];
  for (const answer of others) {
    assert.equal(onAnswer(answer, false), 'failed', JSON.stringify(answer));
    assert.equal(onAnswer(answer, true), 'failed', JSON.stringify(answer));
  }
});

test('a 401 ends the session whatever else the answer carries', () => {
  assert.equal(onAnswer({ status: 401, contentType: 'application/json', fragment: '' }, false), 'ended');
  assert.equal(onAnswer({ ...fragment, status: 401 }, true), 'ended');
});

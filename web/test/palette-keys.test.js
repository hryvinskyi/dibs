import assert from 'node:assert/strict';
import { test } from 'node:test';

import { isMac, next } from '../src/palette-keys.js';

const NONE = { meta: false, ctrl: false };
const META = { meta: true, ctrl: false };
const CTRL = { meta: false, ctrl: true };

/** @param {Partial<{ open: boolean, typing: boolean, count: number, index: number }>} state */
const at = (state) => ({ open: false, typing: false, count: 0, index: 0, ...state });

test('a closed palette opens on the command key or Control with K', () => {
  assert.deepEqual(next(at({}), 'k', META), { action: 'open', index: 0 });
  assert.deepEqual(next(at({}), 'k', CTRL), { action: 'open', index: 0 });
  assert.deepEqual(next(at({}), 'K', CTRL), { action: 'open', index: 0 });
  assert.deepEqual(next(at({ typing: true }), 'k', META), { action: 'open', index: 0 }, 'even in a field');
});

test('a closed palette opens on a slash typed outside a field, and not inside one', () => {
  assert.deepEqual(next(at({}), '/', NONE), { action: 'open', index: 0 });
  assert.equal(next(at({ typing: true }), '/', NONE).action, 'none');
});

test('a closed palette ignores every other key', () => {
  for (const key of ['k', 'Escape', 'ArrowDown', 'ArrowUp', 'Enter', 'a']) {
    assert.equal(next(at({ count: 3 }), key, NONE).action, 'none', key);
  }
  assert.equal(next(at({ count: 3 }), 'Enter', META).action, 'none');
});

test('an open palette closes on the command key or Control with K, and on Escape', () => {
  assert.equal(next(at({ open: true, typing: true }), 'k', META).action, 'close');
  assert.equal(next(at({ open: true, typing: true }), 'k', CTRL).action, 'close');
  assert.equal(next(at({ open: true, typing: true }), 'Escape', NONE).action, 'close');
});

test('the arrows move the selection one hit, kept between the first and the last', () => {
  /** @param {number} index */
  const open = (index) => at({ open: true, typing: true, count: 3, index });
  assert.deepEqual(next(open(0), 'ArrowDown', NONE), { action: 'move', index: 1 });
  assert.deepEqual(next(open(1), 'ArrowDown', NONE), { action: 'move', index: 2 });
  assert.deepEqual(next(open(2), 'ArrowDown', NONE), { action: 'move', index: 2 });
  assert.deepEqual(next(open(2), 'ArrowUp', NONE), { action: 'move', index: 1 });
  assert.deepEqual(next(open(0), 'ArrowUp', NONE), { action: 'move', index: 0 });
});

test('the arrows do nothing without a hit', () => {
  assert.equal(next(at({ open: true, count: 0 }), 'ArrowDown', NONE).action, 'none');
  assert.equal(next(at({ open: true, count: 0 }), 'ArrowUp', NONE).action, 'none');
});

test('Enter opens the selected hit here, and with the command key or Control outside', () => {
  const open = at({ open: true, typing: true, count: 3, index: 2 });
  assert.deepEqual(next(open, 'Enter', NONE), { action: 'here', index: 2 });
  assert.deepEqual(next(open, 'Enter', META), { action: 'out', index: 2 });
  assert.deepEqual(next(open, 'Enter', CTRL), { action: 'out', index: 2 });
});

test('Enter without a hit is left alone, so the form submits', () => {
  assert.equal(next(at({ open: true, typing: true }), 'Enter', NONE).action, 'none');
  assert.equal(next(at({ open: true, typing: true }), 'Enter', META).action, 'none');
});

test('an open palette ignores the other keys, a slash included', () => {
  for (const key of ['a', '/', 'Tab', 'toString', 'constructor']) {
    assert.deepEqual(next(at({ open: true, count: 2, index: 1 }), key, NONE), { action: 'none', index: 1 }, key);
  }
});

test('a Mac is told by its platform name, whatever its case', () => {
  assert.equal(isMac('macOS'), true);
  assert.equal(isMac('MacIntel'), true);
  assert.equal(isMac('Windows'), false);
  assert.equal(isMac('Linux x86_64'), false);
  assert.equal(isMac(''), false);
});

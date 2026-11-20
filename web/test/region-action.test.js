import assert from 'node:assert/strict';
import { test } from 'node:test';

import { regionAction, selectChanged } from '../src/region-action.js';

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
 * Options rendered chosen as `defaults` and chosen now as `now`.
 * @param {readonly boolean[]} defaults
 * @param {readonly boolean[]} now
 */
const options = (defaults, now) => defaults.map((defaultSelected, at) => ({ defaultSelected, selected: now[at] ?? false }));

test('a drop-down with no option marked was rendered with its first one chosen', () => {
  const rendered = options([false, false, false], [true, false, false]);
  assert.equal(selectChanged({ multiple: false, size: 0, selectedIndex: 0, options: rendered }), false);
  assert.equal(selectChanged({ multiple: false, size: 0, selectedIndex: 2, options: rendered }), true);
});

test('a drop-down with several options marked was rendered with the last of them chosen', () => {
  const rendered = options([true, false, true], [false, false, true]);
  assert.equal(selectChanged({ multiple: false, size: 1, selectedIndex: 2, options: rendered }), false);
  assert.equal(selectChanged({ multiple: false, size: 1, selectedIndex: 0, options: rendered }), true);
});

test('an empty drop-down has changed only once something is chosen', () => {
  assert.equal(selectChanged({ multiple: false, size: 0, selectedIndex: -1, options: [] }), false);
});

test('a list box has changed when any option is chosen otherwise than rendered', () => {
  const same = options([true, false], [true, false]);
  const other = options([true, false], [true, true]);
  for (const box of [{ multiple: true, size: 0 }, { multiple: false, size: 4 }]) {
    assert.equal(selectChanged({ ...box, selectedIndex: 0, options: same }), false, JSON.stringify(box));
    assert.equal(selectChanged({ ...box, selectedIndex: 0, options: other }), true, JSON.stringify(box));
  }
});

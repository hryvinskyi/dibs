import assert from 'node:assert/strict';
import { test } from 'node:test';

import { toggled } from '../src/rail-menu.js';

test('a closed menu opens', () => {
  assert.equal(toggled('false'), 'true');
});

test('an open menu closes', () => {
  assert.equal(toggled('true'), 'false');
});

test('a menu that does not say whether it is open counts as closed, so it opens', () => {
  assert.equal(toggled(null), 'true');
  assert.equal(toggled(''), 'true');
});

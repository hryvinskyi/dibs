import assert from 'node:assert/strict';
import { test } from 'node:test';

import { whenDue } from '../src/refresh-timing.js';

test('a refresh that comes due fetches only on a visible page of a live session', () => {
  assert.equal(whenDue({ ended: false, hidden: false }), 'fetch');
  assert.equal(whenDue({ ended: false, hidden: true }), 'owe');
  assert.equal(whenDue({ ended: true, hidden: false }), 'stop');
  assert.equal(whenDue({ ended: true, hidden: true }), 'stop');
});

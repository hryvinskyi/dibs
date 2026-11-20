import assert from 'node:assert/strict';
import { test } from 'node:test';

import { focusIndex } from '../src/focus-return.js';

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

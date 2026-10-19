import assert from 'node:assert/strict';
import { test } from 'node:test';

import { FIRST_DELAY_MS, MAX_DELAY_MS, reconnectDelay } from '../src/reconnect.js';

test('the wait doubles from the first delay and stays within the upper half of its step', () => {
  assert.equal(reconnectDelay(0, () => 0), FIRST_DELAY_MS / 2);
  assert.equal(reconnectDelay(0, () => 0.999_999), FIRST_DELAY_MS);
  assert.equal(reconnectDelay(1, () => 0), FIRST_DELAY_MS);
  assert.equal(reconnectDelay(2, () => 0.5), 3000);
});

test('the wait never exceeds the cap, whatever the attempt or the random number', () => {
  for (const attempt of [5, 6, 30, 1000, Number.MAX_SAFE_INTEGER]) {
    for (const random of [0, 0.5, 0.999, 2, -1]) {
      const delay = reconnectDelay(attempt, () => random);
      assert.ok(delay >= MAX_DELAY_MS / 2 && delay <= MAX_DELAY_MS, `${attempt} ${random} ${delay}`);
    }
  }
  assert.equal(reconnectDelay(-3, () => 0), FIRST_DELAY_MS / 2);
});

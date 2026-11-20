import assert from 'node:assert/strict';
import { test } from 'node:test';

import { streamListeners } from '../src/stream-listeners.js';

test('every listener subscribed is called until it unsubscribes', () => {
  const listeners = streamListeners(() => assert.fail('no listener throws'));
  /** @type {string[]} */
  const calls = [];
  const leave = listeners.subscribe({ onResync: () => calls.push('first') });
  listeners.subscribe({ onResync: () => calls.push('second') });
  listeners.each((listener) => listener.onResync?.());
  leave();
  listeners.each((listener) => listener.onResync?.());
  assert.deepEqual(calls, ['first', 'second', 'second']);
});

test('a listener that throws is reported and does not keep the others from their call', () => {
  /** @type {unknown[]} */
  const reported = [];
  const listeners = streamListeners((error) => reported.push(error));
  const broken = new Error('broken listener');
  let reached = false;
  listeners.subscribe({
    onState: () => {
      throw broken;
    },
  });
  listeners.subscribe({
    onState: () => {
      reached = true;
    },
  });
  listeners.each((listener) => listener.onState?.('live'));
  assert.equal(reached, true);
  assert.deepEqual(reported, [broken]);
});

test('a listener subscribing while the others are called waits for the next call', () => {
  const listeners = streamListeners(() => assert.fail('no listener throws'));
  let late = 0;
  listeners.subscribe({
    onResync: () => {
      listeners.subscribe({
        onResync: () => {
          late += 1;
        },
      });
    },
  });
  listeners.each((listener) => listener.onResync?.());
  assert.equal(late, 0);
});

test('cleared listeners are called no more', () => {
  const listeners = streamListeners(() => assert.fail('no listener throws'));
  let calls = 0;
  listeners.subscribe({
    onResync: () => {
      calls += 1;
    },
  });
  listeners.clear();
  listeners.each((listener) => listener.onResync?.());
  assert.equal(calls, 0);
});

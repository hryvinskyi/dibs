import assert from 'node:assert/strict';
import { test } from 'node:test';

import { coalesce } from '../src/coalesce.js';

/** A timer driven by hand: `elapse` runs what came due. */
function manualTimer() {
  let now = 0;
  let next = 1;
  /** @type {Map<number, { at: number, run: () => void }>} */
  const scheduled = new Map();
  return {
    /** @type {import('../src/coalesce.js').Timer<number>} */
    timer: {
      set(run, ms) {
        const handle = next;
        next += 1;
        scheduled.set(handle, { at: now + ms, run });
        return handle;
      },
      clear(handle) {
        scheduled.delete(handle);
      },
    },
    /** @param {number} ms */
    elapse(ms) {
      now += ms;
      for (const [handle, entry] of [...scheduled]) {
        if (entry.at <= now) {
          scheduled.delete(handle);
          entry.run();
        }
      }
    },
    waiting: () => scheduled.size,
  };
}

test('a burst of requests inside one window runs once, when the window closes', () => {
  const clock = manualTimer();
  let runs = 0;
  const refreshes = coalesce({ windowMs: 500, timer: clock.timer, run: () => { runs += 1; } });
  refreshes.request();
  clock.elapse(200);
  refreshes.request();
  clock.elapse(200);
  refreshes.request();
  assert.equal(runs, 0);
  assert.equal(clock.waiting(), 1, 'one timer for the whole burst');
  assert.equal(refreshes.pending(), true);
  clock.elapse(100);
  assert.equal(runs, 1);
  assert.equal(refreshes.pending(), false);
  clock.elapse(5000);
  assert.equal(runs, 1, 'nothing runs without a request');
});

test('a window is counted from its first request, so a steady stream still runs every window', () => {
  const clock = manualTimer();
  let runs = 0;
  const refreshes = coalesce({ windowMs: 500, timer: clock.timer, run: () => { runs += 1; } });
  for (let step = 0; step < 20; step += 1) {
    refreshes.request();
    clock.elapse(100);
  }
  assert.equal(runs, 4);
});

test('a request after a run opens a new window', () => {
  const clock = manualTimer();
  let runs = 0;
  const refreshes = coalesce({ windowMs: 500, timer: clock.timer, run: () => { runs += 1; } });
  refreshes.request();
  clock.elapse(500);
  refreshes.request();
  clock.elapse(499);
  assert.equal(runs, 1);
  clock.elapse(1);
  assert.equal(runs, 2);
});

test('cancelling drops the waiting run and clears its timer', () => {
  const clock = manualTimer();
  let runs = 0;
  const refreshes = coalesce({ windowMs: 500, timer: clock.timer, run: () => { runs += 1; } });
  refreshes.cancel();
  refreshes.request();
  refreshes.cancel();
  assert.equal(clock.waiting(), 0);
  assert.equal(refreshes.pending(), false);
  clock.elapse(1000);
  assert.equal(runs, 0);
  refreshes.request();
  clock.elapse(500);
  assert.equal(runs, 1, 'a cancelled coalescer still takes requests');
});

test('a run that requests again opens the next window instead of running at once', () => {
  const clock = manualTimer();
  let runs = 0;
  /** @type {import('../src/coalesce.js').Coalescer | null} */
  let self = null;
  const refreshes = coalesce({
    windowMs: 500,
    timer: clock.timer,
    run: () => {
      runs += 1;
      if (runs === 1) {
        self?.request();
      }
    },
  });
  self = refreshes;
  refreshes.request();
  clock.elapse(500);
  assert.equal(runs, 1);
  assert.equal(refreshes.pending(), true);
  clock.elapse(500);
  assert.equal(runs, 2);
});

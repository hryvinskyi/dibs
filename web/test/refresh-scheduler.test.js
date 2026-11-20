import assert from 'node:assert/strict';
import { test } from 'node:test';

import { scheduleRefreshes } from '../src/refresh-scheduler.js';

const WINDOW_MS = 500;

/** Lets every settled promise run its handlers. */
const settle = () => new Promise((resolve) => setImmediate(resolve));

/**
 * A scheduler over a timer driven by hand and a page whose state the test sets, with refreshes
 * that finish only when the test says so.
 */
function harness() {
  /** @type {Map<number, () => void>} */
  const scheduled = new Map();
  let next = 1;
  const page = { ended: false, hidden: false };
  /** @type {(() => void)[]} */
  const running = [];
  /** @type {unknown[]} */
  const failures = [];
  let runs = 0;
  let failNext = false;
  const scheduler = scheduleRefreshes({
    windowMs: WINDOW_MS,
    timer: {
      set(run) {
        const handle = next;
        next += 1;
        scheduled.set(handle, run);
        return handle;
      },
      clear(handle) {
        scheduled.delete(handle);
      },
    },
    state: () => ({ ...page }),
    run: () => {
      runs += 1;
      if (failNext) {
        failNext = false;
        return Promise.reject(new Error('refresh failed'));
      }
      return new Promise((resolve) => {
        running.push(() => resolve(undefined));
      });
    },
    onError: (error) => failures.push(error),
  });
  return {
    scheduler,
    page,
    failures,
    runs: () => runs,
    waiting: () => scheduled.size,
    /** Closes the window open, if any. */
    closeWindow() {
      for (const [handle, run] of [...scheduled]) {
        scheduled.delete(handle);
        run();
      }
    },
    /** Finishes the refresh running. */
    async finish() {
      running.shift()?.();
      await settle();
    },
    failNext() {
      failNext = true;
    },
  };
}

test('the requests of one burst make one refresh, once its window closes', () => {
  const h = harness();
  h.scheduler.request();
  h.scheduler.request();
  h.scheduler.request();
  assert.equal(h.runs(), 0);
  assert.equal(h.waiting(), 1);
  h.closeWindow();
  assert.equal(h.runs(), 1);
});

test('one refresh runs at a time, and a request made meanwhile makes one more after it', async () => {
  const h = harness();
  h.scheduler.request();
  h.closeWindow();
  h.scheduler.request();
  h.closeWindow();
  assert.equal(h.runs(), 1, 'the second waits for the first');
  await h.finish();
  assert.equal(h.waiting(), 1, 'one more is asked for');
  h.closeWindow();
  assert.equal(h.runs(), 2);
  await h.finish();
  assert.equal(h.waiting(), 0, 'and no other');
});

test('a refresh due while the tab is hidden is owed until it shows again', () => {
  const h = harness();
  h.page.hidden = true;
  h.scheduler.request();
  h.closeWindow();
  assert.equal(h.runs(), 0);
  h.scheduler.shown();
  assert.equal(h.waiting(), 0, 'still hidden');
  h.page.hidden = false;
  h.scheduler.shown();
  h.closeWindow();
  assert.equal(h.runs(), 1);
  h.scheduler.shown();
  assert.equal(h.waiting(), 0, 'owed once');
});

test('nothing is owed when the tab shows without a refresh coming due', () => {
  const h = harness();
  h.scheduler.shown();
  assert.equal(h.waiting(), 0);
});

test('nothing runs once the session has ended', () => {
  const h = harness();
  h.scheduler.request();
  h.page.ended = true;
  h.closeWindow();
  assert.equal(h.runs(), 0);
  h.page.hidden = true;
  h.scheduler.request();
  h.closeWindow();
  h.page.hidden = false;
  h.scheduler.shown();
  assert.equal(h.waiting(), 0);
});

test('a stopped scheduler drops the refresh waiting and asks for none any more', async () => {
  const h = harness();
  h.scheduler.request();
  h.scheduler.stop();
  assert.equal(h.waiting(), 0);
  h.scheduler.request();
  assert.equal(h.waiting(), 0);
  assert.equal(h.runs(), 0);
});

test('a refresh asked for while one runs is not asked for once the scheduler stopped', async () => {
  const h = harness();
  h.scheduler.request();
  h.closeWindow();
  h.scheduler.request();
  h.closeWindow();
  h.scheduler.stop();
  await h.finish();
  assert.equal(h.waiting(), 0);
});

test('a refresh that fails is reported, and the next one still runs', async () => {
  const h = harness();
  h.failNext();
  h.scheduler.request();
  h.closeWindow();
  await settle();
  assert.equal(h.failures.length, 1);
  h.scheduler.request();
  h.closeWindow();
  assert.equal(h.runs(), 2);
});

import assert from 'node:assert/strict';
import { test } from 'node:test';

import { catchingUp, createBacklog, createCursor } from '../src/stream-catch-up.js';

/**
 * An event of the log with no message and no acknowledgement.
 * @param {number} id
 * @returns {import('../src/events.js').StreamEvent}
 */
const event = (id) => ({ id, kind: 'claimed', message: null, ack: null });

/**
 * A stream driven by hand: what `deliver`, `resync` and `state` send reaches every subscriber.
 */
function handStream() {
  /** @type {Set<import('../src/stream-catch-up.js').StreamListener>} */
  const listeners = new Set();
  return {
    /** @type {import('../src/stream-catch-up.js').Subscribe} */
    subscribe: (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    /** @param {number} id */
    deliver: (id) => [...listeners].forEach((listener) => listener.onEvent?.(event(id))),
    /** @param {number} [from] */
    resync: (from) => [...listeners].forEach((listener) => listener.onResync?.(from)),
    /** @param {import('../src/live-stream.js').StreamState} state */
    state: (state) => [...listeners].forEach((listener) => listener.onState?.(state)),
    count: () => listeners.size,
  };
}

/**
 * A subscriber that writes down what it is handed.
 */
function recorder() {
  /** @type {number[]} */
  const applied = [];
  /** @type {string[]} */
  const states = [];
  let resyncs = 0;
  return {
    applied,
    states,
    resyncs: () => resyncs,
    /** @type {import('../src/stream-catch-up.js').StreamListener} */
    listener: {
      onEvent: (received) => applied.push(received.id),
      onState: (state) => states.push(state),
      onResync: () => {
        resyncs += 1;
      },
    },
  };
}

test('a cursor applies the events after it, not those before or at it', () => {
  const admits = createCursor(10);
  assert.equal(admits(9), false);
  assert.equal(admits(10), false);
  assert.equal(admits(11), true);
});

test('a cursor applies a duplicate once', () => {
  const admits = createCursor(10);
  assert.equal(admits(12), true);
  assert.equal(admits(12), false);
  assert.equal(admits(13), true);
});

test('the backlog hands a cursor the events delivered after it, oldest first', () => {
  const backlog = createBacklog(8);
  [9, 10, 11, 12].map(event).forEach(backlog.record);
  const missed = backlog.since(10);
  assert.equal(missed.kind, 'events');
  assert.deepEqual(missed.kind === 'events' ? missed.events.map((e) => e.id) : null, [11, 12]);
});

test('the backlog tells a cursor older than what it remembers that it has a hole', () => {
  const backlog = createBacklog(2);
  [9, 10, 11].map(event).forEach(backlog.record);
  assert.deepEqual(backlog.since(8), { kind: 'gap' });
  assert.equal(backlog.since(9).kind, 'events');
});

test('after the stream skipped events, a cursor before the next event delivered has a hole', () => {
  const backlog = createBacklog(8);
  [3, 4].map(event).forEach(backlog.record);
  backlog.lose();
  assert.deepEqual(backlog.since(4), { kind: 'gap' });
  backlog.record(event(20));
  assert.deepEqual(backlog.since(4), { kind: 'gap' });
  const missed = backlog.since(19);
  assert.deepEqual(missed.kind === 'events' ? missed.events.map((e) => e.id) : null, [20]);
});

test('after the stream resumed from a cursor, a cursor at or after it has no hole and one before it has', () => {
  const backlog = createBacklog(8);
  [3, 4].map(event).forEach(backlog.record);
  backlog.lose(30);
  assert.deepEqual(backlog.since(30), { kind: 'events', events: [] });
  assert.deepEqual(backlog.since(31), { kind: 'events', events: [] });
  assert.deepEqual(backlog.since(29), { kind: 'gap' });
  assert.deepEqual(backlog.since(4), { kind: 'gap' });
  backlog.record(event(31));
  const missed = backlog.since(30);
  assert.deepEqual(missed.kind === 'events' ? missed.events.map((e) => e.id) : null, [31]);
  assert.deepEqual(backlog.since(29), { kind: 'gap' });
});

test('a resume cursor older than what the memory already forgot does not vouch for it', () => {
  const backlog = createBacklog(2);
  [9, 10, 11].map(event).forEach(backlog.record);
  backlog.lose(5);
  assert.deepEqual(backlog.since(8), { kind: 'gap' });
  assert.equal(backlog.since(9).kind, 'events');
});

test('a view rendered after a resync with no event since is handed nothing and reads nothing again', () => {
  const stream = handStream();
  const subscribe = catchingUp(stream.subscribe, 64);
  [40, 41].forEach(stream.deliver);
  stream.resync(90);

  const fresh = recorder();
  subscribe(fresh.listener, 90);
  assert.equal(fresh.resyncs(), 0);
  assert.deepEqual(fresh.applied, []);
  stream.deliver(91);
  assert.deepEqual(fresh.applied, [91]);

  const older = recorder();
  subscribe(older.listener, 41);
  assert.equal(older.resyncs(), 1);
  assert.deepEqual(older.applied, []);
});

test('a view mounted late is handed what the stream delivered after its cursor, then what it delivers later', () => {
  const stream = handStream();
  const subscribe = catchingUp(stream.subscribe, 64);
  [40, 41, 42, 43].forEach(stream.deliver);

  const view = recorder();
  subscribe(view.listener, 41);
  assert.deepEqual(view.applied, [42, 43]);

  stream.deliver(44);
  assert.deepEqual(view.applied, [42, 43, 44]);
});

test('an event delivered again after a view caught up on it is applied once', () => {
  const stream = handStream();
  const subscribe = catchingUp(stream.subscribe, 64);
  stream.deliver(42);

  const view = recorder();
  subscribe(view.listener, 41);
  stream.deliver(42);
  stream.deliver(43);
  stream.deliver(43);
  assert.deepEqual(view.applied, [42, 43]);
});

test('a view rendered after events the stream has not delivered yet waits for them', () => {
  const stream = handStream();
  const subscribe = catchingUp(stream.subscribe, 64);
  stream.deliver(40);

  const view = recorder();
  subscribe(view.listener, 45);
  assert.deepEqual(view.applied, []);
  [44, 45, 46].forEach(stream.deliver);
  assert.deepEqual(view.applied, [46]);
});

test('a view whose cursor the memory no longer reaches back to reads its state again', () => {
  const stream = handStream();
  const subscribe = catchingUp(stream.subscribe, 2);
  [40, 41, 42, 43].forEach(stream.deliver);

  const view = recorder();
  subscribe(view.listener, 40);
  assert.equal(view.resyncs(), 1);
  assert.deepEqual(view.applied, []);
});

test("a view is handed the stream's states and resyncs, and nothing once it unsubscribed", () => {
  const stream = handStream();
  const subscribe = catchingUp(stream.subscribe, 64);
  const view = recorder();
  const unsubscribe = subscribe(view.listener, 0);
  stream.state('reconnecting');
  stream.resync();
  assert.deepEqual(view.states, ['reconnecting']);
  assert.equal(view.resyncs(), 1);

  unsubscribe();
  stream.deliver(5);
  stream.state('live');
  assert.deepEqual(view.applied, []);
  assert.deepEqual(view.states, ['reconnecting']);
  assert.equal(stream.count(), 1);
});

test('a subscribed view is told to read again only when the stream resumed after its cursor', () => {
  const stream = handStream();
  const subscribe = catchingUp(stream.subscribe, 64);
  [40, 41].forEach(stream.deliver);
  const current = recorder();
  subscribe(current.listener, 41);
  const behind = recorder();
  subscribe(behind.listener, 30);

  stream.resync(41);
  assert.equal(current.resyncs(), 0);
  assert.equal(behind.resyncs(), 1);

  stream.resync(60);
  assert.equal(current.resyncs(), 1);
  assert.equal(behind.resyncs(), 2);
});

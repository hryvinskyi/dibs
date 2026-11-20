// @ts-check
/**
 * Following the page's one event stream from a cursor. A view rendered by the server shows every
 * event up to the cursor its markup carries (`data-after`), but it may be mounted only after the
 * stream has delivered later ones. So the newest events the stream delivered are remembered, and a
 * view that subscribes is handed first those after its cursor, then each later one after it — each
 * event id once. When the memory no longer reaches back to a view's cursor, the view is told to
 * read its state again instead of being handed a stream with a hole in it. Once the stream skipped
 * events and resumed from a cursor, a view rendered at or after that cursor has missed nothing,
 * and one rendered before it has.
 */

import { createSeen } from './events.js';

/** How many event ids a subscriber remembers against duplicates. */
const SEEN_LIMIT = 512;

/**
 * The callbacks a subscriber of the stream gives, each optional.
 * @typedef {object} StreamListener
 * @property {(event: import('./events.js').StreamEvent) => void} [onEvent]
 * @property {(state: import('./live-stream.js').StreamState) => void} [onState]
 * @property {(from?: number) => void} [onResync] called when the stream skipped events the log no
 *   longer holds, with the cursor it resumed from when the stream knows it
 */

/**
 * Hands the stream to `listener` until the returned function is called.
 * @typedef {(listener: StreamListener) => () => void} Subscribe
 */

/**
 * Hands the stream to `listener`, from the event after `after` on, until the returned function is
 * called.
 * @typedef {(listener: StreamListener, after: number) => () => void} SubscribeAfter
 */

/**
 * What the memory holds after a cursor: the events delivered after it, oldest first, or `gap` when
 * events after it may have been delivered and forgotten, or never delivered at all.
 * @typedef {{ kind: 'events', events: import('./events.js').StreamEvent[] } | { kind: 'gap' }} Missed
 */

/**
 * @typedef {object} Backlog
 * @property {(event: import('./events.js').StreamEvent) => void} record remembers an event the
 *   stream delivered
 * @property {(from?: number) => void} lose notes that the stream skipped events the log no longer
 *   holds and, when `from` is given, resumed from that cursor
 * @property {(after: number) => Missed} since what a view rendered at `after` has missed
 */

/**
 * The newest `limit` events a stream delivered.
 * @param {number} limit
 * @returns {Backlog}
 */
export function createBacklog(limit) {
  const bound = Math.max(1, Math.floor(limit));
  /** @type {import('./events.js').StreamEvent[]} */
  const events = [];
  /** A cursor below it may have missed an event the memory does not hold. */
  let floor = 0;
  /** Whether the stream skipped events, did not say where it resumed, and has delivered none since. */
  let skipped = false;
  return {
    record(event) {
      if (skipped) {
        floor = Math.max(floor, event.id - 1);
        skipped = false;
      }
      events.push(event);
      if (events.length > bound) {
        const forgotten = events.shift();
        if (forgotten !== undefined) {
          floor = Math.max(floor, forgotten.id);
        }
      }
    },
    lose(from) {
      if (from === undefined) {
        skipped = true;
        return;
      }
      floor = Math.max(floor, from);
      skipped = false;
    },
    since(after) {
      if (skipped || after < floor) {
        return { kind: 'gap' };
      }
      return { kind: 'events', events: events.filter((event) => event.id > after) };
    },
  };
}

/**
 * Which events a view rendered at `after` applies: those after its cursor, each id once.
 * @param {number} after
 * @returns {(eventId: number) => boolean} answers whether an event is to be applied
 */
export function createCursor(after) {
  const seen = createSeen(SEEN_LIMIT);
  return (eventId) => eventId > after && seen.add(eventId);
}

/**
 * Calls one callback of a listener; one that throws is logged and does not stop the others.
 * @param {() => void} call
 * @returns {void}
 */
function guarded(call) {
  try {
    call();
  } catch (error) {
    console.error('dibs: a listener of the stream failed', error);
  }
}

/**
 * Remembers the newest `limit` events `source` delivers from now on, and answers a subscription
 * from a cursor over it.
 * @param {Subscribe} source
 * @param {number} limit
 * @returns {SubscribeAfter}
 */
export function catchingUp(source, limit) {
  const backlog = createBacklog(limit);
  source({ onEvent: (event) => backlog.record(event), onResync: (from) => backlog.lose(from) });
  return (listener, after) => {
    const admits = createCursor(after);
    const unsubscribe = source({
      onEvent: (event) => {
        if (admits(event.id)) {
          listener.onEvent?.(event);
        }
      },
      onState: (state) => listener.onState?.(state),
      onResync: (from) => {
        if (from === undefined || after < from) {
          listener.onResync?.();
        }
      },
    });
    const missed = backlog.since(after);
    if (missed.kind === 'gap') {
      guarded(() => listener.onResync?.());
      return unsubscribe;
    }
    for (const event of missed.events) {
      if (admits(event.id)) {
        guarded(() => listener.onEvent?.(event));
      }
    }
    return unsubscribe;
  };
}

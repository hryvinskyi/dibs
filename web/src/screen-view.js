// @ts-check
/**
 * A live screen: while events of the kinds the screen names arrive, the page at the address shown
 * is fetched again and its regions (the rail, the list, the inspector) are applied, region by
 * region. The screen follows the page's one event stream and hands every event, state and resync
 * to its subscribers too. A screen that follows no change answers a form: it never asks for the
 * address shown, which only a POST answers, and follows the stream only for the session's end.
 *
 * Each job lives in a module of its own, wired here: the configuration (`screen-config.js`), which
 * changes ask for a refresh and which address a refresh and the change notice use
 * (`change-following.js`), when a refresh runs (`refresh-scheduler.js`), how an answer's regions
 * are applied and held (`region-apply.js`), how a region is swapped (`region-swap.js`), the
 * notices (`page-notices.js`) and the stream's listeners (`stream-listeners.js`). An answer for an
 * address the page no longer shows is dropped and asked for again, and an in-place navigation
 * aborts the refresh in flight. When the session is gone, the page's session-ended notice shows
 * and nothing is fetched any more.
 */

import { answerStillWanted } from './answer-address.js';
import { asksItsAddress, isRelevant, reloadAddress } from './change-following.js';
import { follow } from './live-stream.js';
import { pageNotices } from './page-notices.js';
import { scheduleRefreshes } from './refresh-scheduler.js';
import { regionApplier } from './region-apply.js';
import { swapKeepingPlace } from './region-swap.js';
import { findRegion } from './regions.js';
import { parseScreenConfig } from './screen-config.js';
import { streamListeners } from './stream-listeners.js';

export { answerStillWanted };

/** How long the events of one burst are gathered before one refresh, in milliseconds. */
const BURST_MS = 500;

/** The regions a refresh asks for, those the page has. */
const SCREEN_REGIONS = Object.freeze(['rail', 'list', 'inspector']);

/** @typedef {import('./stream-listeners.js').StreamListener} StreamListener */

/**
 * What a live screen is given.
 * @typedef {object} ScreenDeps
 * @property {import('./regions.js').FetchRegions} fetchRegions
 * @property {Pick<Location, 'href'>} location
 * @property {(region: Element) => void} onSwap called with each region swapped in
 */

/**
 * @typedef {object} Screen
 * @property {() => void} teardown
 * @property {() => void} onNavigate drops the refresh in flight; called when the page navigates in
 *   place
 * @property {(listener: StreamListener) => () => void} subscribe hands the stream to `listener`
 *   until the returned function is called
 */

/** @type {WeakSet<Element>} */
const mounted = new WeakSet();

/** @returns {Screen} */
function inert() {
  return { teardown: () => {}, onNavigate: () => {}, subscribe: () => () => {} };
}

/**
 * Keeps the screen's regions current while events arrive. Mounting the same element again
 * answers a screen that does nothing.
 * @param {Element} app the `.app` element
 * @param {ScreenDeps} deps
 * @returns {Screen}
 */
export function mountScreen(app, deps) {
  if (mounted.has(app) || !(app instanceof HTMLElement)) {
    return inert();
  }
  /** @type {import('./screen-config.js').ScreenConfig} */
  let config;
  try {
    config = parseScreenConfig(app.dataset);
  } catch (error) {
    console.error('dibs: the screen is missing its live configuration', error);
    return inert();
  }
  mounted.add(app);
  const { fetchRegions, location, onSwap } = deps;
  const { kinds } = config;
  const refreshing = asksItsAddress(kinds);
  const doc = app.ownerDocument;
  const controller = new AbortController();
  const { signal } = controller;
  const stream = new AbortController();
  const notices = pageNotices(doc);
  const listeners = streamListeners((error) => {
    console.error('dibs: a listener of the stream failed', error);
  });
  let ended = false;
  /** @type {AbortController | null} */
  let inflight = null;

  const showChanged = () => notices.showChanged(reloadAddress(kinds, location.href));

  const refreshes = scheduleRefreshes({
    windowMs: BURST_MS,
    timer: {
      set: (run, ms) => setTimeout(run, ms),
      clear: (handle) => clearTimeout(handle),
    },
    state: () => ({ ended, hidden: doc.hidden }),
    run: () => refresh(),
    onError: (error) => {
      console.error('dibs: the screen could not be refreshed', error);
    },
  });

  const end = () => {
    if (ended) {
      return;
    }
    ended = true;
    refreshes.stop();
    inflight?.abort();
    stream.abort();
    notices.showEnded();
    listeners.each((listener) => listener.onState?.('ended'));
  };

  const regions = regionApplier(app, {
    swap: (current, incoming) => onSwap(swapKeepingPlace(current, incoming)),
    onReleased: () => refreshes.request(),
    signal,
  });

  /**
   * What an answer without regions leads to.
   * @type {{ [K in Exclude<import('./regions.js').RegionsAnswer['kind'], 'ok'>]: () => void }}
   */
  const unanswered = {
    'signed-out': end,
    failed: showChanged,
  };

  const refresh = async () => {
    const names = SCREEN_REGIONS.filter((name) => findRegion(app, name) !== null);
    if (!refreshing || names.length === 0) {
      return;
    }
    const fetchedFor = location.href;
    const mine = new AbortController();
    inflight = mine;
    /** @type {import('./regions.js').RegionsAnswer} */
    let answer;
    try {
      answer = await fetchRegions(fetchedFor, names, { signal: AbortSignal.any([signal, mine.signal]) });
    } catch (error) {
      if (mine.signal.aborted || signal.aborted) {
        return;
      }
      console.error('dibs: the screen could not be refreshed', error);
      showChanged();
      return;
    } finally {
      if (inflight === mine) {
        inflight = null;
      }
    }
    if (ended || mine.signal.aborted || signal.aborted) {
      return;
    }
    if (!answerStillWanted(fetchedFor, location.href)) {
      refreshes.request();
      return;
    }
    if (answer.kind === 'ok') {
      if (regions.apply(answer.regions, names)) {
        showChanged();
      } else {
        notices.hideChanged();
      }
      return;
    }
    unanswered[answer.kind]();
  };

  doc.addEventListener('visibilitychange', () => {
    if (!doc.hidden) {
      refreshes.shown();
    }
  }, { signal });

  follow({
    config: config.stream,
    signal: AbortSignal.any([signal, stream.signal]),
    onEvent: (event) => {
      if (isRelevant(event, kinds)) {
        refreshes.request();
      }
      listeners.each((listener) => listener.onEvent?.(event));
    },
    onState: (state) => {
      if (state === 'ended') {
        end();
        return;
      }
      listeners.each((listener) => listener.onState?.(state));
    },
    onResync: (from) => {
      if (refreshing) {
        refreshes.request();
      }
      listeners.each((listener) => listener.onResync?.(from));
    },
  });

  return {
    teardown: () => {
      controller.abort();
      refreshes.stop();
      inflight?.abort();
      listeners.clear();
      mounted.delete(app);
    },
    onNavigate: () => {
      if (inflight !== null) {
        inflight.abort();
        refreshes.request();
      }
    },
    subscribe: listeners.subscribe,
  };
}

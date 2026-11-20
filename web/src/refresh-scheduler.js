// @ts-check
/**
 * When a live page fetches what it shows again: the requests of one burst make one refresh, once
 * the burst's window closes; one refresh runs at a time, and a request made while one runs makes
 * one more after it; a refresh that comes due while the tab is hidden is owed until it shows again,
 * and none runs once the session has ended or the scheduler is stopped. The timer and the page's
 * state are parameters, so a test drives it without a page or a clock.
 */

import { coalesce } from './coalesce.js';
import { whenDue } from './refresh-timing.js';

/**
 * @template H
 * @typedef {object} SchedulerOptions
 * @property {number} windowMs how long the requests of one burst are gathered, in milliseconds
 * @property {import('./coalesce.js').Timer<H>} timer
 * @property {() => import('./refresh-timing.js').PageState} state the page's state now
 * @property {() => Promise<void>} run one refresh
 * @property {(error: unknown) => void} onError told of a refresh that failed
 */

/**
 * @typedef {object} RefreshScheduler
 * @property {() => void} request asks for a refresh
 * @property {() => void} shown tells that the tab shows again: a refresh owed meanwhile is asked
 *   for
 * @property {() => void} stop drops the refresh waiting; none is asked for any more
 */

/**
 * A scheduler of the refreshes `run` makes.
 * @template H
 * @param {SchedulerOptions<H>} options
 * @returns {RefreshScheduler}
 */
export function scheduleRefreshes({ windowMs, timer, state, run, onError }) {
  let stopped = false;
  let running = false;
  let again = false;
  let owed = false;

  const request = () => {
    if (!stopped) {
      windows.request();
    }
  };

  const start = () => {
    if (running) {
      again = true;
      return;
    }
    running = true;
    run()
      .catch(onError)
      .finally(() => {
        running = false;
        if (again) {
          again = false;
          request();
        }
      });
  };

  /** @type {{ [K in import('./refresh-timing.js').DueAction]: () => void }} */
  const due = {
    fetch: start,
    owe: () => {
      owed = true;
    },
    stop: () => {},
  };

  const windows = coalesce({ windowMs, timer, run: () => due[whenDue(state())]() });

  return {
    request,
    shown() {
      const { ended, hidden } = state();
      if (owed && !ended && !hidden) {
        owed = false;
        request();
      }
    },
    stop() {
      stopped = true;
      windows.cancel();
    },
  };
}

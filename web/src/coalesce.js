// @ts-check
/**
 * Many requests in a short window turned into one run: the first request opens a window of a fixed
 * length, the requests made while it is open join it, and the run happens once when it closes. The
 * timer is a parameter, so the module needs no clock of its own and a test drives it by hand.
 */

/**
 * The timer a window waits on: `set` schedules `run` after `ms` milliseconds and answers a handle,
 * `clear` cancels the run of that handle.
 * @template H
 * @typedef {object} Timer
 * @property {(run: () => void, ms: number) => H} set
 * @property {(handle: H) => void} clear
 */

/**
 * @typedef {object} Coalescer
 * @property {() => void} request asks for a run; one already waiting absorbs it
 * @property {() => void} cancel drops the run waiting, if any
 * @property {() => boolean} pending whether a run is waiting
 */

/**
 * @template H
 * @typedef {object} CoalesceOptions
 * @property {number} windowMs how long a window stays open, in milliseconds
 * @property {Timer<H>} timer
 * @property {() => void} run what runs once per window
 */

/**
 * A coalescer over `timer`.
 * @template H
 * @param {CoalesceOptions<H>} options
 * @returns {Coalescer}
 */
export function coalesce({ windowMs, timer, run }) {
  /** @type {{ handle: H } | null} */
  let waiting = null;
  const fire = () => {
    waiting = null;
    run();
  };
  return {
    request() {
      if (waiting === null) {
        waiting = { handle: timer.set(fire, Math.max(0, windowMs)) };
      }
    },
    cancel() {
      if (waiting !== null) {
        timer.clear(waiting.handle);
        waiting = null;
      }
    },
    pending() {
      return waiting !== null;
    },
  };
}

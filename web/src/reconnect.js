// @ts-check
/** How long to wait before opening a stream again after it was refused. */

/** The first wait, in milliseconds. */
export const FIRST_DELAY_MS = 1000;

/** The longest wait, in milliseconds. */
export const MAX_DELAY_MS = 30_000;

/**
 * The wait before attempt `attempt` (0 for the first retry): doubling from `FIRST_DELAY_MS` up to
 * `MAX_DELAY_MS`, then spread over its upper half so that many pages do not come back together.
 * @param {number} attempt
 * @param {() => number} random a number in [0, 1)
 * @returns {number} milliseconds
 */
export function reconnectDelay(attempt, random) {
  const step = Math.max(0, Math.min(Math.floor(attempt), 30));
  const ceiling = Math.min(MAX_DELAY_MS, FIRST_DELAY_MS * 2 ** step);
  const spread = Math.min(Math.max(random(), 0), 1);
  return Math.round(ceiling / 2 + (spread * ceiling) / 2);
}

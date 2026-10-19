// @ts-check
/** The wall-clock text of a moment in the viewer's own time zone. */

/**
 * @param {number} value
 * @returns {string}
 */
const two = (value) => String(value).padStart(2, '0');

/**
 * `HH:MM` when `at` falls on the same local day as `now`, otherwise `YYYY-MM-DD HH:MM`, both in
 * the zone the `Date` getters read.
 * @param {Date} at
 * @param {Date} now
 * @returns {string}
 */
export function clockText(at, now) {
  const time = `${two(at.getHours())}:${two(at.getMinutes())}`;
  const sameDay =
    at.getFullYear() === now.getFullYear() &&
    at.getMonth() === now.getMonth() &&
    at.getDate() === now.getDate();
  return sameDay
    ? time
    : `${at.getFullYear()}-${two(at.getMonth() + 1)}-${two(at.getDate())} ${time}`;
}

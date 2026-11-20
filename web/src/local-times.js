// @ts-check
/**
 * The viewer's own time zone on a page: every `<time data-clock>` written in it, and its current
 * UTC offset recorded for the server.
 */

import { clockText } from './clock-text.js';
import { zoneCookie } from './zone-cookie.js';

/**
 * Sets the text of one `<time datetime>` to its wall-clock time in the viewer's zone; an element
 * whose `datetime` is not a time keeps the text the server wrote.
 * @param {Element} element
 * @param {Date} now
 * @returns {void}
 */
export function localizeTime(element, now) {
  const at = new Date(element.getAttribute('datetime') ?? '');
  if (!Number.isNaN(at.getTime())) {
    element.textContent = clockText(at, now);
  }
}

/**
 * Localizes every `<time data-clock>` inside `scope`, against the clock's current time.
 * @param {ParentNode} scope
 * @param {{ now(): Date }} clock
 * @returns {void}
 */
export function localizeTimes(scope, clock) {
  const now = clock.now();
  scope.querySelectorAll('time[data-clock]').forEach((element) => localizeTime(element, now));
}

/**
 * Records the viewer's UTC offset at the clock's current time in its cookie, when it is not
 * recorded already.
 * @param {{ cookie: string }} doc
 * @param {{ now(): Date }} clock
 * @returns {void}
 */
export function rememberZone(doc, clock) {
  const cookie = zoneCookie(doc.cookie, -clock.now().getTimezoneOffset());
  if (cookie !== null) {
    doc.cookie = cookie;
  }
}

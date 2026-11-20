// @ts-check
/**
 * Which changes a screen follows, and what that means for its own address. A screen refreshes on
 * the changes of the kinds its frame lists, or on every change when the frame lists none; a
 * screen whose frame lists an empty set follows no change at all. That is the frame of a page
 * answering a form: the browser shows the form's address, which only a POST answers, so the page
 * never asks for its own address again, not even when the stream says it may have missed events,
 * and its change notice's Reload keeps the screen's address the server rendered there. Such a page
 * follows the stream only to learn that the session ended. Pure, so a test reaches it without a
 * page.
 */

import { isChange } from './events.js';

/**
 * Whether an event asks the screen for a refresh: a change of one of its kinds, or any event when
 * the screen lists none.
 * @param {import('./events.js').StreamEvent} event
 * @param {ReadonlySet<string> | null} kinds
 * @returns {boolean}
 */
export function isRelevant(event, kinds) {
  return kinds === null || isChange(event, kinds);
}

/**
 * Whether the screen may ask for the address the page shows again, to refresh its regions: every
 * screen that follows a change does; one that follows none answers a form and never does.
 * @param {ReadonlySet<string> | null} kinds
 * @returns {boolean}
 */
export function asksItsAddress(kinds) {
  return kinds === null || kinds.size > 0;
}

/**
 * Where the change notice's Reload leads: the address the page shows, which an in-place navigation
 * may have changed since the server rendered the page; `null` on a page answering a form, whose
 * Reload keeps the address the server rendered there.
 * @param {ReadonlySet<string> | null} kinds
 * @param {string} shown the address the page shows
 * @returns {string | null}
 */
export function reloadAddress(kinds, shown) {
  return asksItsAddress(kinds) ? shown : null;
}

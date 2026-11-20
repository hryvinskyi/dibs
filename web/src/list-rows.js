// @ts-check
/**
 * The rows of a screen's list (`[data-row]` inside its `list` region) and the links they select
 * their object with (`a[data-inspect]`): which row is marked as current, and marking the one a page
 * shows.
 */

import { pathOf } from './page-addresses.js';
import { findRegion } from './regions.js';
import { selectedIndex } from './list-selection.js';

/**
 * The link a row selects its object with: the row itself when it is that link.
 * @param {Element} row
 * @returns {HTMLAnchorElement | null}
 */
export function rowLink(row) {
  if (row instanceof HTMLAnchorElement && row.hasAttribute('data-inspect')) {
    return row;
  }
  const link = row.querySelector('a[data-inspect]');
  return link instanceof HTMLAnchorElement ? link : null;
}

/**
 * The rows of the screen's list.
 * @param {Element} app
 * @returns {Element[]}
 */
export function listRows(app) {
  const list = findRegion(app, 'list');
  return list === null ? [] : Array.from(list.querySelectorAll('[data-row]'));
}

/**
 * The link of the row marked as current, if any.
 * @param {Element} app
 * @returns {HTMLAnchorElement | null}
 */
export function currentRowLink(app) {
  const row = listRows(app).find((candidate) => candidate.getAttribute('aria-current') === 'true');
  return row === undefined ? null : rowLink(row);
}

/**
 * Marks the row whose link points at the page `href` as current; the others lose the mark.
 * @param {Element} app
 * @param {string} href an absolute address
 * @returns {void}
 */
export function markSelected(app, href) {
  const rows = listRows(app);
  const chosen = selectedIndex(rows.map((row) => rowLink(row)?.href ?? ''), pathOf(href) ?? '');
  rows.forEach((row, index) => {
    if (index === chosen) {
      row.setAttribute('aria-current', 'true');
    } else {
      row.removeAttribute('aria-current');
    }
  });
}

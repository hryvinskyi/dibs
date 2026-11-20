// @ts-check
/**
 * Which link of a screen's list or inspector a page's path points at: the row that selects the
 * object shown, and the inspector's link back to the list. Pure, over absolute addresses.
 */

import { pathOf } from './page-addresses.js';

/**
 * Which of the rows' links (absolute addresses, `''` for a row without one) points at the page
 * shown, or -1.
 * @param {readonly string[]} hrefs
 * @param {string} pathname the path of the page shown
 * @returns {number}
 */
export function selectedIndex(hrefs, pathname) {
  return hrefs.findIndex((href) => pathOf(href) === pathname);
}

/**
 * Which of the inspector's links (absolute addresses) is its close link: the one that goes back to
 * the list the object shown belongs to, whose path is the page's path without its last segment.
 * @param {readonly string[]} hrefs
 * @param {string} pathname the path of the page shown
 * @returns {number}
 */
export function closeLinkIndex(hrefs, pathname) {
  const cut = pathname.lastIndexOf('/');
  if (cut <= 0) {
    return -1;
  }
  const list = pathname.slice(0, cut);
  return hrefs.findIndex((href) => pathOf(href) === list);
}

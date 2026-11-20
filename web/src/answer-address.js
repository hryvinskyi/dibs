// @ts-check
/**
 * Whether an answer fetched for one address may still be shown once the page shows another, so
 * that an answer for an address the page has left never replaces what the page shows now. Pure,
 * so a test reaches it without a page.
 */

/**
 * Whether an answer fetched for one address may still be shown at another: one origin, path and
 * query; the fragment is ignored.
 * @param {string} fetchedFor
 * @param {string} current
 * @returns {boolean}
 */
export function answerStillWanted(fetchedFor, current) {
  try {
    const was = new URL(fetchedFor);
    const now = new URL(current);
    return was.origin === now.origin && was.pathname === now.pathname && was.search === now.search;
  } catch {
    return false;
  }
}
